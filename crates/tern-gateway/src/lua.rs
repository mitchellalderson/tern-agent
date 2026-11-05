//! Gateway connectors (tooling spec §6.4): a Luau connector with a
//! `gateway` table, run as a chat transport beside Telegram. The daemon
//! treats it like any [`Gateway`]: routine results (after quiet hours,
//! initiative and digests), approvals, outbox cards and secret notices go
//! to `<connector>:<chat>`, and messages it polls go to the [`ChatHost`].
//!
//! Pairing is shared with Telegram (`tern gateway pair`): the first
//! message from a sender must be the code. A platform that names no sender
//! (ntfy) pairs the chat instead. Unpaired senders get one fixed reply, are
//! ignored, or, with `strangers = "taint"`, start a tainted run of their own.
//!
//! Without `buttons`, approvals and outbox cards ask for a reply: a number
//! for an approval, `approve 12` / `reject 12` for an outbox item.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_core::approval::{ApprovalRequest, Decision, SecretRequest};
use tern_core::config::{GatewayConnectorConfig, Strangers};
use tern_core::outbox::OutboxItem;
use tern_lua::gateway::{self as lg, Button, GatewayInfo, Inbound};
use tern_store::Store;
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

use crate::telegram::{approval_prompt, outbox_prompt, secret_notice};
use crate::{ChatHost, Gateway};

/// kv namespace of paired senders: `<connector>:<identity>` → paired at.
pub const PAIRED_NS: &str = "gateway:paired";
const CURSOR_NS: &str = "gateway:cursor";
const MAX_BACKOFF: Duration = Duration::from_secs(300);

const HELP: &str = "Send me a message and I'll work on it.\n\
/new — new topic (I keep a summary of this one)\n\
/stop — stop the current task\n\
/status — what I'm doing\n\
/outbox — actions waiting for your approval";

const UNPAIRED: &str = "This tern is private. On the computer running tern, run `tern gateway pair` and send me the code it prints.";

/// Whether `identity` has paired on gateway `name`.
pub fn is_paired(store: &Store, name: &str, identity: &str) -> bool {
    store
        .kv_get(PAIRED_NS, &format!("{name}:{identity}"))
        .ok()
        .flatten()
        .is_some()
}

pub fn pair(store: &Store, name: &str, identity: &str) -> tern_store::Result<()> {
    store.kv_set(
        PAIRED_NS,
        &format!("{name}:{identity}"),
        &tern_store::now_ms().to_string(),
    )
}

/// How many senders have paired on gateway `name`.
pub fn paired_count(store: &Store, name: &str) -> usize {
    let prefix = format!("{name}:");
    store
        .kv_list(PAIRED_NS)
        .map(|v| v.iter().filter(|(k, _)| k.starts_with(&prefix)).count())
        .unwrap_or(0)
}

/// An approval waiting for a tap or a numbered reply.
struct Pending {
    /// The chat ref it was sent to; only an answer there counts.
    chat: String,
    choices: Vec<(String, Decision)>,
    tx: oneshot::Sender<Decision>,
    /// Newer approvals win a bare number when several wait in one chat.
    seq: u64,
}

pub struct LuaGateway {
    name: String,
    host: tern_lua::Host,
    dir: PathBuf,
    store: Store,
    cfg: GatewayConnectorConfig,
    settings: Value,
    info: GatewayInfo,
    approvals: Mutex<HashMap<String, Pending>>,
    seq: AtomicU64,
}

impl LuaGateway {
    /// Gateway `name`: active connector `<dir>/<name>.luau`, which must have
    /// a `gateway` table with `send`.
    pub async fn open(
        name: &str,
        host: tern_lua::Host,
        dir: PathBuf,
        cfg: GatewayConnectorConfig,
    ) -> Result<Arc<Self>, String> {
        let info = lg::info(&host, &dir, name).await?;
        let settings = serde_json::to_value(&cfg.settings).map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            name: name.to_owned(),
            store: host.store.clone(),
            host,
            dir,
            cfg,
            settings,
            info,
            approvals: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
        }))
    }

    pub fn info(&self) -> &GatewayInfo {
        &self.info
    }

    /// `(chat, thread)` from a chat ref; only gateways with threads split.
    fn place<'a>(&self, target: &'a str) -> (&'a str, Option<&'a str>) {
        match target.rsplit_once('/') {
            Some((c, t)) if self.info.threads => (c, Some(t)),
            _ => (target, None),
        }
    }

    fn opts(&self, target: &str, kind: &str, run_id: Option<&str>) -> Value {
        let (_, thread) = self.place(target);
        json!({ "thread": thread, "kind": kind, "run_id": run_id, "settings": self.settings })
    }

    async fn send(
        &self,
        target: &str,
        text: &str,
        kind: &str,
        run_id: Option<&str>,
    ) -> Result<(), String> {
        let (chat, _) = self.place(target);
        lg::send(
            &self.host,
            &self.dir,
            &self.name,
            chat,
            text,
            &self.opts(target, kind, run_id),
        )
        .await
    }

    async fn reply(&self, target: &str, text: &str) {
        if let Err(e) = self.send(target, text, "reply", None).await {
            tracing::warn!(gateway = %self.name, error = %e, "gateway send failed");
        }
    }

    /// Send with buttons, or as text with `fallback` (how to answer by
    /// reply) when the gateway has none.
    async fn ask(
        &self,
        target: &str,
        text: &str,
        buttons: &[Button],
        fallback: &str,
        kind: &str,
    ) -> Result<(), String> {
        let (chat, _) = self.place(target);
        if self.info.buttons {
            let shown = lg::buttons(
                &self.host,
                &self.dir,
                &self.name,
                chat,
                text,
                buttons,
                &self.opts(target, kind, None),
            )
            .await?;
            if shown {
                return Ok(());
            }
        }
        self.send(target, &format!("{text}\n{fallback}"), kind, None)
            .await
    }

    /// `Research · ` when the chat talks to an agent other than the default.
    fn prefix(&self, chat_ref: &str) -> String {
        let id = crate::chat_agents::get(&self.store, &self.name, chat_ref);
        match self.store.agent(&id) {
            Ok(Some(a)) if !a.is_default() => format!("{} · ", a.name),
            _ => String::new(),
        }
    }

    fn agent_name(&self, item: &OutboxItem, chat_ref: &str) -> String {
        let id = item
            .agent_id
            .clone()
            .unwrap_or_else(|| crate::chat_agents::get(&self.store, &self.name, chat_ref));
        self.store
            .agent(&id)
            .ok()
            .flatten()
            .map_or_else(|| "tern".to_owned(), |a| a.name)
    }

    async fn outbox_card(&self, target: &str, item: &OutboxItem) {
        let text = outbox_prompt(item, &self.agent_name(item, target));
        let result = if item.local_only {
            self.send(target, &text, "outbox", None).await
        } else {
            let buttons = [
                Button {
                    label: "Approve".into(),
                    data: format!("ob:{}:approve", item.id),
                },
                Button {
                    label: "Reject".into(),
                    data: format!("ob:{}:reject", item.id),
                },
            ];
            let fallback = format!("Reply \"approve {0}\" or \"reject {0}\".", item.id);
            self.ask(target, &text, &buttons, &fallback, "outbox").await
        };
        if let Err(e) = result {
            tracing::warn!(gateway = %self.name, error = %e, "could not send outbox notice");
        }
    }

    /// Resolve approval `id` with choice `key`, if it was asked in `chat`.
    async fn answer(&self, chat: &str, id: &str, key: &str) -> Option<Decision> {
        let mut approvals = self.approvals.lock().await;
        let p = approvals.get(id).filter(|p| p.chat == chat)?;
        let decision = p.choices.iter().find(|(k, _)| k == key)?.1;
        let p = approvals.remove(id)?;
        p.tx.send(decision).is_ok().then_some(decision)
    }

    /// A bare number answers the newest approval waiting in `chat`.
    async fn answer_latest(&self, chat: &str, key: &str) -> Option<Decision> {
        let id = {
            let approvals = self.approvals.lock().await;
            approvals
                .iter()
                .filter(|(_, p)| p.chat == chat)
                .max_by_key(|(_, p)| p.seq)
                .map(|(id, _)| id.clone())?
        };
        self.answer(chat, &id, key).await
    }

    async fn decide(&self, host: &Arc<dyn ChatHost>, chat: &str, id: &str, approve: bool) {
        let status = match id.parse::<i64>() {
            Ok(id) => match host.decide_outbox(id, approve).await {
                Ok(s) => format!("Outbox #{id}: {s}"),
                Err(e) => format!("Outbox #{id}: {e}"),
            },
            Err(_) => format!("No outbox item {id:?}."),
        };
        self.reply(chat, &status).await;
    }

    /// A button tap from a paired sender.
    async fn on_action(&self, host: &Arc<dyn ChatHost>, chat: &str, data: &str) {
        let mut parts = data.splitn(3, ':');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("ap"), Some(id), Some(key)) => {
                let text = match self.answer(chat, id, key).await {
                    Some(d) => format!("→ {}", label(d)),
                    None => "→ Expired".to_owned(),
                };
                self.reply(chat, &text).await;
            }
            (Some("ob"), Some(id), Some(choice)) => {
                self.decide(host, chat, id, choice == "approve").await;
            }
            _ => tracing::debug!(gateway = %self.name, data, "unknown button"),
        }
    }

    async fn on_message(&self, host: &Arc<dyn ChatHost>, m: Inbound) {
        let chat = m.chat_ref();
        let text = m.text.trim();
        if !is_paired(&self.store, &self.name, m.identity()) {
            let code = text.strip_prefix("/start").map_or(text, str::trim);
            if !code.is_empty() && self.store.consume_pairing_code(code).unwrap_or(false) {
                if let Err(e) = pair(&self.store, &self.name, m.identity()) {
                    tracing::warn!(gateway = %self.name, error = %e, "could not save pairing");
                }
                tracing::info!(gateway = %self.name, "gateway sender paired");
                self.reply(&chat, &format!("Paired. {HELP}")).await;
                return;
            }
            // A stranger's button tap never counts.
            if m.action.is_some() || text.is_empty() {
                return;
            }
            match self.cfg.strangers {
                Strangers::Reply => self.reply(&chat, UNPAIRED).await,
                Strangers::Ignore => {}
                Strangers::Taint => {
                    let who = m
                        .from_name
                        .as_deref()
                        .or(m.from.as_deref())
                        .unwrap_or("someone");
                    let taint = format!("message from {who} on {}", self.info.kind);
                    if let Err(e) = host.send_untrusted(&self.name, &chat, text, &taint).await {
                        tracing::warn!(gateway = %self.name, error = %e, "stranger's message not taken");
                    }
                }
            }
            return;
        }
        if let Some(data) = &m.action {
            return self.on_action(host, &chat, data).await;
        }
        if text.is_empty() {
            return;
        }
        if text.len() == 1
            && text.chars().all(|c| c.is_ascii_digit())
            && let Some(d) = self.answer_latest(&chat, text).await
        {
            return self.reply(&chat, &format!("→ {}", label(d))).await;
        }
        let lower = text.to_ascii_lowercase();
        let (cmd, arg) = lower
            .split_once(char::is_whitespace)
            .map_or((lower.as_str(), ""), |(c, a)| (c, a.trim()));
        match cmd {
            "approve" | "reject" if !arg.is_empty() => {
                let id = arg.trim_start_matches('#');
                return self.decide(host, &chat, id, cmd == "approve").await;
            }
            "/start" | "/help" => return self.reply(&chat, HELP).await,
            "/new" => {
                let text = match host.new_session(&self.name, &chat).await {
                    Ok(()) => format!(
                        "{}New topic. I kept a summary of the last one.",
                        self.prefix(&chat)
                    ),
                    Err(e) => format!("Error: {e}"),
                };
                return self.reply(&chat, &text).await;
            }
            "/stop" => {
                let text = if host.stop(&self.name, &chat).await {
                    "Stopping."
                } else {
                    "Nothing is running."
                };
                return self.reply(&chat, text).await;
            }
            "/status" => {
                let text = format!(
                    "{}{}",
                    self.prefix(&chat),
                    host.status(&self.name, &chat).await
                );
                return self.reply(&chat, &text).await;
            }
            "/outbox" => {
                let items = host.pending_outbox().await;
                if items.is_empty() {
                    self.reply(&chat, "Nothing is waiting for you.").await;
                }
                for item in items.iter().take(10) {
                    self.outbox_card(&chat, item).await;
                }
                if items.len() > 10 {
                    let more = format!("{} more: `tern outbox` lists them all.", items.len() - 10);
                    self.reply(&chat, &more).await;
                }
                return;
            }
            _ => {}
        }
        if let Err(e) = host.send(&self.name, &chat, text).await {
            self.reply(&chat, &format!("Error: {e}")).await;
        }
    }

    fn cursor(&self) -> Value {
        self.store
            .kv_get(CURSOR_NS, &self.name)
            .ok()
            .flatten()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or(Value::Null)
    }

    /// One poll: handle what it found. The cursor is saved first, so a
    /// restart never handles a message twice.
    pub async fn poll_once(self: &Arc<Self>, host: &Arc<dyn ChatHost>) -> Result<usize, String> {
        let found = lg::poll(
            &self.host,
            &self.dir,
            &self.name,
            &self.cursor(),
            &self.settings,
        )
        .await?;
        if let Err(e) = self
            .store
            .kv_set(CURSOR_NS, &self.name, &found.cursor.to_string())
        {
            tracing::warn!(gateway = %self.name, error = %e, "could not save the gateway's cursor");
        }
        let n = found.messages.len();
        for m in found.messages {
            // Each on its own task, so a slow command can't hold up an
            // approval tap in the same poll.
            let (me, host) = (self.clone(), host.clone());
            tokio::spawn(async move { me.on_message(&host, m).await });
        }
        Ok(n)
    }
}

fn label(d: Decision) -> &'static str {
    match d {
        Decision::AllowOnce => "Allowed",
        Decision::AlwaysAllow => "Always allowed",
        Decision::AllowRun => "Allowed all sites for this run",
        Decision::AllowSession => "Allowed for this conversation",
        Decision::AlwaysAllowHost => "Site always allowed",
        Decision::Deny | Decision::Unanswered => "Denied",
    }
}

#[async_trait]
impl Gateway for LuaGateway {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &str {
        &self.info.kind
    }

    async fn run(self: Arc<Self>, host: Arc<dyn ChatHost>, cancel: CancellationToken) {
        if !self.info.poll {
            // Push only: nothing to receive.
            return cancel.cancelled().await;
        }
        let every = Duration::from_secs(self.cfg.poll_secs.max(1));
        let mut wait = Duration::ZERO;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = cancel.cancelled() => return,
            }
            wait = match self.poll_once(&host).await {
                Ok(_) => every,
                Err(e) => {
                    let backoff = (wait * 2).clamp(every, MAX_BACKOFF);
                    tracing::warn!(gateway = %self.name, error = %e, retry_in = ?backoff, "gateway poll failed");
                    backoff
                }
            };
        }
    }

    async fn deliver(&self, target: &str, run_id: Option<&str>, text: &str) -> Result<(), String> {
        let text = if text.trim().is_empty() {
            "(no response)"
        } else {
            text
        };
        let text = format!("{}{text}", self.prefix(target));
        self.send(target, &text, "message", run_id).await
    }

    async fn ask_approval(&self, target: &str, req: &ApprovalRequest) -> Decision {
        let id: String = {
            let chars: Vec<char> = req.id.chars().collect();
            chars[chars.len().saturating_sub(12)..].iter().collect()
        };
        let mut choices = vec![("1".to_owned(), "Allow once".to_owned(), Decision::AllowOnce)];
        if let Some(pattern) = &req.pattern {
            choices.push((
                "2".into(),
                format!("Always ({pattern})"),
                Decision::AlwaysAllow,
            ));
        }
        if let Some(h) = &req.egress_host {
            choices.push(("3".into(), "All sites this run".into(), Decision::AllowRun));
            choices.push(("4".into(), format!("Always {h}"), Decision::AlwaysAllowHost));
        }
        choices.push(("0".into(), "Deny".into(), Decision::Deny));
        let buttons: Vec<Button> = choices
            .iter()
            .map(|(k, l, _)| Button {
                label: l.clone(),
                data: format!("ap:{id}:{k}"),
            })
            .collect();
        let fallback = format!(
            "Reply {}.",
            choices
                .iter()
                .map(|(k, l, _)| format!("{k} · {l}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().await.insert(
            id.clone(),
            Pending {
                chat: target.to_owned(),
                choices: choices.iter().map(|(k, _, d)| (k.clone(), *d)).collect(),
                tx,
                seq: self.seq.fetch_add(1, Ordering::Relaxed),
            },
        );
        if let Err(e) = self
            .ask(
                target,
                &approval_prompt(req),
                &buttons,
                &fallback,
                "approval",
            )
            .await
        {
            tracing::warn!(gateway = %self.name, error = %e, "could not send approval request");
            self.approvals.lock().await.remove(&id);
            return Decision::Unanswered;
        }
        // Dropped if the agent stops waiting; a late answer is "Expired".
        rx.await.unwrap_or(Decision::Deny)
    }

    async fn outbox_notice(&self, target: &str, item: &OutboxItem) {
        self.outbox_card(target, item).await;
    }

    async fn secret_request_notice(&self, target: &str, req: &SecretRequest) -> Option<String> {
        let id = crate::chat_agents::get(&self.store, &self.name, target);
        let agent = self
            .store
            .agent(&id)
            .ok()
            .flatten()
            .map_or_else(|| "tern".to_owned(), |a| a.name);
        if let Err(e) = self
            .send(target, &secret_notice(req, &agent), "notice", None)
            .await
        {
            tracing::warn!(gateway = %self.name, error = %e, "could not send secret request notice");
        }
        None
    }
}

#[cfg(test)]
mod tests;
