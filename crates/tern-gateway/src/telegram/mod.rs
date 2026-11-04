//! Telegram gateway: long-polls the Bot API, pairs users with one-time
//! codes, streams replies by editing one message (at most once a second),
//! and asks for approvals with inline buttons.

pub mod api;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tern_core::approval::{ApprovalRequest, Decision, SecretRequest};
use tern_core::events::AgentEvent;
use tern_store::Store;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{ChatHost, Gateway};
use api::{Api, Buttons, CallbackQuery, MAX_MESSAGE, Message, split_message};

pub const SOURCE: &str = "telegram";
const EDIT_INTERVAL: Duration = Duration::from_secs(1);
const OFFSET_KEY: &str = "update_offset";

const HELP: &str = "Send me a message and I'll work on it.\n\
/new — new topic (I keep a summary of this one)\n\
/model [provider:model] — show or change the model\n\
/stop — stop the current task\n\
/status — what I'm doing";

const UNPAIRED: &str = "This bot is private. On the computer running tern, run `tern gateway pair` and send me the code it prints.";

#[derive(Default)]
struct Stream {
    chat: i64,
    message_id: Option<i64>,
    text: String,
    dirty: bool,
}

pub struct TelegramGateway {
    api: Api,
    store: Store,
    poll_timeout_secs: u64,
    /// Live replies by run id.
    streams: Mutex<HashMap<String, Stream>>,
    /// Pending approvals by callback id, with the chat they were sent to.
    approvals: Mutex<HashMap<String, (i64, oneshot::Sender<Decision>)>>,
}

impl TelegramGateway {
    pub fn new(api_url: &str, token: &str, store: Store) -> Result<Arc<Self>, reqwest::Error> {
        Ok(Arc::new(Self {
            api: Api::new(api_url, token)?,
            store,
            poll_timeout_secs: 25,
            streams: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
        }))
    }

    async fn reply(&self, chat: i64, text: &str) {
        for chunk in split_message(text, MAX_MESSAGE) {
            if let Err(e) = self.api.send_message(chat, &chunk, None).await {
                tracing::warn!(chat, error = %e, "telegram send failed");
            }
        }
    }

    fn paired(&self, user: i64) -> bool {
        self.store.is_telegram_user_paired(user).unwrap_or(false)
    }

    async fn on_message(&self, host: &Arc<dyn ChatHost>, msg: Message) {
        let chat = msg.chat.id;
        let Some(text) = msg.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
            return;
        };
        let Some(user) = msg.from.as_ref().map(|u| u.id) else {
            return;
        };
        if !self.paired(user) {
            let code = text.strip_prefix("/start").map(str::trim).unwrap_or(text);
            if !code.is_empty() && self.store.consume_pairing_code(code).unwrap_or(false) {
                let _ = self.store.pair_telegram_user(user);
                tracing::info!(user, "telegram user paired");
                self.reply(chat, &format!("Paired. {HELP}")).await;
            } else {
                self.reply(chat, UNPAIRED).await;
            }
            return;
        }

        let chat_ref = chat.to_string();
        let (cmd, arg) = match text.split_once(char::is_whitespace) {
            Some((c, a)) => (c, a.trim()),
            None => (text, ""),
        };
        // Commands may arrive as `/stop@my_bot` in groups.
        let cmd = cmd.split('@').next().unwrap_or(cmd);
        match cmd {
            "/start" | "/help" => self.reply(chat, HELP).await,
            "/new" => match host.new_session(SOURCE, &chat_ref).await {
                Ok(()) => {
                    self.reply(chat, "New topic. I kept a summary of the last one.")
                        .await
                }
                Err(e) => self.reply(chat, &format!("Error: {e}")).await,
            },
            "/model" => {
                let set = (!arg.is_empty()).then_some(arg);
                match host.model(SOURCE, &chat_ref, set).await {
                    Ok(m) if set.is_some() => self.reply(chat, &format!("Model set to {m}.")).await,
                    Ok(m) => self.reply(chat, &format!("Model: {m}")).await,
                    Err(e) => self.reply(chat, &format!("Error: {e}")).await,
                }
            }
            "/stop" => {
                let msg = if host.stop(SOURCE, &chat_ref).await {
                    "Stopping."
                } else {
                    "Nothing is running."
                };
                self.reply(chat, msg).await;
            }
            "/status" => {
                self.reply(chat, &host.status(SOURCE, &chat_ref).await)
                    .await
            }
            _ => {
                self.api.typing(chat).await;
                match host.send(SOURCE, &chat_ref, text).await {
                    // A queued message returns the run already streaming here.
                    Ok(run_id) => {
                        self.streams
                            .lock()
                            .await
                            .entry(run_id)
                            .or_insert_with(|| Stream {
                                chat,
                                ..Stream::default()
                            });
                    }
                    Err(e) => self.reply(chat, &format!("Error: {e}")).await,
                }
            }
        }
    }

    async fn on_callback(&self, cb: CallbackQuery) {
        if !self.paired(cb.from.id) {
            let _ = self.api.answer_callback(&cb.id, "Not paired").await;
            return;
        }
        let data = cb.data.unwrap_or_default();
        let mut parts = data.splitn(3, ':');
        let (Some("ap"), Some(id), Some(choice)) = (parts.next(), parts.next(), parts.next())
        else {
            let _ = self.api.answer_callback(&cb.id, "").await;
            return;
        };
        let decision = match choice {
            "once" => Decision::AllowOnce,
            "always" => Decision::AlwaysAllow,
            _ => Decision::Deny,
        };
        let label = match decision {
            Decision::AllowOnce => "Allowed",
            Decision::AlwaysAllow => "Always allowed",
            Decision::Deny => "Denied",
        };
        // Only a tap in the chat the request was sent to counts.
        let chat = cb.message.as_ref().map(|m| m.chat.id);
        let pending = {
            let mut approvals = self.approvals.lock().await;
            match approvals.get(id) {
                Some((target, _)) if Some(*target) == chat => approvals.remove(id),
                _ => None,
            }
        };
        let answered = pending.is_some_and(|(_, tx)| tx.send(decision).is_ok());
        let _ = self
            .api
            .answer_callback(&cb.id, if answered { label } else { "Expired" })
            .await;
        if let Some(m) = cb.message {
            let original = m.text.unwrap_or_default();
            let status = if answered { label } else { "Expired" };
            let _ = self
                .api
                .edit_message(
                    m.chat.id,
                    m.message_id,
                    &format!("{original}\n→ {status}"),
                    None,
                )
                .await;
        }
    }

    /// Fold agent events into live replies.
    async fn on_event(&self, ev: AgentEvent) {
        let mut streams = self.streams.lock().await;
        match ev {
            AgentEvent::TextDelta { run_id, text } => {
                if let Some(s) = streams.get_mut(&run_id) {
                    s.text.push_str(&text);
                    s.dirty = true;
                }
            }
            AgentEvent::ToolStarted {
                run_id, summary, ..
            } => {
                if let Some(s) = streams.get_mut(&run_id) {
                    if !s.text.is_empty() && !s.text.ends_with('\n') {
                        s.text.push('\n');
                    }
                    s.text.push_str(&format!("▸ {summary}\n"));
                    s.dirty = true;
                }
            }
            AgentEvent::RunFinished {
                run_id,
                status,
                error,
                ..
            } if status != "completed" => {
                // Completed runs are finalized by `deliver`.
                if let Some(s) = streams.remove(&run_id) {
                    let note = match error {
                        Some(e) => format!("Run failed: {e}"),
                        None => "(stopped)".to_owned(),
                    };
                    let text = if s.text.trim().is_empty() {
                        note
                    } else {
                        format!("{}\n\n{note}", s.text.trim_end())
                    };
                    drop(streams);
                    self.put(s.chat, s.message_id, &text).await;
                }
            }
            _ => {}
        }
    }

    /// Show `text` in `message_id` (or a new message), splitting if needed.
    async fn put(&self, chat: i64, message_id: Option<i64>, text: &str) {
        let chunks = split_message(
            if text.trim().is_empty() {
                "(no response)"
            } else {
                text
            },
            MAX_MESSAGE,
        );
        let mut rest = chunks.iter();
        if let Some(id) = message_id
            && let Some(first) = rest.next()
            && let Err(e) = self.api.edit_message(chat, id, first, None).await
        {
            tracing::warn!(chat, error = %e, "telegram edit failed");
        }
        for chunk in rest {
            if let Err(e) = self.api.send_message(chat, chunk, None).await {
                tracing::warn!(chat, error = %e, "telegram send failed");
            }
        }
    }

    /// Push pending streamed text, at most once per `EDIT_INTERVAL`.
    async fn flush(&self) {
        let mut streams = self.streams.lock().await;
        for s in streams.values_mut().filter(|s| s.dirty) {
            s.dirty = false;
            let shown = live_view(&s.text);
            match s.message_id {
                None => match self.api.send_message(s.chat, &shown, None).await {
                    Ok(id) => s.message_id = Some(id),
                    Err(e) => tracing::warn!(error = %e, "telegram send failed"),
                },
                Some(id) => {
                    if let Err(e) = self.api.edit_message(s.chat, id, &shown, None).await {
                        tracing::warn!(error = %e, "telegram edit failed");
                    }
                }
            }
        }
    }
}

/// What a still-streaming reply shows: the tail, if it's over the limit.
/// "Allow destructive · shell git push?", prefixed with a sub-agent's label when one asks.
fn approval_prompt(req: &ApprovalRequest) -> String {
    match &req.agent_label {
        Some(l) => format!("[{l}] Allow {}?", req.headline()),
        None => format!("Allow {}?", req.headline()),
    }
}

fn live_view(text: &str) -> String {
    let n = text.chars().count();
    if n <= MAX_MESSAGE - 2 {
        return format!("{text} ▌");
    }
    let tail: String = text.chars().skip(n - (MAX_MESSAGE - 4)).collect();
    format!("…{tail} ▌")
}

#[async_trait]
impl Gateway for TelegramGateway {
    fn name(&self) -> &str {
        SOURCE
    }

    async fn run(self: Arc<Self>, host: Arc<dyn ChatHost>, cancel: CancellationToken) {
        let events = {
            let (me, mut rx, cancel) = (self.clone(), host.events(), cancel.clone());
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        ev = rx.recv() => match ev {
                            Ok(ev) => me.on_event(ev).await,
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => return,
                        },
                        () = cancel.cancelled() => return,
                    }
                }
            })
        };
        let flusher = {
            let (me, cancel) = (self.clone(), cancel.clone());
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(EDIT_INTERVAL);
                loop {
                    tokio::select! {
                        _ = tick.tick() => me.flush().await,
                        () = cancel.cancelled() => return,
                    }
                }
            })
        };

        // Persisted so a restart never handles the same message twice.
        let mut offset: i64 = self
            .store
            .kv_get(SOURCE, OFFSET_KEY)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut backoff = Duration::from_secs(1);
        loop {
            let updates = tokio::select! {
                u = self.api.get_updates(offset, self.poll_timeout_secs) => u,
                () = cancel.cancelled() => break,
            };
            match updates {
                Ok(updates) => {
                    backoff = Duration::from_secs(1);
                    if let Some(last) = updates.iter().map(|u| u.update_id).max() {
                        offset = offset.max(last + 1);
                        if let Err(e) = self.store.kv_set(SOURCE, OFFSET_KEY, &offset.to_string()) {
                            tracing::warn!(error = %e, "could not save telegram offset");
                        }
                    }
                    for u in updates {
                        if let Some(m) = u.message {
                            // Handle each message on its own task so a slow
                            // command can't block approvals arriving meanwhile.
                            let (me, host) = (self.clone(), host.clone());
                            tokio::spawn(async move { me.on_message(&host, m).await });
                        } else if let Some(cb) = u.callback_query {
                            self.on_callback(cb).await;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, retry_in = ?backoff, "telegram polling failed");
                    tokio::select! {
                        () = tokio::time::sleep(backoff) => {}
                        () = cancel.cancelled() => break,
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
        events.abort();
        flusher.abort();
    }

    async fn deliver(&self, target: &str, run_id: Option<&str>, text: &str) -> Result<(), String> {
        let chat: i64 = target
            .parse()
            .map_err(|_| format!("bad telegram chat id {target:?}"))?;
        let stream = match run_id {
            Some(r) => self.streams.lock().await.remove(r),
            None => None,
        };
        self.put(chat, stream.and_then(|s| s.message_id), text)
            .await;
        Ok(())
    }

    async fn ask_approval(&self, target: &str, req: &ApprovalRequest) -> Decision {
        let Ok(chat) = target.parse::<i64>() else {
            return Decision::Deny;
        };
        // Callback data is limited to 64 bytes.
        let id: String = req
            .id
            .chars()
            .rev()
            .take(16)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().await.insert(id.clone(), (chat, tx));
        let mut buttons: Buttons = vec![("Allow once".into(), format!("ap:{id}:once"))];
        if let Some(pattern) = &req.pattern {
            buttons.push((format!("Always ({pattern})"), format!("ap:{id}:always")));
        }
        buttons.push(("Deny".into(), format!("ap:{id}:deny")));
        if let Err(e) = self
            .api
            .send_message(chat, &approval_prompt(req), Some(&buttons))
            .await
        {
            tracing::warn!(error = %e, "could not send approval request");
            self.approvals.lock().await.remove(&id);
            return Decision::Deny;
        }
        // If the agent stops waiting (timeout, cancel), this future is
        // dropped; the stale entry then answers "Expired".
        rx.await.unwrap_or(Decision::Deny)
    }

    async fn secret_request_notice(&self, target: &str, req: &SecretRequest) -> Option<String> {
        let chat = target.parse::<i64>().ok()?;
        match self.api.send_message(chat, &secret_notice(req), None).await {
            Ok(id) => Some(id.to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "could not send secret request notice");
                None
            }
        }
    }

    async fn secret_request_done(&self, target: &str, handle: &str, text: &str) {
        let (Ok(chat), Ok(id)) = (target.parse::<i64>(), handle.parse::<i64>()) else {
            return;
        };
        if let Err(e) = self.api.edit_message(chat, id, text, None).await {
            tracing::warn!(error = %e, "could not update secret request notice");
        }
    }
}

/// Points the user at an app that can take the value: never this chat.
fn secret_notice(req: &SecretRequest) -> String {
    let names: Vec<&str> = req.asks.iter().map(|a| a.name.as_str()).collect();
    let who = match &req.agent_label {
        Some(l) => format!("Sub-agent {l}"),
        None => "tern".to_owned(),
    };
    let mut text = format!(
        "🔑 {who} needs {} to continue.\nThe agent says: \"{}\"\n\n",
        names.join(", "),
        req.reason
    );
    text.push_str(
        "Enter it in the tern app or the TUI, not here: chat messages keep it in the history. \
         It's waiting in the conversation, or set it with /secrets in the TUI.",
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_view_shows_the_tail_within_limits() {
        assert_eq!(live_view("hi"), "hi ▌");
        let long = "a".repeat(5000) + "END";
        let v = live_view(&long);
        assert!(v.chars().count() <= MAX_MESSAGE);
        assert!(v.starts_with('…') && v.contains("END"));
    }
}
