//! Agents in topic channels (docs/agent-profiles-design.md §5): members,
//! the lead, @mentions, and the wake guards.

use rusqlite::{OptionalExtension, params};

use crate::{AgentRow, ChannelRow, DEFAULT_AGENT, Result, Store, now_ms};

/// A channel's limits on agents waking agents (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelWakes {
    /// Agents' @mentions wake the agents they name. Off by default.
    pub agent_wakes: bool,
    /// Posts at this wake depth or deeper wake nobody.
    pub max_hops: i64,
    /// Agent-started runs per hour, at most.
    pub wakes_per_hour: i64,
}

impl Default for ChannelWakes {
    fn default() -> Self {
        Self {
            agent_wakes: false,
            max_hops: 2,
            wakes_per_hour: 6,
        }
    }
}

/// An agent in a topic channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub agent_id: String,
    /// Answers the user's posts that mention no one.
    pub lead: bool,
    pub added_at: i64,
}

/// The slugs `@mentioned` in `text`, lowercased, each once, in order. An
/// `@` counts at the start or after a character that can't be in an
/// email address's local part, so `me@example.com` mentions no one.
pub fn mention_slugs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c != '@' {
            continue;
        }
        if i > 0 {
            let p = chars[i - 1];
            if p.is_alphanumeric() || matches!(p, '.' | '_' | '-' | '+' | '@') {
                continue;
            }
        }
        let slug: String = chars[i + 1..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || **c == '-')
            .collect::<String>()
            .to_ascii_lowercase();
        let slug = slug.trim_end_matches('-');
        if !slug.starts_with('-')
            && !slug.is_empty()
            && slug.len() <= 32
            && !out.iter().any(|s| s == slug)
        {
            out.push(slug.to_owned());
        }
    }
    out
}

/// `["a","b"]`. Agent ids are `[a-z0-9:-]`, so nothing needs escaping.
pub(crate) fn ids_to_json<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    let quoted: Vec<String> = ids.map(|i| format!("\"{i}\"")).collect();
    format!("[{}]", quoted.join(","))
}

pub(crate) fn ids_from_json(s: &str) -> Vec<String> {
    s.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|i| i.trim().trim_matches('"').to_owned())
        .filter(|i| !i.is_empty())
        .collect()
}

impl ChannelRow {
    /// The agent that answers posts mentioning no one: a topic channel's
    /// lead, a main channel's owner.
    pub fn lead(&self) -> &str {
        self.agent_id.as_deref().unwrap_or(DEFAULT_AGENT)
    }
}

impl Store {
    /// The active agents `text` @mentions, in order.
    pub fn mentioned_agents(&self, text: &str) -> Result<Vec<AgentRow>> {
        let mut out = Vec::new();
        for slug in mention_slugs(text) {
            if let Some(a) = self.agent(&slug)?
                && a.slug == slug
                && a.archived_at.is_none()
            {
                out.push(a);
            }
        }
        Ok(out)
    }

    /// The agents in topic channel `channel`: the lead first, then the
    /// default agent (a member of every channel), then the others in the
    /// order they joined. Archived agents are left out.
    pub fn channel_members(&self, channel: &ChannelRow) -> Result<Vec<Member>> {
        let mut out = vec![Member {
            agent_id: channel.lead().to_owned(),
            lead: true,
            added_at: channel.created_at,
        }];
        if channel.kind != "topic" {
            return Ok(out);
        }
        if channel.lead() != DEFAULT_AGENT {
            out.push(Member {
                agent_id: DEFAULT_AGENT.into(),
                lead: false,
                added_at: channel.created_at,
            });
        }
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.agent_id, m.added_at FROM channel_agents m JOIN agents a ON a.id = m.agent_id
             WHERE m.channel_id = ?1 AND a.archived_at IS NULL ORDER BY m.added_at, m.agent_id",
        )?;
        let rows = stmt.query_map([&channel.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        for row in rows {
            let (agent_id, added_at): (String, i64) = row?;
            if !out.iter().any(|m| m.agent_id == agent_id) {
                out.push(Member {
                    agent_id,
                    lead: false,
                    added_at,
                });
            }
        }
        Ok(out)
    }

    /// Whether `agent_id` is in `channel`: a main channel's owner, or a
    /// topic channel's member.
    pub fn is_member(&self, channel: &ChannelRow, agent_id: &str) -> Result<bool> {
        if channel.kind != "topic" {
            return Ok(channel.lead() == agent_id);
        }
        if agent_id == DEFAULT_AGENT || channel.lead() == agent_id {
            return Ok(true);
        }
        Ok(self
            .conn()?
            .query_row(
                "SELECT 1 FROM channel_agents WHERE channel_id = ?1 AND agent_id = ?2",
                params![channel.id, agent_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Add `agent_id` to topic channel `channel`. True if it wasn't a
    /// member (the default agent and the lead always are).
    pub fn add_channel_agent(&self, channel: &ChannelRow, agent_id: &str) -> Result<bool> {
        if channel.kind != "topic" || self.is_member(channel, agent_id)? {
            return Ok(false);
        }
        let n = self.conn()?.execute(
            "INSERT OR IGNORE INTO channel_agents (channel_id, agent_id, added_at) VALUES (?1, ?2, ?3)",
            params![channel.id, agent_id, now_ms()],
        )?;
        Ok(n > 0)
    }

    /// Take `agent_id` out of a channel. Its threads stay, as History.
    /// False if it wasn't a member that can leave (the lead and the
    /// default agent can't).
    pub fn remove_channel_agent(&self, channel: &ChannelRow, agent_id: &str) -> Result<bool> {
        if agent_id == DEFAULT_AGENT || channel.lead() == agent_id {
            return Ok(false);
        }
        let n = self.conn()?.execute(
            "DELETE FROM channel_agents WHERE channel_id = ?1 AND agent_id = ?2",
            params![channel.id, agent_id],
        )?;
        Ok(n > 0)
    }

    /// Make `agent_id` topic channel `channel_id`'s lead. The channel's
    /// main thread is the lead's, so the old lead keeps its thread as a
    /// member's and the new lead's own (if any) becomes the main one.
    pub fn set_channel_lead(&self, channel_id: &str, agent_id: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let (old, main): (Option<String>, Option<String>) = tx.query_row(
            "SELECT agent_id, main_session_id FROM channels WHERE id = ?1 AND kind = 'topic'",
            [channel_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let old = old.unwrap_or_else(|| DEFAULT_AGENT.into());
        if old == agent_id {
            return Ok(());
        }
        let now = now_ms();
        let upsert = "INSERT INTO channel_agents (channel_id, agent_id, added_at, main_session_id)
                      VALUES (?1, ?2, ?3, ?4)
                      ON CONFLICT (channel_id, agent_id) DO UPDATE SET main_session_id = ?4";
        let theirs: Option<String> = tx
            .query_row(
                "SELECT main_session_id FROM channel_agents WHERE channel_id = ?1 AND agent_id = ?2",
                params![channel_id, agent_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        tx.execute(upsert, params![channel_id, old, now, main])?;
        tx.execute(
            upsert,
            params![channel_id, agent_id, now, Option::<String>::None],
        )?;
        tx.execute(
            "UPDATE channels SET agent_id = NULLIF(?2, 'tern'), main_session_id = ?3, updated_at = ?4
             WHERE id = ?1",
            params![channel_id, agent_id, theirs, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A member's own main thread in a channel (not the lead's: that is
    /// the channel's `main_session_id`).
    pub fn member_thread(&self, channel_id: &str, agent_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT m.main_session_id FROM channel_agents m JOIN sessions s ON s.id = m.main_session_id
                 WHERE m.channel_id = ?1 AND m.agent_id = ?2",
                params![channel_id, agent_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_member_thread(&self, channel_id: &str, agent_id: &str, session: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO channel_agents (channel_id, agent_id, added_at, main_session_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (channel_id, agent_id) DO UPDATE SET main_session_id = ?4",
            params![channel_id, agent_id, now_ms(), session],
        )?;
        Ok(())
    }

    pub fn channel_wakes(&self, channel_id: &str) -> Result<ChannelWakes> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT agent_wakes, max_hops, wakes_per_hour FROM channels WHERE id = ?1",
                [channel_id],
                |r| {
                    Ok(ChannelWakes {
                        agent_wakes: r.get::<_, i64>(0)? != 0,
                        max_hops: r.get(1)?,
                        wakes_per_hour: r.get(2)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn set_channel_wakes(&self, channel_id: &str, w: &ChannelWakes) -> Result<()> {
        self.conn()?.execute(
            "UPDATE channels SET agent_wakes = ?2, max_hops = ?3, wakes_per_hour = ?4, updated_at = ?5
             WHERE id = ?1",
            params![
                channel_id,
                w.agent_wakes,
                w.max_hops,
                w.wakes_per_hour,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Runs agents' posts in `channel_id` started since `since` (unix ms):
    /// what the hourly wake budget counts.
    pub fn agent_wakes_since(&self, channel_id: &str, since: i64) -> Result<i64> {
        Ok(self.conn()?.query_row(
            "SELECT count(*) FROM runs r JOIN posts p ON p.id = r.woken_by
             WHERE p.channel_id = ?1 AND p.author_kind <> 'user' AND r.started_at >= ?2",
            params![channel_id, since],
            |r| r.get(0),
        )?)
    }

    /// Record that post `post_id` woke run `run_id`; a run woken by a
    /// tainted post starts tainted (§6.2), with `taint` as the reason.
    pub fn set_run_woken(&self, run_id: &str, post_id: i64, taint: Option<&str>) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE runs SET woken_by = ?2 WHERE id = ?1",
            params![run_id, post_id],
        )?;
        if let Some(t) = taint {
            conn.execute(
                "UPDATE runs SET input_taint = COALESCE(input_taint, ?2), taint = COALESCE(taint, ?2)
                 WHERE id = ?1",
                params![run_id, t],
            )?;
        }
        Ok(())
    }

    /// Whether post `post_id` already woke `agent_id` (one wake per agent
    /// per post).
    pub fn woke(&self, post_id: i64, agent_id: &str) -> Result<bool> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT 1 FROM runs r JOIN sessions s ON s.id = r.session_id
                 WHERE r.woken_by = ?1 AND COALESCE(s.agent_id, 'tern') = ?2 LIMIT 1",
                params![post_id, agent_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// How far down a wake chain a post by run `run_id` is: one more than
    /// the post that woke its tree's root run, else 0.
    pub fn run_wake_depth(&self, run_id: &str) -> Result<i64> {
        let root = self.root_run(run_id)?;
        Ok(self
            .conn()?
            .query_row(
                "SELECT p.wake_depth + 1 FROM runs r JOIN posts p ON p.id = r.woken_by WHERE r.id = ?1",
                [root],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// The post that woke run `run_id`, if a post did.
    pub fn run_woken_by(&self, run_id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn()?
            .query_row("SELECT woken_by FROM runs WHERE id = ?1", [run_id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten())
    }

    /// Point post `id` (written before its runs started) at the thread and
    /// run that answer it.
    pub fn set_post_run(&self, id: i64, session_id: &str, run_id: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE posts SET session_id = ?2, run_id = ?3 WHERE id = ?1",
            params![id, session_id, run_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{AgentRow, NewPost, NewThread};

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn agent(s: &Store, slug: &str) -> String {
        s.create_agent(&AgentRow {
            id: format!("tern:{slug}"),
            slug: slug.into(),
            name: slug.into(),
            memory: "private".into(),
            ..AgentRow::default()
        })
        .unwrap()
        .0
        .id
    }

    fn topic(s: &Store, slug: &str) -> ChannelRow {
        s.create_channel(&ChannelRow {
            id: slug.into(),
            slug: slug.into(),
            title: slug.into(),
            kind: "topic".into(),
            created_by: "owner".into(),
            ..ChannelRow::default()
        })
        .unwrap()
    }

    #[test]
    fn mentions_are_slugs_after_an_at_sign() {
        assert_eq!(
            mention_slugs("@Research and @ops-2, ask @research again. mail me@example.com @-x"),
            vec!["research", "ops-2"]
        );
        assert_eq!(mention_slugs("(@ops) @ops- @"), vec!["ops"]);
        assert!(mention_slugs("no one").is_empty());
        assert_eq!(
            ids_from_json(&ids_to_json(["a", "tern:b"].into_iter())),
            vec!["a", "tern:b"]
        );
    }

    #[test]
    fn members_are_the_lead_tern_and_who_joined() {
        let s = store();
        let research = agent(&s, "research");
        let ops = agent(&s, "ops");
        let c = topic(&s, "launch");
        let ids = |c: &ChannelRow| -> Vec<(String, bool)> {
            s.channel_members(c)
                .unwrap()
                .into_iter()
                .map(|m| (m.agent_id, m.lead))
                .collect()
        };
        assert_eq!(ids(&c), vec![("tern".into(), true)]);
        assert!(s.add_channel_agent(&c, &research).unwrap());
        assert!(!s.add_channel_agent(&c, &research).unwrap(), "once");
        assert!(
            !s.add_channel_agent(&c, "tern").unwrap(),
            "tern is in every channel"
        );
        assert!(s.is_member(&c, &research).unwrap());
        assert!(!s.is_member(&c, &ops).unwrap());

        // A new lead: the main thread changes hands.
        let t = s
            .create_thread(&NewThread {
                id: "tern-main",
                source: "api",
                channel_id: "launch",
                ..NewThread::default()
            })
            .unwrap();
        s.set_main_session("launch", &t.id).unwrap();
        s.set_channel_lead("launch", &research).unwrap();
        let c = s.channel("launch").unwrap().unwrap();
        assert_eq!(c.lead(), research);
        assert_eq!(c.main_session_id, None);
        assert_eq!(
            ids(&c),
            vec![(research.clone(), true), ("tern".into(), false)]
        );
        assert_eq!(
            s.member_thread("launch", "tern").unwrap().as_deref(),
            Some("tern-main")
        );
        assert!(
            !s.remove_channel_agent(&c, &research).unwrap(),
            "the lead stays"
        );
        assert!(!s.remove_channel_agent(&c, "tern").unwrap());

        // A member's thread is its own.
        let r = s
            .create_thread(&NewThread {
                id: "ops-main",
                source: "api",
                channel_id: "launch",
                agent_id: Some(&ops),
                ..NewThread::default()
            })
            .unwrap();
        assert_eq!(r.agent(), ops);
        assert_eq!(s.session_agent("ops-main").unwrap(), ops);
        s.add_channel_agent(&c, &ops).unwrap();
        s.set_member_thread("launch", &ops, "ops-main").unwrap();
        assert_eq!(
            s.member_thread("launch", &ops).unwrap().as_deref(),
            Some("ops-main")
        );
        assert!(s.remove_channel_agent(&c, &ops).unwrap());
        // Back to tern: its old thread is the main one again.
        s.set_channel_lead("launch", "tern").unwrap();
        let c = s.channel("launch").unwrap().unwrap();
        assert_eq!(c.agent_id, None);
        assert_eq!(c.main_session_id.as_deref(), Some("tern-main"));
        assert_eq!(s.channel_wakes("launch").unwrap(), ChannelWakes::default());
    }

    #[test]
    fn posts_record_mentions_and_wake_depth() {
        let s = store();
        let research = agent(&s, "research");
        topic(&s, "launch");
        s.create_session("t", "api", None).unwrap();
        let post = |author_kind: &str, run: Option<&str>, text: &str, key: &str| {
            s.add_post(&NewPost {
                channel_id: "launch",
                session_id: Some("t"),
                run_id: run,
                author_kind,
                author_id: "x",
                author: "x",
                kind: "message",
                text,
                dedupe_key: Some(key),
                ..NewPost::default()
            })
            .unwrap()
            .0
        };
        let mine = post("user", None, "@research @nobody look", "a");
        assert_eq!(mine.mentions, vec![research.clone()]);
        assert_eq!(mine.wake_depth, 0);
        s.create_run(
            &crate::NewRun {
                id: "r1",
                session_id: "t",
                trigger: "api",
                model: "m",
                ..crate::NewRun::default()
            },
            &crate::NewMessage {
                role: "user".into(),
                content_json: "[]".into(),
                text: String::new(),
                summary_of: None,
            },
        )
        .unwrap();
        s.set_run_woken("r1", mine.id, Some("woken by a tainted post"))
            .unwrap();
        let run = s.run("r1").unwrap().unwrap();
        assert_eq!(run.taint.as_deref(), Some("woken by a tainted post"));
        assert!(s.woke(mine.id, "tern").unwrap());
        assert!(!s.woke(mine.id, &research).unwrap());
        let answer = post("agent", Some("r1"), "done", "b");
        assert_eq!(answer.wake_depth, 1);
        assert!(answer.mentions.is_empty());
        assert_eq!(
            s.agent_wakes_since("launch", 0).unwrap(),
            0,
            "the user's wake"
        );
        // The waking post isn't news to the run it woke.
        let (unseen, n) = s.unseen_posts("launch", "other", 0, 10).unwrap();
        assert_eq!(n, 2);
        assert_eq!(unseen.len(), 2);
    }
}
