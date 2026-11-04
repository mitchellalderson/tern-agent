//! The `channel` step (docs/channels-design.md §3): before a model call, a
//! run in a channel is shown the posts other agents, routines and threads
//! made there since its session last looked. Replay reads the journal, so
//! a resumed run sees exactly what it saw before.

use serde::{Deserialize, Serialize};
use tern_store::{ChannelRow, RunRow};

use super::taint::Taint;
use super::{Agent, AgentError};
use crate::channels::{DigestPost, pick_digest, render_digest};
use crate::journal::{InFlight, StepError, StepKind, StepOutput};
use crate::types::Message;

#[derive(Serialize)]
struct DigestInput<'a> {
    channel_id: &'a str,
}

/// What a `channel` step records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestOutput {
    pub channel_id: String,
    /// What the model was shown, oldest first.
    pub posts: Vec<DigestPost>,
    /// Unseen posts that didn't fit.
    pub left_out: usize,
    /// The newest unseen post: the session's cursor moves here.
    pub seen_through: i64,
}

/// How a channel is named in a digest.
fn place(c: &ChannelRow) -> String {
    if c.kind == "main" {
        c.title.clone()
    } else {
        format!("#{}", c.slug)
    }
}

impl Agent {
    /// Whether step `idx` is a channel step: on replay, exactly when one
    /// was recorded there; live, when the run's session is a channel thread
    /// (not a sub-agent's or a routine's) and posts came in from elsewhere
    /// since its cursor.
    pub(super) fn channel_due(
        &self,
        run: &RunRow,
        idx: u32,
    ) -> Result<Option<ChannelRow>, AgentError> {
        if run.parent_run_id.is_some() {
            return Ok(None);
        }
        match self.journal.peek(&run.id, idx)? {
            Some(StepKind::Channel) => Ok(self.store.session_channel(&run.session_id)?),
            Some(_) => Ok(None),
            None => {
                let Some(session) = self.store.session(&run.session_id)? else {
                    return Ok(None);
                };
                if matches!(session.source.as_str(), "subagent" | "cron") {
                    return Ok(None);
                }
                let Some(channel) = self.store.session_channel(&session.id)? else {
                    return Ok(None);
                };
                let seen = self.store.posts_seen(&session.id)?;
                let (_, total) = self.store.unseen_posts(&channel.id, &session.id, seen, 0)?;
                Ok((total > 0).then_some(channel))
            }
        }
    }

    /// Record the digest as one user message, then move the session's
    /// cursor (also on replay, which makes a crash between the two
    /// harmless) and taint the run if a post came from a tainted run.
    pub(super) async fn channel_step(
        &self,
        run: &RunRow,
        idx: u32,
        channel: &ChannelRow,
        taint: &mut Taint,
    ) -> Result<(), StepError> {
        let out = self
            .journal
            .step_with(
                &run.id,
                idx,
                StepKind::Channel,
                &DigestInput {
                    channel_id: &channel.id,
                },
                InFlight::Retry,
                || async {
                    let seen = self.store.posts_seen(&run.session_id)?;
                    let (rows, total) = self.store.unseen_posts(
                        &channel.id,
                        &run.session_id,
                        seen,
                        crate::channels::DIGEST_MAX_POSTS,
                    )?;
                    let seen_through = rows.first().map_or(seen, |p| p.id);
                    let newest_first = rows
                        .into_iter()
                        .map(|p| DigestPost {
                            id: p.id,
                            author: p.author,
                            author_kind: p.author_kind,
                            kind: p.kind,
                            created_at: p.created_at,
                            text: p.text,
                            tainted: p.tainted,
                        })
                        .collect();
                    let (posts, left_out) = pick_digest(newest_first, total);
                    let text = render_digest(&place(channel), &posts, left_out);
                    Ok(StepOutput::new(DigestOutput {
                        channel_id: channel.id.clone(),
                        posts,
                        left_out,
                        seen_through,
                    })
                    .with_messages(vec![Message::user_text(text)]))
                },
            )
            .await?;
        self.store
            .advance_posts_seen(&run.session_id, out.value.seen_through)?;
        if out.value.posts.iter().any(|p| p.tainted) {
            taint.mark(&self.store, &run.id, &tainted_post_reason(&place(channel)))?;
        }
        if !out.replayed {
            tracing::info!(run = %run.id, idx, posts = out.value.posts.len(), "showed new channel posts");
        }
        Ok(())
    }
}

/// Reason for a digest with a post from a tainted run.
pub fn tainted_post_reason(place: &str) -> String {
    format!("a post in {place} from a tainted run")
}
