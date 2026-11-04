//! Channels in the daemon (docs/channels-design.md §2.3, §4): sending into
//! a channel's threads, and the posts runs write when they finish.

use std::sync::Arc;

use anyhow::{Context, bail};
use tern_core::channels::{Post, author};
use tern_core::models::Change;
use tern_core::types::ContentBlock;
use tern_store::{ChannelRow, HOME, NewPost, NewThread, RunRow, Store};

use super::manager::Manager;

/// What a send into a channel did.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Sent {
    /// The user's post; none in Home, whose stream is its messages.
    pub post: Option<Post>,
    pub run_id: String,
    pub session_id: String,
    /// The text went to a run already working there (C1).
    pub queued: bool,
    /// Home: the send started a new segment of the stream (C1).
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
    Post {
        replies,
        ..Post::from(row)
    }
}

impl Manager {
    pub(super) fn publish(&self, post: Post) {
        let _ = self.rt.changes.send(Change::PostCreated { post });
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
    /// segment.
    pub async fn send_to_channel(
        self: &Arc<Self>,
        channel: &ChannelRow,
        text: &str,
        reply_to: Option<i64>,
        model: Option<&str>,
    ) -> anyhow::Result<Sent> {
        if text.trim().is_empty() {
            bail!("text is empty");
        }
        if channel.archived_at.is_some() {
            bail!("#{} is archived", channel.slug);
        }
        if channel.kind == "main" {
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
        }
        let session = match reply_to {
            Some(to) => self.reply_thread(channel, to)?,
            None => self.main_thread(channel)?,
        };
        let started = self
            .start(Some(&session), model, text, "api", "user", None)
            .await?;
        let key = format!("send:{}", started.run_id);
        let (row, _) = self.rt.store.add_post(&NewPost {
            channel_id: &channel.id,
            session_id: Some(&started.session_id),
            run_id: Some(&started.run_id),
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
            run_id: started.run_id,
            session_id: started.session_id,
            queued: false,
            new_segment: false,
        })
    }

    /// Write the post a finished run owes its channel (§2.3), at most once:
    /// a routine's result, or the agent's answer in a topic channel. Home's
    /// own conversation is its messages, so it gets none.
    pub fn post_run_result(&self, run_id: &str) -> anyhow::Result<Option<Post>> {
        let store = &self.rt.store;
        let Some(run) = store.run(run_id)? else {
            return Ok(None);
        };
        if run.parent_run_id.is_some() || run.quiet {
            return Ok(None);
        }
        let text = match run.status.as_str() {
            "completed" => final_text(store, &run)?,
            "failed" => format!(
                "Failed: {}",
                run.error.as_deref().unwrap_or("unknown error")
            ),
            _ => return Ok(None),
        };
        if text.trim().is_empty() {
            return Ok(None);
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
