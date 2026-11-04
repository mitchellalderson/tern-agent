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
use tern_core::outbox::OutboxItem;
use tern_store::Store;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{ChatHost, Gateway};
use api::{Api, Buttons, CallbackQuery, MAX_MESSAGE, Message, split_message};

pub const SOURCE: &str = "telegram";
const EDIT_INTERVAL: Duration = Duration::from_secs(1);
const OFFSET_KEY: &str = "update_offset";

const HELP: &str = "Send me a message and I'll work on it.\n\
/agents — your agents; /agent <name> — talk to one (/agent tern: back)\n\
/new — new topic (I keep a summary of this one)\n\
/model [provider:model] — show or change the model\n\
/stop — stop the current task\n\
/status — what I'm doing";

const UNPAIRED: &str = "This bot is private. On the computer running tern, run `tern gateway pair` and send me the code it prints.";

#[derive(Default)]
struct Stream {
    chat: i64,
    /// A forum topic.
    thread: Option<i64>,
    /// `Research · ` when the chat talks to an agent other than the default.
    prefix: String,
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
        self.reply_in(chat, None, text).await;
    }

    async fn reply_in(&self, chat: i64, thread: Option<i64>, text: &str) {
        for chunk in split_message(text, MAX_MESSAGE) {
            if let Err(e) = self.api.send_message_in(chat, thread, &chunk, None).await {
                tracing::warn!(chat, error = %e, "telegram send failed");
            }
        }
    }

    fn paired(&self, user: i64) -> bool {
        self.store.is_telegram_user_paired(user).unwrap_or(false)
    }

    /// What a reply in `chat_ref` starts with: the agent's name, unless
    /// it's the default agent.
    fn prefix(&self, chat_ref: &str) -> String {
        let id = crate::chat_agents::get(&self.store, SOURCE, chat_ref);
        match self.store.agent(&id) {
            Ok(Some(a)) if !a.is_default() => format!("{} · ", a.name),
            _ => String::new(),
        }
    }

    /// `/agents`.
    fn agents_text(&self, chat_ref: &str) -> String {
        let current = crate::chat_agents::get(&self.store, SOURCE, chat_ref);
        let agents = self.store.agents(false).unwrap_or_default();
        if agents.len() < 2 {
            return "You have one agent, tern. Create more in the tern app or with `tern agents new`."
                .into();
        }
        let lines: Vec<String> = agents
            .iter()
            .map(|a| {
                let here = if a.id == current { "▸" } else { "·" };
                format!("{here} {} (/agent {})", a.name, a.slug)
            })
            .collect();
        format!("Your agents:\n{}", lines.join("\n"))
    }

    /// `/agent <name>`: this chat (or topic) talks to that agent from now
    /// on. In a forum group, a new topic per agent instead, when the bot
    /// may create one.
    async fn switch_agent(&self, msg: &Message, chat_ref: &str, name: &str) {
        let (chat, thread) = (msg.chat.id, msg.message_thread_id);
        if name.is_empty() {
            let text = format!(
                "{}\n{}",
                self.prefix_or_tern(chat_ref),
                self.agents_text(chat_ref)
            );
            return self.reply_in(chat, thread, &text).await;
        }
        let name = name.trim_start_matches('@');
        let agent = self
            .store
            .agents(false)
            .unwrap_or_default()
            .into_iter()
            .find(|a| a.slug == name || a.id == name || a.name.eq_ignore_ascii_case(name));
        let Some(agent) = agent else {
            let text = format!("No agent {name:?}.\n{}", self.agents_text(chat_ref));
            return self.reply_in(chat, thread, &text).await;
        };
        if msg.chat.is_forum && !agent.is_default() {
            match self.api.create_forum_topic(chat, &agent.name).await {
                Ok(topic) => {
                    let topic_ref = format!("{chat}/{topic}");
                    if let Err(e) =
                        crate::chat_agents::set(&self.store, SOURCE, &topic_ref, &agent.id)
                    {
                        tracing::warn!(error = %e, "could not save the topic's agent");
                    }
                    self.reply_in(
                        chat,
                        Some(topic),
                        &format!(
                            "{} · This topic is my chat. Write here to talk to me.",
                            agent.name
                        ),
                    )
                    .await;
                    return;
                }
                Err(e) => {
                    tracing::info!(error = %e, "no forum topic for the agent; switching this one");
                }
            }
        }
        if let Err(e) = crate::chat_agents::set(&self.store, SOURCE, chat_ref, &agent.id) {
            return self.reply_in(chat, thread, &format!("Error: {e}")).await;
        }
        let text = if agent.is_default() {
            format!("Back to {}.", agent.name)
        } else {
            format!(
                "{} · You're talking to {} now. /agent tern goes back.",
                agent.name, agent.name
            )
        };
        self.reply_in(chat, thread, &text).await;
    }

    fn prefix_or_tern(&self, chat_ref: &str) -> String {
        let id = crate::chat_agents::get(&self.store, SOURCE, chat_ref);
        let name = self
            .store
            .agent(&id)
            .ok()
            .flatten()
            .map_or_else(|| "tern".to_owned(), |a| a.name);
        format!("This chat talks to {name}.")
    }

    async fn on_message(&self, host: &Arc<dyn ChatHost>, msg: Message) {
        let chat = msg.chat.id;
        // In a forum group each topic is its own chat (agent profiles §6.4).
        let thread = msg.message_thread_id.filter(|_| msg.chat.is_forum);
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

        let chat_ref = match thread {
            Some(t) => format!("{chat}/{t}"),
            None => chat.to_string(),
        };
        let (cmd, arg) = match text.split_once(char::is_whitespace) {
            Some((c, a)) => (c, a.trim()),
            None => (text, ""),
        };
        // Commands may arrive as `/stop@my_bot` in groups.
        let cmd = cmd.split('@').next().unwrap_or(cmd);
        match cmd {
            "/start" | "/help" => self.reply_in(chat, thread, HELP).await,
            "/agents" => {
                let text = self.agents_text(&chat_ref);
                self.reply_in(chat, thread, &text).await;
            }
            "/agent" => self.switch_agent(&msg, &chat_ref, arg).await,
            "/new" => match host.new_session(SOURCE, &chat_ref).await {
                Ok(()) => {
                    let text = format!(
                        "{}New topic. I kept a summary of the last one.",
                        self.prefix(&chat_ref)
                    );
                    self.reply_in(chat, thread, &text).await
                }
                Err(e) => self.reply_in(chat, thread, &format!("Error: {e}")).await,
            },
            "/model" => {
                let set = (!arg.is_empty()).then_some(arg);
                match host.model(SOURCE, &chat_ref, set).await {
                    Ok(m) if set.is_some() => {
                        self.reply_in(chat, thread, &format!("Model set to {m}."))
                            .await
                    }
                    Ok(m) => self.reply_in(chat, thread, &format!("Model: {m}")).await,
                    Err(e) => self.reply_in(chat, thread, &format!("Error: {e}")).await,
                }
            }
            "/stop" => {
                let msg = if host.stop(SOURCE, &chat_ref).await {
                    "Stopping."
                } else {
                    "Nothing is running."
                };
                self.reply_in(chat, thread, msg).await;
            }
            "/status" => {
                let text = format!(
                    "{}{}",
                    self.prefix(&chat_ref),
                    host.status(SOURCE, &chat_ref).await
                );
                self.reply_in(chat, thread, &text).await
            }
            _ => {
                self.api.typing(chat).await;
                match host.send(SOURCE, &chat_ref, text).await {
                    // A queued message returns the run already streaming here.
                    Ok(run_id) => {
                        let prefix = self.prefix(&chat_ref);
                        self.streams
                            .lock()
                            .await
                            .entry(run_id)
                            .or_insert_with(|| Stream {
                                chat,
                                thread,
                                prefix,
                                ..Stream::default()
                            });
                    }
                    Err(e) => self.reply_in(chat, thread, &format!("Error: {e}")).await,
                }
            }
        }
    }

    async fn on_callback(&self, host: &Arc<dyn ChatHost>, cb: CallbackQuery) {
        if !self.paired(cb.from.id) {
            let _ = self.api.answer_callback(&cb.id, "Not paired").await;
            return;
        }
        let data = cb.data.clone().unwrap_or_default();
        let mut parts = data.splitn(3, ':');
        let (kind, id, choice) = (parts.next(), parts.next(), parts.next());
        if let (Some("ob"), Some(id), Some(choice)) = (kind, id, choice) {
            return self.on_outbox_callback(host, cb, id, choice).await;
        }
        let (Some("ap"), Some(id), Some(choice)) = (kind, id, choice) else {
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
            Decision::Deny | Decision::Unanswered => "Denied",
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
                    self.put(
                        s.chat,
                        s.thread,
                        s.message_id,
                        &format!("{}{text}", s.prefix),
                    )
                    .await;
                }
            }
            _ => {}
        }
    }

    /// Show `text` in `message_id` (or a new message), splitting if needed.
    async fn put(&self, chat: i64, thread: Option<i64>, message_id: Option<i64>, text: &str) {
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
            if let Err(e) = self.api.send_message_in(chat, thread, chunk, None).await {
                tracing::warn!(chat, error = %e, "telegram send failed");
            }
        }
    }

    /// Push pending streamed text, at most once per `EDIT_INTERVAL`.
    async fn flush(&self) {
        let mut streams = self.streams.lock().await;
        for s in streams.values_mut().filter(|s| s.dirty) {
            s.dirty = false;
            let shown = live_view(&format!("{}{}", s.prefix, s.text));
            match s.message_id {
                None => match self
                    .api
                    .send_message_in(s.chat, s.thread, &shown, None)
                    .await
                {
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

/// A chat ref: `<chat>` or, for a forum topic, `<chat>/<thread>`.
fn place(target: &str) -> Option<(i64, Option<i64>)> {
    match target.split_once('/') {
        Some((c, t)) => Some((c.parse().ok()?, Some(t.parse().ok()?))),
        None => Some((target.parse().ok()?, None)),
    }
}

/// What a still-streaming reply shows: the tail, if it's over the limit.
/// "Allow destructive · shell git push?", prefixed with a sub-agent's label when one asks,
/// and followed by why a tainted run asks.
fn approval_prompt(req: &ApprovalRequest) -> String {
    let ask = match &req.agent_label {
        Some(l) => format!("[{l}] Allow {}?", req.headline()),
        None => format!("Allow {}?", req.headline()),
    };
    match req.taint_note() {
        Some(note) => format!("{ask}\n⚠ {note}"),
        None => ask,
    }
}

impl TelegramGateway {
    async fn on_outbox_callback(
        &self,
        host: &Arc<dyn ChatHost>,
        cb: CallbackQuery,
        id: &str,
        choice: &str,
    ) {
        let outcome = match id.parse::<i64>() {
            Ok(id) => host.decide_outbox(id, choice == "approve").await,
            Err(_) => Err("unknown item".into()),
        };
        let status = match &outcome {
            Ok(s) => s.clone(),
            Err(e) => e.clone(),
        };
        let _ = self.api.answer_callback(&cb.id, &status).await;
        if let Some(m) = cb.message {
            let original = m.text.unwrap_or_default();
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
}

/// "1 action waiting" for the user, with what it is and why it waits.
fn outbox_prompt(item: &OutboxItem) -> String {
    let mut text = format!(
        "📬 Waiting for you (outbox #{}): {}\nNobody was here to approve it, so tern carried on without it.",
        item.id,
        item.headline()
    );
    if let Some(taint) = &item.taint {
        text.push_str(&format!("\n⚠ {}", tern_core::approval::taint_note(taint)));
    }
    if item.local_only {
        text.push_str("\nApprove it in the tern app or TUI on the computer running tern.");
    }
    text
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
                            self.on_callback(&host, cb).await;
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
        let (chat, thread) =
            place(target).ok_or_else(|| format!("bad telegram chat id {target:?}"))?;
        let stream = match run_id {
            Some(r) => self.streams.lock().await.remove(r),
            None => None,
        };
        let prefix = match &stream {
            Some(s) => s.prefix.clone(),
            None => self.prefix(target),
        };
        self.put(
            chat,
            thread,
            stream.and_then(|s| s.message_id),
            &format!("{prefix}{text}"),
        )
        .await;
        Ok(())
    }

    async fn ask_approval(&self, target: &str, req: &ApprovalRequest) -> Decision {
        let Some((chat, thread)) = place(target) else {
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
            .send_message_in(chat, thread, &approval_prompt(req), Some(&buttons))
            .await
        {
            tracing::warn!(error = %e, "could not send approval request");
            self.approvals.lock().await.remove(&id);
            return Decision::Unanswered;
        }
        // If the agent stops waiting (timeout, cancel), this future is
        // dropped; the stale entry then answers "Expired".
        rx.await.unwrap_or(Decision::Deny)
    }

    async fn outbox_notice(&self, target: &str, item: &OutboxItem) {
        let Ok(chat) = target.parse::<i64>() else {
            return;
        };
        let buttons: Buttons = vec![
            ("Approve".into(), format!("ob:{}:approve", item.id)),
            ("Reject".into(), format!("ob:{}:reject", item.id)),
        ];
        let buttons = (!item.local_only).then_some(&buttons);
        if let Err(e) = self
            .api
            .send_message(chat, &outbox_prompt(item), buttons)
            .await
        {
            tracing::warn!(error = %e, "could not send outbox notice");
        }
    }

    async fn secret_request_notice(&self, target: &str, req: &SecretRequest) -> Option<String> {
        let (chat, thread) = place(target)?;
        match self
            .api
            .send_message_in(chat, thread, &secret_notice(req), None)
            .await
        {
            Ok(id) => Some(id.to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "could not send secret request notice");
                None
            }
        }
    }

    async fn secret_request_done(&self, target: &str, handle: &str, text: &str) {
        let (Some((chat, _)), Ok(id)) = (place(target), handle.parse::<i64>()) else {
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
