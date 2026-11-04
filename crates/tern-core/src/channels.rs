//! Channels: topic timelines over one shared memory, and Home
//! (docs/channels-design.md). The store holds them; this module has the
//! shapes clients see and the rules that don't need the store.

use serde::{Deserialize, Serialize};

/// Stable author ids. They are what a member id (spec §15) or an agent
/// profile id replaces later, so they never change for an author; the
/// display name (`author`) may.
pub mod author {
    /// The one person who owns this core; becomes the owner's member id.
    pub const OWNER: &str = "owner";
    /// The main agent; becomes its agent profile's id.
    pub const AGENT: &str = "tern";

    /// A sub-agent of the main agent, by its label.
    pub fn subagent(label: &str) -> String {
        format!("{AGENT}/{label}")
    }

    /// A routine, by its schedule id (its title may change).
    pub fn routine(schedule_id: &str) -> String {
        format!("routine:{schedule_id}")
    }
}

/// A post as clients see it (`GET /v1/channels/{id}/posts`, `post_created`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    pub id: i64,
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// `user`, `agent`, `subagent` or `routine`.
    pub author_kind: String,
    /// Stable identity (see [`author`]).
    pub author_id: String,
    /// What to show.
    pub author: String,
    /// `message`, `answer`, `update`, `needs_you` or `pointer`.
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points_to: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tainted: bool,
    /// Posts replying to this one in a side thread.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub replies: i64,
    /// Unix ms.
    pub created_at: i64,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

impl From<tern_store::PostRow> for Post {
    fn from(p: tern_store::PostRow) -> Self {
        Self {
            id: p.id,
            channel_id: p.channel_id,
            session_id: p.session_id,
            run_id: p.run_id,
            author_kind: p.author_kind,
            author_id: p.author_id,
            author: p.author,
            kind: p.kind,
            text: p.text,
            reply_to: p.reply_to,
            points_to: p.points_to,
            tainted: p.tainted,
            replies: 0,
            created_at: p.created_at,
        }
    }
}

/// A channel slug: `[a-z0-9][a-z0-9-]{0,31}`.
pub fn check_slug(slug: &str) -> Result<(), String> {
    let ok = !slug.is_empty()
        && slug.len() <= 32
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "channel name {slug:?} must be 1–32 lowercase letters, digits or dashes, not starting with a dash"
        ))
    }
}

/// The system prompt section for runs in a channel with instructions.
pub fn instructions_section(slug: &str, instructions: &str) -> Option<String> {
    let text = instructions.trim();
    (!text.is_empty()).then(|| {
        format!(
            "\n\n## Channel #{slug}\n\nThis conversation is in the #{slug} channel. The user's instructions for it:\n\n{text}"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        for ok in ["finances", "q4-launch", "a", "0", &"x".repeat(32)] {
            assert!(check_slug(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "Finances", "a b", "a_b", "#x", &"x".repeat(33)] {
            assert!(check_slug(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn author_ids_are_stable_names() {
        assert_eq!(author::subagent("research-1"), "tern/research-1");
        assert_eq!(author::routine("abc"), "routine:abc");
    }

    #[test]
    fn instructions_become_a_prompt_section_only_when_given() {
        assert_eq!(instructions_section("fin", "  "), None);
        let s = instructions_section("fin", "Amounts in EUR.").unwrap();
        assert!(s.contains("## Channel #fin") && s.ends_with("Amounts in EUR."));
    }
}
