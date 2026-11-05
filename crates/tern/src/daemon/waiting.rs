//! Approvals and secret requests wait in the conversation their run is in
//! (DECISIONS.md "Approvals land where the work is"): a topic channel, an
//! agent's DM, or Home. Outside Home each one is a `needs_you` post there,
//! so C4's Home pointer, the channel's `needs_you` count and clearing on
//! every surface come with it; the post becomes a line saying how it ended.

use std::sync::{Arc, Weak};

use tern_core::approval::SecretRequest;
use tern_core::approval::{ApprovalRequest, Decision};
use tern_core::channels::author;
use tern_core::models::Change;
use tern_store::{HOME, NewPost, Store};

use super::manager::{Manager, Where};

/// The conversation a run tree's root session belongs to, and its agent:
/// for a routine's private session the channel it posts to, else the
/// session's channel. Home when there is none or it was archived.
pub(crate) fn conversation_of(store: &Store, root_session: &str) -> Where {
    let Ok(Some(s)) = store.session(root_session) else {
        return (None, None);
    };
    let agent = s.agent().to_owned();
    // A routine's own session sits in Home; its work shows where it posts.
    let channel = store
        .schedule_for_session(&s.id)
        .ok()
        .flatten()
        .and_then(|r| r.channel_id)
        .or_else(|| s.channel_id.clone())
        .filter(|c| {
            store
                .channel(c)
                .ok()
                .flatten()
                .is_some_and(|c| c.archived_at.is_none())
        })
        .unwrap_or_else(|| HOME.into());
    (Some(channel), Some(agent))
}

/// Whether a request placed at `at` waits in Home's own stream.
pub(crate) fn in_home(at: &Where) -> bool {
    at.0.as_deref().is_none_or(|c| c == HOME)
}

/// The approval or secret request a `needs_you` post (or its Home pointer)
/// stands for: `("approval" | "secret", id)`.
pub fn waiting_of(store: &Store, post: &tern_store::PostRow) -> Option<(&'static str, String)> {
    let key = match (post.kind.as_str(), post.points_to) {
        ("pointer", Some(to)) => store.post(to).ok().flatten()?.dedupe_key,
        ("needs_you", _) => post.dedupe_key.clone(),
        _ => None,
    }?;
    if let Some(id) = key.strip_prefix("approval:") {
        Some(("approval", id.to_owned()))
    } else {
        key.strip_prefix("secret:")
            .map(|id| ("secret", id.to_owned()))
    }
}

/// A request's post while it waits. Dropping it without [`Waiting::close`]
/// (the run was cancelled) closes it as "no longer waiting".
pub(crate) struct Waiting {
    manager: Weak<Manager>,
    post: i64,
    what: String,
    ended: Option<String>,
}

impl Waiting {
    /// Post `text` as waiting in `at`'s channel, unless that is Home (the
    /// card is in Home's stream already).
    fn open(
        manager: &Arc<Manager>,
        at: &Where,
        key: &str,
        run_id: &str,
        session_id: &str,
        text: &str,
        what: String,
    ) -> Option<Self> {
        if in_home(at) {
            return None;
        }
        let store = &manager.rt.store;
        let channel = at.0.as_deref()?;
        let agent = store
            .session_agent(session_id)
            .ok()
            .and_then(|a| store.agent(&a).ok().flatten());
        let (author_id, author_name) = agent
            .map(|a| (a.id, a.name))
            .unwrap_or_else(|| (author::AGENT.into(), author::AGENT.into()));
        // The text quotes the model's arguments or words, which a tainted
        // run's reading may have shaped.
        let tainted = store
            .run(run_id)
            .ok()
            .flatten()
            .is_some_and(|r| r.taint.is_some());
        match store.add_post(&NewPost {
            channel_id: channel,
            session_id: Some(session_id),
            run_id: Some(run_id),
            author_kind: "agent",
            author_id: &author_id,
            author: &author_name,
            kind: "needs_you",
            text,
            tainted,
            dedupe_key: Some(key),
            ..NewPost::default()
        }) {
            Ok((row, fresh)) => {
                if fresh {
                    if let Some(pointer) = store
                        .post_by_key(&format!("pointer:{}", row.id))
                        .ok()
                        .flatten()
                    {
                        manager.publish(super::channels::view(store, pointer));
                    }
                    manager.publish(super::channels::view(store, row.clone()));
                    manager.channels_changed();
                }
                Some(Self {
                    manager: Arc::downgrade(manager),
                    post: row.id,
                    what,
                    ended: None,
                })
            }
            Err(e) => {
                tracing::warn!(error = %e, "needs-you post for a request failed");
                None
            }
        }
    }

    /// Say how it ended: "Approved", "Denied", ….
    fn close(mut self, how: &str) {
        self.ended = Some(how.to_owned());
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let Some(m) = self.manager.upgrade() else {
            return;
        };
        let how = self.ended.as_deref().unwrap_or("No longer waiting");
        let store = &m.rt.store;
        let text = format!("{how}: {}", self.what);
        let tainted = store
            .post(self.post)
            .ok()
            .flatten()
            .is_some_and(|p| p.tainted);
        if let Err(e) = store
            .update_post_text(self.post, &text, tainted)
            .and_then(|_| store.resolve_post(self.post))
        {
            tracing::warn!(post = self.post, error = %e, "closing a request's post failed");
            return;
        }
        let pointer = store
            .post_by_key(&format!("pointer:{}", self.post))
            .ok()
            .flatten();
        for row in [store.post(self.post).ok().flatten(), pointer]
            .into_iter()
            .flatten()
        {
            let _ = m.rt.changes.send(Change::PostUpdated {
                post: Box::new(super::channels::view(store, row)),
            });
        }
        m.channels_changed();
    }
}

/// At most this much of a summary or reason goes in a post.
const SHORT: usize = 200;

fn short(s: &str) -> String {
    let line = s.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= SHORT {
        line.to_owned()
    } else {
        format!("{}…", line.chars().take(SHORT).collect::<String>())
    }
}

/// An approval's post: "Needs your approval: http_fetch GET example.com".
/// Plain text: it is also a one-line note once answered.
pub(crate) fn open_approval(
    m: &Arc<Manager>,
    req: &ApprovalRequest,
    at: &Where,
) -> Option<Waiting> {
    let summary = short(&req.summary);
    let what = if summary.starts_with(&req.call.name) {
        summary
    } else {
        format!("{} {summary}", req.call.name)
    };
    let mut text = format!("Needs your approval: {what}");
    if let Some(t) = &req.taint {
        text.push_str(&format!("\n⚠ {}", tern_core::approval::taint_note(t)));
    }
    Waiting::open(
        m,
        at,
        &format!("approval:{}", req.id),
        &req.run_id,
        &req.session_id,
        &text,
        what,
    )
}

/// How an approval ended, for its post.
pub(crate) fn close_approval(w: Option<Waiting>, d: &Decision) {
    if let Some(w) = w {
        w.close(match d {
            Decision::AllowOnce => "Approved",
            Decision::AlwaysAllow => "Approved always",
            Decision::AllowRun => "Approved for this run",
            Decision::AllowSession => "Approved for this conversation",
            Decision::AlwaysAllowHost => "Approved this site always",
            Decision::Deny => "Denied",
            Decision::Unanswered => "Not answered in time",
        });
    }
}

/// A secret request's post: "Needs your secret GITHUB_TOKEN · to read issues".
pub(crate) fn open_secret(m: &Arc<Manager>, req: &SecretRequest, at: &Where) -> Option<Waiting> {
    let names: Vec<&str> = req.asks.iter().map(|a| a.name.as_str()).collect();
    let what = format!(
        "{} {}",
        if names.len() == 1 {
            "secret"
        } else {
            "secrets"
        },
        names.join(", ")
    );
    let text = format!("Needs your {what} · {}", short(&req.reason));
    Waiting::open(
        m,
        at,
        &format!("secret:{}", req.id),
        &req.run_id,
        &req.session_id,
        &text,
        what,
    )
}

/// How a secret request ended.
pub(crate) fn close_secret(w: Option<Waiting>, saved: bool) {
    if let Some(w) = w {
        w.close(if saved { "Answered" } else { "Skipped" });
    }
}
