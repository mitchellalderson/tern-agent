//! The outbox in the daemon (docs/personal-agent-tooling-spec.md §5.2):
//! deciding items, the system run that executes an approved one, expiry,
//! and recovery after a crash. The agent queues items itself
//! (`tern_core::agent`); `Router::deferred` announces them.
//!
//! A pending item is a `needs_you` post in the conversation its run tree is
//! in (`waiting::conversation_of`), with C4's pointer in Home; clients also
//! get `Change::OutboxChanged` and the list at `/v1/outbox`.

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

/// How long stale items gather before one message reports them.
const STALE_WINDOW: Duration = Duration::from_millis(700);

/// How often pending items are checked for expiry.
const EXPIRY_TICK: Duration = Duration::from_secs(60);

impl Manager {
    /// An item that can still be decided. `discard`: a stale item may be
    /// rejected too, but never approved.
    fn pending_item(
        &self,
        id: i64,
        remote: bool,
        discard: bool,
    ) -> Result<OutboxRow, OutboxError> {
        let item = self
            .rt
            .store
            .outbox_item(id)?
            .ok_or(OutboxError::NotFound)?;
        if remote && item.local_only {
            return Err(OutboxError::LocalOnly);
        }
        if item.state != "pending" && !(discard && item.state == "stale") {
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
        let item = self.pending_item(id, remote, false)?;
        // Its file changed since it was queued: don't run it, and say so
        // once for the batch.
        if outbox::file_changed(item.file_path.as_deref(), item.file_hash.as_deref()) {
            return self.skip_stale(item);
        }
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

    /// Mark a pending item stale instead of running it.
    fn skip_stale(self: &Arc<Self>, item: OutboxRow) -> Result<OutboxItem, OutboxError> {
        let path = item.file_path.clone().unwrap_or_default();
        let result = serde_json::to_string(&tern_core::tool::ToolResult::error(
            outbox::stale_message(&path),
        ))
        .map_err(|e| OutboxError::Internal(e.into()))?;
        self.rt.store.mark_outbox_stale(item.id, &result)?;
        let item = self
            .rt
            .store
            .outbox_item(item.id)?
            .ok_or(OutboxError::NotFound)?;
        tracing::info!(outbox = item.id, tool = %item.tool, "outbox item stale: its file changed");
        self.item_stale(&item);
        Ok(self.outbox_view(item))
    }

    /// A stale item: tell its run, update its card, and queue its line for
    /// the batch message.
    fn item_stale(self: &Arc<Self>, item: &OutboxRow) {
        if let Err(e) = tern_core::agent::tell_outbox_run(&self.rt.store, item, None) {
            tracing::warn!(outbox = item.id, error = %e, "telling the run failed");
        }
        self.outbox_changed(item);
        self.note_stale(item.clone());
    }

    /// Report stale items once per file: a cascade, or several approvals
    /// in a row, share one message. Items gather for a moment first.
    fn note_stale(self: &Arc<Self>, item: OutboxRow) {
        let first = {
            let Ok(mut b) = self.stale_batch.lock() else {
                return;
            };
            b.push(item);
            b.len() == 1
        };
        if first {
            let me = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(STALE_WINDOW).await;
                me.flush_stale().await;
            });
        }
    }

    /// Post one summary per file for the items gathered.
    async fn flush_stale(&self) {
        let items = self
            .stale_batch
            .lock()
            .map(|mut b| std::mem::take(&mut *b))
            .unwrap_or_default();
        let mut by_file: Vec<(String, Vec<OutboxRow>)> = Vec::new();
        for item in items {
            let file = item.file_path.clone().unwrap_or_default();
            match by_file.iter_mut().find(|(f, _)| *f == file) {
                Some((_, v)) => v.push(item),
                None => by_file.push((file, vec![item])),
            }
        }
        for (file, items) in by_file {
            let text = outbox::stale_summary(&file, items.len());
            let first = &items[0];
            self.outbox_post(first, &text, &format!("outbox-stale:{}", first.id));
            if let Some(target) = self.outbox_target(first)
                && let Err(e) = self.send_to(&target, None, &text).await
            {
                tracing::warn!(target, error = %e, "outbox summary delivery failed");
            }
        }
    }

    /// Reject item `id`, with an optional note the run that queued it gets
    /// if it is still working.
    pub fn reject_outbox(
        &self,
        id: i64,
        note: Option<&str>,
        remote: bool,
    ) -> Result<OutboxItem, OutboxError> {
        self.pending_item(id, remote, true)?;
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
    async fn outbox_finished(self: &Arc<Self>, item: &OutboxRow) {
        if item.state == "approved" {
            return; // interrupted; resumed later
        }
        if item.state == "stale" {
            self.item_stale(item);
            return;
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
            .and_then(|r| store.run(&r).ok().flatten());
        let mut item = OutboxItem::from(row);
        if let Some(r) = root {
            (item.channel_id, item.agent_id) =
                super::waiting::conversation_of(store, &r.session_id);
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

    /// A new item: its `needs_you` post (and Home pointer), clients, and
    /// the chat its run reports to. A routine's chat gets no card now: the
    /// routine's own delivery, which keeps its quiet hours and initiative,
    /// lists what waits (spec W4: "in the morning").
    pub async fn announce_outbox(&self, id: i64) {
        let Ok(Some(item)) = self.rt.store.outbox_item(id) else {
            return;
        };
        if !self.queued_by_routine(&item)
            && let Some(target) = self.outbox_target(&item)
            && let Some((name, chat)) = target.split_once(':')
            && let Some(g) = self.gateway(name)
        {
            g.outbox_notice(chat, &self.outbox_view(item.clone())).await;
        }
        self.outbox_changed(&item);
    }

    /// Whether the run tree that queued `item` is a routine's.
    fn queued_by_routine(&self, item: &OutboxRow) -> bool {
        let store = &self.rt.store;
        store
            .root_run(&item.run_id)
            .ok()
            .and_then(|r| store.run(&r).ok().flatten())
            .is_some_and(|r| r.trigger == "cron")
    }

    /// Every change to an item goes through here: its `needs_you` post
    /// follows its state (open while pending, resolved after), chats that
    /// show its card hear how it ended, and clients get `outbox_changed`.
    pub(super) fn outbox_changed(&self, item: &OutboxRow) {
        if item.state == "pending" {
            self.open_needs_you(item);
        } else {
            self.resolve_needs_you(item);
            let view = self.outbox_view(item.clone());
            for g in self
                .routes
                .gateways
                .get()
                .into_iter()
                .flat_map(|g| g.values())
            {
                let (g, view) = (g.clone(), view.clone());
                tokio::spawn(async move { g.outbox_resolved(&view).await });
            }
        }
        let _ = self.rt.changes.send(Change::OutboxChanged {
            id: item.id,
            state: item.state.clone(),
        });
    }

    /// A pending item's `needs_you` post, in the channel the run tree
    /// started in (Home, an agent's DM, a topic channel), by its agent.
    /// Outside Home the store adds the Home pointer (C4). Once per item.
    fn open_needs_you(&self, item: &OutboxRow) {
        let store = &self.rt.store;
        let view = self.outbox_view(item.clone());
        let channel = view
            .channel_id
            .clone()
            .filter(|c| {
                store
                    .channel(c)
                    .ok()
                    .flatten()
                    .is_some_and(|c| c.archived_at.is_none())
            })
            .unwrap_or_else(|| HOME.into());
        let agent = view
            .agent_id
            .as_deref()
            .and_then(|a| store.agent(a).ok().flatten());
        let (author_id, author_name) = agent
            .map(|a| (a.id, a.name))
            .unwrap_or_else(|| (author::AGENT.into(), author::AGENT.into()));
        let mut text = format!(
            "Waiting for your approval (outbox #{}): {}",
            item.id,
            view.headline()
        );
        if let Some(t) = &item.taint {
            text.push_str(&format!("\n⚠ {}", tern_core::approval::taint_note(t)));
        }
        let key = needs_you_key(item.id);
        match store.add_post(&NewPost {
            channel_id: &channel,
            session_id: Some(&item.session_id),
            run_id: Some(&item.run_id),
            author_kind: "agent",
            author_id: &author_id,
            author: &author_name,
            kind: "needs_you",
            text: &text,
            // The summary is the model's arguments, which a tainted run's
            // reading may have shaped.
            tainted: item.taint.is_some(),
            dedupe_key: Some(&key),
            ..NewPost::default()
        }) {
            Ok((row, true)) => {
                if let Some(pointer) = row_pointer(store, row.id) {
                    self.publish(super::channels::view(store, pointer));
                }
                self.publish(super::channels::view(store, row));
                self.channels_changed();
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(outbox = item.id, error = %e, "outbox needs-you post failed"),
        }
    }

    /// Close an item's `needs_you` post and its Home pointer.
    fn resolve_needs_you(&self, item: &OutboxRow) {
        let store = &self.rt.store;
        let resolved = store
            .post_by_key(&needs_you_key(item.id))
            .ok()
            .flatten()
            .is_some_and(|p| store.resolve_post(p.id).unwrap_or(false));
        if resolved {
            self.channels_changed();
        }
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

/// The dedupe key of an item's `needs_you` post.
pub fn needs_you_key(id: i64) -> String {
    format!("outbox:{id}")
}

/// The outbox item a `needs_you` post or its Home pointer stands for.
pub fn outbox_of_post(store: &tern_store::Store, post: &tern_store::PostRow) -> Option<i64> {
    let key = match (post.kind.as_str(), post.points_to) {
        ("pointer", Some(to)) => store.post(to).ok().flatten()?.dedupe_key,
        ("needs_you", _) => post.dedupe_key.clone(),
        _ => None,
    }?;
    key.strip_prefix("outbox:")?.parse().ok()
}

/// The Home pointer the store wrote for `needs_you` post `id`, if any.
fn row_pointer(store: &tern_store::Store, id: i64) -> Option<tern_store::PostRow> {
    store.post_by_key(&format!("pointer:{id}")).ok().flatten()
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
