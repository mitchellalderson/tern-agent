//! Channels in the daemon (docs/channels-design.md §2.3, §4): sending into
//! a channel's threads, and the posts runs write when they finish.

use std::sync::Arc;

use anyhow::{Context, bail};
use tern_core::channels::{CutReason, Post, author};
use tern_core::models::Change;
use tern_core::types::ContentBlock;
use tern_store::{ChannelRow, HOME, NewPost, NewThread, RunRow, SessionRow, Store};

use super::manager::Manager;

/// What a send into a channel did.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Sent {
    /// The user's post; none in Home, whose stream is its messages.
    pub post: Option<Post>,
    pub run_id: String,
    pub session_id: String,
    /// The text went to the run already working there (`run_id`), which
    /// reads it at its next model call.
    pub queued: bool,
    /// Home: the send started a new segment of the stream.
    pub new_segment: bool,
}

/// The text of a run's last assistant message.
pub fn final_text(store: &Store, run: &RunRow) -> anyhow::Result<String> {
    let messages = store.session_messages(&run.session_id)?;
    Ok(messages
        .iter()
        .rev()
        .find(|m| m.run_id.as_deref() == Some(run.id.as_str()) && m.role == "assistant")
        .and_then(|m| serde_json::from_str::<Vec<ContentBlock>>(&m.content_json).ok())
        .map(|blocks| {
            blocks
                .into_iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text),
                    _ => None,
                })
                .collect::<String>()
        })
        .unwrap_or_default())
}

/// A post with its reply count filled in.
pub fn view(store: &Store, row: tern_store::PostRow) -> Post {
    let replies = store
        .reply_counts(&[row.id])
        .ok()
        .and_then(|c| c.first().map(|(_, n)| *n))
        .unwrap_or(0);
    let points_to_channel = row
        .points_to
        .and_then(|id| store.post(id).ok().flatten())
        .map(|p| p.channel_id);
    let outbox_id = super::outbox::outbox_of_post(store, &row);
    Post {
        replies,
        points_to_channel,
        outbox_id,
        ..Post::from(row)
    }
}

impl Manager {
    pub(super) fn publish(&self, post: Post) {
        let _ = self.rt.changes.send(Change::PostCreated {
            post: Box::new(post),
        });
    }

    pub(super) fn channels_changed(&self) {
        let _ = self.rt.changes.send(Change::ChannelsChanged {});
    }

    /// A channel's main thread, created on first use.
    fn main_thread(&self, channel: &ChannelRow) -> anyhow::Result<String> {
        if let Some(id) = &channel.main_session_id
            && self.rt.store.session(id)?.is_some()
        {
            return Ok(id.clone());
        }
        let s = self.rt.store.create_thread(&NewThread {
            id: &tern_core::agent::new_id(),
            source: "api",
            channel_id: &channel.id,
            ..NewThread::default()
        })?;
        self.rt.store.set_main_session(&channel.id, &s.id)?;
        self.channels_changed();
        Ok(s.id)
    }

    /// The side thread a reply to post `to` goes to: the thread the post
    /// is in if that's a side thread, else a new one branched from it.
    fn reply_thread(&self, channel: &ChannelRow, to: i64) -> anyhow::Result<String> {
        let post = self
            .rt
            .store
            .post(to)?
            .filter(|p| p.channel_id == channel.id)
            .with_context(|| format!("post {to} is not in #{}", channel.slug))?;
        if let Some(s) = &post.session_id
            && Some(s) != channel.main_session_id.as_ref()
            && let Some(row) = self.rt.store.session(s)?
            && row.channel_id.as_deref() == Some(channel.id.as_str())
            && row.source != "cron"
        {
            return Ok(row.id);
        }
        let s = self.rt.store.create_thread(&NewThread {
            id: &tern_core::agent::new_id(),
            source: "api",
            channel_id: &channel.id,
            branched_from: Some(&format!("post:{to}")),
            ..NewThread::default()
        })?;
        Ok(s.id)
    }

    /// The user sends `text` into `channel` (`POST /v1/channels/{id}/posts`).
    /// In a topic channel it becomes a post and a run in the main thread (or
    /// in a side thread with `reply_to`); in Home it is a run in the current
    /// segment, or in a new one when §2.6 says to cut (`new_topic` forces
    /// it). Where a run is already working, the text is queued for it.
    pub async fn send_to_channel(
        self: &Arc<Self>,
        channel: &ChannelRow,
        text: &str,
        reply_to: Option<i64>,
        new_topic: bool,
        model: Option<&str>,
    ) -> anyhow::Result<Sent> {
        if text.trim().is_empty() {
            bail!("text is empty");
        }
        if channel.archived_at.is_some() {
            bail!("#{} is archived", channel.slug);
        }
        if channel.kind == "main" {
            return self.send_home(channel, text, new_topic, model).await;
        }
        let session = match reply_to {
            Some(to) => self.reply_thread(channel, to)?,
            None => self.main_thread(channel)?,
        };
        let (run_id, session_id, key, queued) = match self.queue_message(&session, text)? {
            Some((run, mail)) => (run, session, format!("queued:{mail}"), true),
            None => {
                let started = self
                    .start(Some(&session), model, text, "api", "user", None)
                    .await?;
                let key = format!("send:{}", started.run_id);
                (started.run_id, started.session_id, key, false)
            }
        };
        let (row, _) = self.rt.store.add_post(&NewPost {
            channel_id: &channel.id,
            session_id: Some(&session_id),
            run_id: Some(&run_id),
            author_kind: "user",
            author_id: author::OWNER,
            author: "you",
            kind: "message",
            text,
            reply_to,
            dedupe_key: Some(&key),
            ..NewPost::default()
        })?;
        let post = view(&self.rt.store, row);
        self.publish(post.clone());
        Ok(Sent {
            post: Some(post),
            run_id,
            session_id,
            queued,
            new_segment: false,
        })
    }

    /// A send into Home (§2.6, §4).
    async fn send_home(
        self: &Arc<Self>,
        channel: &ChannelRow,
        text: &str,
        new_topic: bool,
        model: Option<&str>,
    ) -> anyhow::Result<Sent> {
        let head = match &channel.main_session_id {
            Some(id) => self.rt.store.session(id)?,
            None => None,
        };
        let Some(head) = head else {
            let session = self.main_thread(channel)?;
            let started = self
                .start(Some(&session), model, text, "api", "user", None)
                .await?;
            return Ok(Sent {
                post: None,
                run_id: started.run_id,
                session_id: started.session_id,
                queued: false,
                new_segment: false,
            });
        };
        if !new_topic && let Some((run_id, _)) = self.queue_message(&head.id, text)? {
            return Ok(Sent {
                post: None,
                run_id,
                session_id: head.id,
                queued: true,
                new_segment: false,
            });
        }
        let session = match self.cut(&head, new_topic)? {
            Some(next) => {
                self.rt.store.set_main_session(&channel.id, &next.id)?;
                self.channels_changed();
                next
            }
            None => head.clone(),
        };
        let started = self
            .start(Some(&session.id), model, text, "api", "user", None)
            .await?;
        Ok(Sent {
            post: None,
            run_id: started.run_id,
            new_segment: session.id != head.id,
            session_id: started.session_id,
            queued: false,
        })
    }

    /// The next segment after `head`, if the stream should be cut before
    /// the user's message (§2.6): on New topic, or after idle hours or a
    /// new day. The caller has checked that no run is working in `head`
    /// (New topic cuts anyway; that run finishes in its own segment).
    pub(super) fn cut(
        &self,
        head: &SessionRow,
        new_topic: bool,
    ) -> anyhow::Result<Option<SessionRow>> {
        let reason = if new_topic {
            Some(CutReason::NewTopic)
        } else {
            let last = self.rt.store.session_last_activity(&head.id)?;
            let last = chrono::DateTime::from_timestamp_millis(last)
                .map(|t| t.with_timezone(&chrono::Local))
                .unwrap_or_else(chrono::Local::now);
            tern_core::channels::cut_reason(&self.rt.live().home, last, chrono::Local::now())
        };
        let Some(reason) = reason else {
            return Ok(None);
        };
        let next = self.rt.store.create_thread(&NewThread {
            id: &tern_core::agent::new_id(),
            source: &head.source,
            channel_id: head.channel_id.as_deref().unwrap_or(HOME),
            model: head.model.as_deref(),
            external_ref: head.external_ref.as_deref(),
            prev_session_id: Some(&head.id),
            segment_reason: Some(reason.as_str()),
            ..NewThread::default()
        })?;
        tracing::info!(prev = %head.id, next = %next.id, reason = reason.as_str(), "cut the stream into a new segment");
        Ok(Some(next))
    }

    /// Messages the user queued for `run_id` that it finished without
    /// reading: answer them with a new run in the same thread (or give them
    /// to the run that has started there since). A stopped run's queue is
    /// left alone.
    pub(super) async fn follow_up_queued(self: &Arc<Self>, run_id: &str) -> anyhow::Result<()> {
        let store = &self.rt.store;
        let Some(run) = store.run(run_id)? else {
            return Ok(());
        };
        if run.parent_run_id.is_some() || !matches!(run.status.as_str(), "completed" | "failed") {
            return Ok(());
        }
        let mail: Vec<_> = store
            .pending_mail(run_id)?
            .into_iter()
            .filter(|m| m.from_run_id.is_none() && m.kind == "message")
            .collect();
        if mail.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = mail.iter().map(|m| m.id).collect();
        let text = mail
            .iter()
            .map(|m| {
                serde_json::from_str::<serde_json::Value>(&m.body)
                    .ok()
                    .and_then(|b| b.get("text")?.as_str().map(str::to_owned))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if let Some((active, _)) = self.queue_message(&run.session_id, &text)? {
            store.hand_over_mail(&ids, &active)?;
            return Ok(());
        }
        let deliver_to = store.run_deliver_to(run_id)?;
        let started = self
            .start(
                Some(&run.session_id),
                None,
                &text,
                "api",
                "user",
                deliver_to.as_deref(),
            )
            .await?;
        store.hand_over_mail(&ids, &started.run_id)?;
        tracing::info!(run = %started.run_id, after = %run_id, "answering messages queued too late for the last run");
        Ok(())
    }

    /// Write the post a finished run owes its channel (§2.3), at most once:
    /// a routine's result, or the agent's answer in a topic channel. Home's
    /// own conversation is its messages, so it gets none.
    pub fn post_run_result(&self, run_id: &str) -> anyhow::Result<Option<Post>> {
        let store = &self.rt.store;
        let Some(run) = store.run(run_id)? else {
            return Ok(None);
        };
        // Outbox runs post their own result (`daemon::outbox`).
        if run.quiet || run.trigger == tern_core::outbox::TRIGGER {
            return Ok(None);
        }
        // A sub-agent reports to its parent; one spawned with `"post": true`
        // also posts its report (§2.4).
        let posting_child = run.parent_run_id.is_some() && store.run_can_post(&run.id)?;
        if run.parent_run_id.is_some() && !posting_child {
            return Ok(None);
        }
        let text = match run.status.as_str() {
            "completed" => final_text(store, &run)? + &super::outbox::pending_note(store, &run.id),
            "failed" => format!(
                "Failed: {}",
                run.error.as_deref().unwrap_or("unknown error")
            ),
            _ => return Ok(None),
        };
        if text.trim().is_empty() {
            return Ok(None);
        }
        // A tainted run's answer may carry what it read (tooling spec §5.3).
        let tainted = run.taint.is_some();
        if posting_child {
            let Some(channel) = store.session_channel(&run.session_id)? else {
                return Ok(None);
            };
            let who = author::subagent(run.label.as_deref().unwrap_or("agent"));
            let key = format!("answer:{}", run.id);
            let (row, created) = store.add_post(&NewPost {
                channel_id: &channel.id,
                session_id: Some(&run.session_id),
                run_id: Some(&run.id),
                author_kind: "subagent",
                author_id: &who,
                author: &who,
                kind: "update",
                text: &text,
                dedupe_key: Some(&key),
                tainted,
                ..NewPost::default()
            })?;
            let post = view(store, row);
            if created {
                self.publish(post.clone());
            }
            return Ok(Some(post));
        }
        let key = format!("answer:{}", run.id);
        let schedule = (run.trigger == "cron")
            .then(|| store.schedule_for_session(&run.session_id))
            .transpose()?
            .flatten();
        let (row, created) = if let Some(s) = schedule {
            let channel = s.channel_id.clone().unwrap_or_else(|| HOME.into());
            let title = super::scheduler::title(&s);
            store.add_post(&NewPost {
                channel_id: &channel,
                session_id: Some(&run.session_id),
                run_id: Some(&run.id),
                author_kind: "routine",
                author_id: &author::routine(&s.id),
                author: &title,
                kind: "answer",
                text: &text,
                tainted,
                dedupe_key: Some(&key),
                ..NewPost::default()
            })?
        } else {
            let Some(channel) = store.session_channel(&run.session_id)? else {
                return Ok(None);
            };
            if channel.kind != "topic" {
                return Ok(None);
            }
            let reply_to = match store.post_by_key(&format!("send:{}", run.id))? {
                Some(p) => Some(p.id),
                None => store
                    .session(&run.session_id)?
                    .and_then(|s| s.branched_from)
                    .and_then(|b| b.strip_prefix("post:")?.parse().ok()),
            };
            store.add_post(&NewPost {
                channel_id: &channel.id,
                session_id: Some(&run.session_id),
                run_id: Some(&run.id),
                author_kind: "agent",
                author_id: author::AGENT,
                author: author::AGENT,
                kind: "answer",
                text: &text,
                reply_to,
                tainted,
                dedupe_key: Some(&key),
                ..NewPost::default()
            })?
        };
        let post = view(store, row);
        if created {
            self.publish(post.clone());
        }
        Ok(Some(post))
    }

    /// Write posts a crash may have skipped: runs that finished in the last
    /// day without their answer post. Idempotent.
    pub fn reconcile_posts(&self) {
        let since = tern_store::now_ms() - 24 * 60 * 60 * 1000;
        let runs = match self.rt.store.top_runs_finished_since(since) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "reconciling channel posts failed");
                return;
            }
        };
        for run in runs {
            if let Err(e) = self.post_run_result(&run.id) {
                tracing::warn!(run = %run.id, error = %e, "writing a run's post failed");
            }
        }
    }
}

/// One conversation moved by [`Manager::move_threads`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MovedThread {
    pub session_id: String,
    /// The channel it left.
    pub from: String,
    /// "Moved to #slug" where it was.
    pub marker: Post,
    /// "Moved here from …" in the target channel's timeline.
    pub arrival: Post,
    /// The empty segment that took its place in a stream (Home's current
    /// one, a chat's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_session_id: Option<String>,
}

/// How a channel is named in move notes.
fn place(c: &ChannelRow) -> String {
    if c.kind == "main" {
        c.title.clone()
    } else {
        format!("#{}", c.slug)
    }
}

impl Manager {
    /// Whether a run is working in `session`, or a sub-agent of one of
    /// its runs.
    fn busy_in(&self, session: &str) -> bool {
        self.active_runs().into_iter().any(|(s, run)| {
            s == session
                || self
                    .rt
                    .store
                    .root_run(&run)
                    .ok()
                    .and_then(|root| self.rt.store.run(&root).ok().flatten())
                    .is_some_and(|r| r.session_id == session)
        })
    }

    /// Move conversations into topic channel `target` (DECISIONS.md "T2
    /// moves"). All are checked first: each must exist, be a conversation
    /// (not a routine's or a sub-agent's), have messages, not be in
    /// `target` already, and have nothing running. Then each becomes a
    /// thread there, its old place gets a marker and the channel an arrival
    /// post, both tainted if any of its runs is.
    /// The conversations `sessions` names, if every one may move (into
    /// `target`, when it exists yet).
    pub fn check_move(
        &self,
        sessions: &[String],
        target: Option<&str>,
    ) -> Result<Vec<tern_store::SessionRow>, MoveError> {
        use MoveError::{Bad, Busy};
        let store = &self.rt.store;
        if sessions.is_empty() {
            return Err(Bad("no conversations to move".into()));
        }
        let mut rows: Vec<tern_store::SessionRow> = Vec::new();
        for id in sessions {
            let s = store
                .session(id)
                .map_err(MoveError::internal)?
                .ok_or_else(|| Bad(format!("conversation {id} not found")))?;
            if matches!(s.source.as_str(), "subagent" | "cron") {
                return Err(Bad(format!(
                    "{id} is a {} conversation; it stays with its {}",
                    if s.source == "cron" {
                        "routine's"
                    } else {
                        "sub-agent's"
                    },
                    if s.source == "cron" {
                        "routine"
                    } else {
                        "parent"
                    }
                )));
            }
            if target.is_some() && s.channel_id.as_deref() == target {
                return Err(Bad(format!("{id} is in that channel already")));
            }
            if store.session_counts(id).map_err(MoveError::internal)?.0 == 0 {
                return Err(Bad(format!("{id} has nothing in it yet")));
            }
            if self.busy_in(id) {
                return Err(Busy(format!(
                    "tern is still working in {}; wait for it or stop it, then move it",
                    s.title.as_deref().unwrap_or(id)
                )));
            }
            if !rows.iter().any(|r| r.id == s.id) {
                rows.push(s);
            }
        }
        Ok(rows)
    }

    pub fn move_threads(
        &self,
        sessions: &[String],
        target: &ChannelRow,
    ) -> Result<Vec<MovedThread>, MoveError> {
        if target.kind != "topic" || target.archived_at.is_some() {
            return Err(MoveError::Bad(format!(
                "#{} can't take conversations",
                target.slug
            )));
        }
        let rows = self.check_move(sessions, Some(&target.id))?;
        let store = &self.rt.store;
        let mut out = Vec::new();
        for s in rows {
            let title: String = s
                .title
                .clone()
                .unwrap_or_else(|| "a conversation".into())
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(80)
                .collect();
            let tainted = store
                .session_taint(&s.id)
                .map_err(MoveError::internal)?
                .is_some();
            let last = store
                .session_last_activity(&s.id)
                .map_err(MoveError::internal)?;
            let reply = store
                .runs_for_session(&s.id)
                .map_err(MoveError::internal)?
                .into_iter()
                .rev()
                .find(|r| r.parent_run_id.is_none() && r.status == "completed")
                .and_then(|r| final_text(store, &r).ok())
                .and_then(|t| t.lines().find(|l| !l.trim().is_empty()).map(str::to_owned));
            let moved = store
                .move_thread(&s.id, &target.id, &tern_core::agent::new_id())
                .map_err(MoveError::internal)?;
            let from = store
                .channel(&moved.from)
                .map_err(MoveError::internal)?
                .ok_or_else(|| MoveError::internal("source channel gone"))?;
            let mut text = format!("Moved here from {}: {title}", place(&from));
            if let Some(r) = reply {
                let r: String = r.chars().take(200).collect();
                text.push_str(&format!("\nLast reply: {r}"));
            }
            let arrival_key = format!("arrived:{}:{}", s.id, target.id);
            let (arrival, _) = store
                .add_post(&NewPost {
                    channel_id: &target.id,
                    session_id: Some(&s.id),
                    author_kind: "user",
                    author_id: author::OWNER,
                    author: "you",
                    kind: "moved",
                    text: &text,
                    tainted,
                    dedupe_key: Some(&arrival_key),
                    ..NewPost::default()
                })
                .map_err(MoveError::internal)?;
            let marker_key = format!("moved:{}:{}", s.id, target.id);
            let marker_text = format!("Moved to {}: {title}", place(target));
            let (marker, _) = store
                .add_post(&NewPost {
                    channel_id: &from.id,
                    session_id: Some(&s.id),
                    author_kind: "user",
                    author_id: author::OWNER,
                    author: "you",
                    kind: "moved",
                    text: &marker_text,
                    points_to: Some(arrival.id),
                    tainted,
                    dedupe_key: Some(&marker_key),
                    // Where the conversation was in the stream.
                    created_at: Some(last),
                    ..NewPost::default()
                })
                .map_err(MoveError::internal)?;
            let (marker, arrival) = (view(store, marker), view(store, arrival));
            self.publish(marker.clone());
            self.publish(arrival.clone());
            tracing::info!(session = %s.id, from = %from.id, to = %target.id, "moved a conversation");
            out.push(MovedThread {
                session_id: s.id,
                from: from.id,
                marker,
                arrival,
                fresh_session_id: moved.fresh,
            });
        }
        self.channels_changed();
        Ok(out)
    }
}

/// Why a move was refused.
#[derive(Debug)]
pub enum MoveError {
    Bad(String),
    /// Something is running in a conversation.
    Busy(String),
    Internal(String),
}

impl MoveError {
    fn internal(e: impl std::fmt::Display) -> Self {
        Self::Internal(format!("{e:#}"))
    }
}
