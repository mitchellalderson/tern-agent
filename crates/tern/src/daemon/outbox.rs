//! The outbox in the daemon (docs/personal-agent-tooling-spec.md §5.2):
//! deciding items, the system run that executes an approved one, expiry,
//! and recovery after a crash. The agent queues items itself
//! (`tern_core::agent`); `Router::deferred` announces them.
//!
//! Home integration is deliberately thin: a `Change::OutboxChanged` event,
//! the list at `/v1/outbox`, and an `update` post for results and expiries.
//! C4's `needs_you` pointers into Home should replace the announcement here
//! (`Manager::announce_outbox`) when they land.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;
use tern_core::channels::author;
use tern_core::models::Change;
use tern_core::outbox::{self, OutboxItem};
use tern_store::{HOME, NewPost, OutboxRow};
use tokio_util::sync::CancellationToken;

use super::manager::Manager;

/// Why a decision was refused.
#[derive(Debug)]
pub enum OutboxError {
    NotFound,
    /// Only a client on this host may decide it.
    LocalOnly,
    /// Already decided, or expired.
    NotPending(String),
    Bad(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for OutboxError {
    fn from(e: anyhow::Error) -> Self {
        OutboxError::Internal(e)
    }
}

impl From<tern_store::StoreError> for OutboxError {
    fn from(e: tern_store::StoreError) -> Self {
        OutboxError::Internal(e.into())
    }
}

impl std::fmt::Display for OutboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutboxError::NotFound => f.write_str("no such outbox item"),
            OutboxError::LocalOnly => f.write_str(
                "this item runs a command on the host outside the sandbox; decide it in the TUI or \
                 desktop app on that machine",
            ),
            OutboxError::NotPending(state) => write!(f, "this item is already {state}"),
            OutboxError::Bad(m) => f.write_str(m),
            OutboxError::Internal(e) => write!(f, "{e:#}"),
        }
    }
}

/// How often pending items are checked for expiry.
const EXPIRY_TICK: Duration = Duration::from_secs(60);

impl Manager {
    fn pending_item(&self, id: i64, remote: bool) -> Result<OutboxRow, OutboxError> {
        let item = self
            .rt
            .store
            .outbox_item(id)?
            .ok_or(OutboxError::NotFound)?;
        if remote && item.local_only {
            return Err(OutboxError::LocalOnly);
        }
        if item.state != "pending" {
            return Err(OutboxError::NotPending(item.state));
        }
        Ok(item)
    }

    /// Approve item `id` (`remote`: asked over TCP or from a chat, so not a
    /// local-only item), with `input` replacing its arguments, and start
    /// the run that executes it. Exactly once: a second approval is
    /// refused.
    pub fn approve_outbox(
        self: &Arc<Self>,
        id: i64,
        input: Option<Value>,
        remote: bool,
    ) -> Result<OutboxItem, OutboxError> {
        let item = self.pending_item(id, remote)?;
        let edited = match input {
            Some(v @ Value::Object(_)) => Some(v.to_string()),
            Some(_) => return Err(OutboxError::Bad("input must be a JSON object".into())),
            None => None,
        };
        let exec_run_id = tern_core::agent::new_id();
        if !self
            .rt
            .store
            .approve_outbox(id, &exec_run_id, edited.as_deref())?
        {
            let now = self
                .rt
                .store
                .outbox_item(id)?
                .ok_or(OutboxError::NotFound)?;
            let state = if now.state == "pending" {
                "expired".into()
            } else {
                now.state
            };
            return Err(OutboxError::NotPending(state));
        }
        tracing::info!(outbox = id, tool = %item.tool, edited = edited.is_some(), "outbox item approved");
        let item = self
            .rt
            .store
            .outbox_item(id)?
            .ok_or(OutboxError::NotFound)?;
        self.start_outbox_run(&item)?;
        self.outbox_changed(&item);
        Ok(self.outbox_view(item))
    }

    /// Reject item `id`, with an optional note the run that queued it gets
    /// if it is still working.
    pub fn reject_outbox(
        &self,
        id: i64,
        note: Option<&str>,
        remote: bool,
    ) -> Result<OutboxItem, OutboxError> {
        self.pending_item(id, remote)?;
        let note = note.map(str::trim).filter(|n| !n.is_empty());
        if !self.rt.store.reject_outbox(id, note)? {
            return Err(OutboxError::NotPending("decided".into()));
        }
        let item = self
            .rt
            .store
            .outbox_item(id)?
            .ok_or(OutboxError::NotFound)?;
        tracing::info!(outbox = id, tool = %item.tool, "outbox item rejected");
        tern_core::agent::tell_outbox_run(&self.rt.store, &item, None)?;
        self.outbox_changed(&item);
        Ok(self.outbox_view(item))
    }

    /// Create the system run for an approved item (if a crash didn't leave
    /// one already) and drive it.
    fn start_outbox_run(self: &Arc<Self>, item: &OutboxRow) -> anyhow::Result<()> {
        let exec = item
            .exec_run_id
            .clone()
            .context("approved item without a run id")?;
        if self.rt.store.run(&exec)?.is_none() {
            let origin = self
                .rt
                .store
                .run(&item.run_id)?
                .context("the run that queued it is gone")?;
            let text = format!("[You approved outbox #{}: {}]", item.id, item.summary);
            let input = self
                .rt
                .agent
                .journal()
                .to_row(&tern_core::types::Message::user_text(&text))?;
            self.rt.store.create_run(
                &tern_store::NewRun {
                    id: &exec,
                    session_id: &item.session_id,
                    trigger: outbox::TRIGGER,
                    model: origin.model.as_deref().unwrap_or_default(),
                    system_prompt: "",
                    ..tern_store::NewRun::default()
                },
                &input,
            )?;
        }
        self.drive_outbox_run(exec);
        Ok(())
    }

    /// Drive an exec run on its own task. Not through `spawn`: it shares
    /// its session with the run that queued it, which may still be the one
    /// active there.
    pub fn drive_outbox_run(self: &Arc<Self>, run_id: String) {
        let me = self.clone();
        tokio::spawn(async move {
            let result = me.rt.agent.drive(&run_id, CancellationToken::new()).await;
            if let Err(e) = result {
                tracing::error!(run = %run_id, error = %e, "outbox run could not be driven");
            }
            match me.rt.store.outbox_by_exec_run(&run_id) {
                Ok(Some(item)) => me.outbox_finished(&item).await,
                Ok(None) => {}
                Err(e) => tracing::warn!(run = %run_id, error = %e, "reading outbox item failed"),
            }
        });
    }

    /// Post an executed item's result to its channel and its chat, once.
    async fn outbox_finished(&self, item: &OutboxRow) {
        if item.state == "approved" {
            return; // interrupted; resumed later
        }
        let failed = item.state == "failed";
        let result: Option<tern_core::tool::ToolResult> = item
            .result_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok());
        let detail = result
            .map(|r| tern_core::agent::preview(&r.content))
            .unwrap_or_default();
        let text = format!(
            "Outbox #{} {}: {}{}",
            item.id,
            if failed { "failed" } else { "done" },
            item.summary,
            if detail.is_empty() {
                String::new()
            } else {
                format!("\n{detail}")
            }
        );
        self.outbox_post(item, &text, &format!("outbox-done:{}", item.id));
        if let Some(target) = self.outbox_target(item)
            && let Err(e) = self.send_to(&target, None, &text).await
        {
            tracing::warn!(target, error = %e, "outbox result delivery failed");
        }
        self.outbox_changed(item);
    }

    /// `row` for clients, with where its run tree started.
    pub fn outbox_view(&self, row: OutboxRow) -> OutboxItem {
        let store = &self.rt.store;
        let root = store
            .root_run(&row.run_id)
            .ok()
            .and_then(|r| store.run(&r).ok().flatten())
            .and_then(|r| store.session(&r.session_id).ok().flatten());
        let mut item = OutboxItem::from(row);
        if let Some(s) = root {
            item.agent_id = Some(s.agent().to_owned());
            item.channel_id = s.channel_id;
        }
        item
    }

    /// An `update` post in the channel of the item's session (Home if it
    /// has none), by the session's agent.
    fn outbox_post(&self, item: &OutboxRow, text: &str, key: &str) {
        let store = &self.rt.store;
        let agent = store
            .session_agent(&item.session_id)
            .ok()
            .and_then(|a| store.agent(&a).ok().flatten());
        let (author_id, author_name) = agent
            .map(|a| (a.id, a.name))
            .unwrap_or_else(|| (author::AGENT.into(), author::AGENT.into()));
        let channel = store
            .session_channel(&item.session_id)
            .ok()
            .flatten()
            .map(|c| c.id)
            .unwrap_or_else(|| HOME.into());
        match store.add_post(&NewPost {
            channel_id: &channel,
            session_id: Some(&item.session_id),
            run_id: item.exec_run_id.as_deref(),
            author_kind: "agent",
            author_id: &author_id,
            author: &author_name,
            kind: "update",
            text,
            dedupe_key: Some(key),
            ..NewPost::default()
        }) {
            Ok((row, true)) => self.publish(super::channels::view(store, row)),
            Ok(_) => {}
            Err(e) => tracing::warn!(outbox = item.id, error = %e, "outbox post failed"),
        }
    }

    /// The chat the run tree that queued `item` reports to, if any.
    fn outbox_target(&self, item: &OutboxRow) -> Option<String> {
        let store = &self.rt.store;
        let root = store.root_run(&item.run_id).ok()?;
        store.run_deliver_to(&root).ok().flatten()
    }

    /// A new item: tell clients, and the chat its run reports to.
    pub async fn announce_outbox(&self, id: i64) {
        let Ok(Some(item)) = self.rt.store.outbox_item(id) else {
            return;
        };
        if let Some(target) = self.outbox_target(&item)
            && let Some((name, chat)) = target.split_once(':')
            && let Some(g) = self.gateway(name)
        {
            g.outbox_notice(chat, &self.outbox_view(item.clone())).await;
        }
        self.outbox_changed(&item);
    }

    pub(super) fn outbox_changed(&self, item: &OutboxRow) {
        let _ = self.rt.changes.send(Change::OutboxChanged {
            id: item.id,
            state: item.state.clone(),
        });
    }

    /// Expire overdue items: each tells its run if it is still working and
    /// posts once to Home.
    pub fn expire_outbox(&self) {
        let expired = match self.rt.store.expire_outbox(tern_store::now_ms()) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "expiring outbox items failed");
                return;
            }
        };
        for mut item in expired {
            item.state = "expired".into();
            tracing::info!(outbox = item.id, tool = %item.tool, "outbox item expired");
            if let Err(e) = tern_core::agent::tell_outbox_run(&self.rt.store, &item, None) {
                tracing::warn!(outbox = item.id, error = %e, "telling the run failed");
            }
            let text = format!(
                "Outbox #{} expired without an answer, so it never ran: {}",
                item.id, item.summary
            );
            let key = format!("outbox-expired:{}", item.id);
            match self.rt.store.add_post(&NewPost {
                channel_id: HOME,
                author_kind: "agent",
                author_id: author::AGENT,
                author: author::AGENT,
                kind: "update",
                text: &text,
                dedupe_key: Some(&key),
                ..NewPost::default()
            }) {
                Ok((row, true)) => self.publish(super::channels::view(&self.rt.store, row)),
                Ok(_) => {}
                Err(e) => tracing::warn!(outbox = item.id, error = %e, "outbox post failed"),
            }
            self.outbox_changed(&item);
        }
    }

    /// After a restart: start runs for items approved just before a crash.
    /// (Exec runs a crash cut off are resumed with the other runs.)
    pub fn reconcile_outbox(self: &Arc<Self>) {
        match self.rt.store.approved_outbox_without_run() {
            Ok(items) => {
                for item in items {
                    tracing::info!(
                        outbox = item.id,
                        "starting an approved outbox item after restart"
                    );
                    if let Err(e) = self.start_outbox_run(&item) {
                        tracing::warn!(outbox = item.id, error = %e, "could not start it");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "reading approved outbox items failed"),
        }
    }

    /// Expire items every minute until `cancel`.
    pub fn spawn_outbox_expiry(self: &Arc<Self>, cancel: CancellationToken) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                me.expire_outbox();
                tokio::select! {
                    () = tokio::time::sleep(EXPIRY_TICK) => {}
                    () = cancel.cancelled() => break,
                }
            }
        });
    }
}

/// "Waiting for your approval: …" for a run's final post, or empty.
pub fn pending_note(store: &tern_store::Store, run_id: &str) -> String {
    let pending = store.pending_outbox_for_run(run_id).unwrap_or_default();
    if pending.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = pending
        .iter()
        .map(|o| format!("- outbox #{}: {} · {}", o.id, o.effect, o.summary))
        .collect();
    format!("\n\nWaiting for your approval:\n{}", lines.join("\n"))
}

/// The JSON body of an approve request.
pub fn approve_input(body: &Value) -> Option<Value> {
    body.get("input").filter(|v| !v.is_null()).cloned()
}
