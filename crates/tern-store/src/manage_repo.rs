//! Deleting agents and channels, renaming channels, and the counts a
//! confirmation shows (DECISIONS.md "Editing and deleting").
//!
//! An agent is never removed from the table: it becomes a tombstone
//! (`deleted_at`, slug freed), so every id in history keeps resolving. A
//! channel's own rows go; its conversations stay in History.

use rusqlite::{OptionalExtension, params};

use crate::{DEFAULT_AGENT, HOME, LedgerEntry, Result, Store, StoreError, now_ms};

/// What an agent owns, for deciding whether it can go and for the
/// confirmation text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentUse {
    /// `(id, first line of its prompt)` of the routines it owns.
    pub routines: Vec<(String, String)>,
    /// `(id, slug)` of the topic channels it leads.
    pub led_channels: Vec<(String, String)>,
    /// `(id, slug)` of the topic channels it is a (non-lead) member of.
    pub member_channels: Vec<(String, String)>,
    /// Its conversations (top-level sessions): they stay in History.
    pub conversations: usize,
    /// Memories private to it: they are deleted with it.
    pub memories: usize,
    /// Runs of its in progress.
    pub running_runs: usize,
    /// Outbox items from its runs waiting for an answer.
    pub pending_outbox: usize,
}

/// What happens to an agent's routines when it is deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineFate {
    /// They become this agent's (id), with their results going to its DM
    /// when they went to the deleted agent's.
    Move(String),
    /// They are deleted (with their webhooks).
    Delete,
}

/// What a channel holds, for the confirmation text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelUse {
    pub posts: usize,
    /// Conversations in it: they stay in History, under Home.
    pub conversations: usize,
    /// `(id, first line of its prompt)` of the routines that post to it.
    pub routines: Vec<(String, String)>,
    /// Agents that joined it (besides the lead and `tern`).
    pub members: usize,
    pub running_runs: usize,
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or_default().trim();
    let mut out: String = l.chars().take(80).collect();
    if l.chars().count() > 80 {
        out.push('…');
    }
    out
}

impl Store {
    /// What agent `id` owns.
    pub fn agent_use(&self, id: &str) -> Result<AgentUse> {
        let conn = self.conn()?;
        let pairs = |sql: &str| -> Result<Vec<(String, String)>> {
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        let count = |sql: &str| -> Result<usize> {
            let n: i64 = conn.query_row(sql, [id], |r| r.get(0))?;
            Ok(n.max(0) as usize)
        };
        Ok(AgentUse {
            routines: pairs(
                "SELECT id, prompt FROM schedules WHERE COALESCE(agent_id, 'tern') = ?1
                 ORDER BY next_run_at",
            )?
            .into_iter()
            .map(|(i, p)| (i, first_line(&p)))
            .collect(),
            led_channels: pairs(
                "SELECT id, slug FROM channels WHERE kind = 'topic' AND agent_id = ?1 ORDER BY slug",
            )?,
            member_channels: pairs(
                "SELECT c.id, c.slug FROM channel_agents m JOIN channels c ON c.id = m.channel_id
                 WHERE m.agent_id = ?1 AND COALESCE(c.agent_id, 'tern') != ?1 ORDER BY c.slug",
            )?,
            conversations: count(
                "SELECT count(*) FROM sessions WHERE COALESCE(agent_id, 'tern') = ?1
                   AND source != 'subagent'",
            )?,
            memories: count("SELECT count(*) FROM memories WHERE agent_id = ?1")?,
            running_runs: count(
                "SELECT count(*) FROM runs r JOIN sessions s ON s.id = r.session_id
                 WHERE r.status = 'running' AND COALESCE(s.agent_id, 'tern') = ?1",
            )?,
            pending_outbox: count(
                "SELECT count(*) FROM outbox o JOIN runs r ON r.id = o.run_id
                 JOIN sessions s ON s.id = r.session_id
                 WHERE o.state = 'pending' AND COALESCE(s.agent_id, 'tern') = ?1",
            )?,
        })
    }

    /// Delete agent `id`: its routines move or go (`routines`), it leaves
    /// every channel and every other agent's `delegates`, its private
    /// memories go, its DM is archived and renamed out of the way, and the
    /// row becomes a tombstone with its slug freed. Conversations, runs and
    /// posts are untouched, so they keep showing its name. Topic channels
    /// it leads must have been handed over first
    /// ([`Self::set_channel_lead`]). Returns false if there is no such
    /// agent. The default agent can't be deleted.
    pub fn delete_agent(&self, id: &str, routines: &RoutineFate) -> Result<bool> {
        if id == DEFAULT_AGENT {
            return Err(StoreError::Invalid(
                "the default agent can't be deleted".into(),
            ));
        }
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT name, slug FROM agents WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((name, slug)) = row else {
            return Ok(false);
        };
        let led: i64 = tx.query_row(
            "SELECT count(*) FROM channels WHERE kind = 'topic' AND agent_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        if led > 0 {
            return Err(StoreError::Invalid(format!(
                "@{slug} still leads {led} channel(s); hand them over first"
            )));
        }
        let routine_ids: Vec<String> = {
            let mut stmt = tx.prepare("SELECT id FROM schedules WHERE agent_id = ?1")?;
            let rows = stmt.query_map([id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        match routines {
            RoutineFate::Move(to) => {
                let to_db = (to != DEFAULT_AGENT).then_some(to.as_str());
                let to_dm: String = tx
                    .query_row(
                        "SELECT id FROM channels WHERE agent_id = ?1 AND kind = 'main'
                         ORDER BY created_at LIMIT 1",
                        [to],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or_else(|| HOME.into());
                // A routine's session acts as its agent.
                tx.execute(
                    "UPDATE sessions SET agent_id = ?2 WHERE id IN
                       (SELECT session_id FROM schedules WHERE agent_id = ?1)",
                    params![id, to_db],
                )?;
                tx.execute(
                    "UPDATE schedules SET channel_id = ?3 WHERE agent_id = ?1
                       AND channel_id IN (SELECT id FROM channels WHERE agent_id = ?1 AND kind = 'main')",
                    params![id, to_db, to_dm],
                )?;
                tx.execute(
                    "UPDATE schedules SET agent_id = ?2 WHERE agent_id = ?1",
                    params![id, to_db],
                )?;
            }
            RoutineFate::Delete => {
                tx.execute("DELETE FROM schedules WHERE agent_id = ?1", [id])?;
            }
        }
        tx.execute("DELETE FROM channel_agents WHERE agent_id = ?1", [id])?;
        // Other agents' delegate lists drop it.
        let lists: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, delegates FROM agents WHERE delegates IS NOT NULL AND id != ?1",
            )?;
            let rows = stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (other, json) in lists {
            let Ok(list) = serde_json::from_str::<Vec<String>>(&json) else {
                continue;
            };
            if list.iter().any(|x| x == id || *x == slug) {
                let kept: Vec<&String> = list.iter().filter(|x| *x != id && **x != slug).collect();
                tx.execute(
                    "UPDATE agents SET delegates = ?2 WHERE id = ?1",
                    params![other, serde_json::to_string(&kept).unwrap_or_default()],
                )?;
            }
        }
        let memories = tx.execute("DELETE FROM memories WHERE agent_id = ?1", [id])?;
        // Its DM stays (archived) so the conversations and posts in it
        // remain; its slug is freed.
        tx.execute(
            "UPDATE channels SET archived_at = COALESCE(archived_at, ?2), slug = '~' || id,
               updated_at = ?2 WHERE agent_id = ?1 AND kind = 'main' AND id != 'home'",
            params![id, now],
        )?;
        tx.execute(
            "UPDATE agents SET deleted_at = ?2, archived_at = COALESCE(archived_at, ?2),
               slug = '~' || id, updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        let e = LedgerEntry::new(
            "agent.delete",
            id,
            format!("deleted agent {name} (@{slug}); its history stays readable"),
        )
        .detail(serde_json::json!({
            "routines": routine_ids.len(),
            "routines_fate": match routines {
                RoutineFate::Move(to) => format!("moved to {to}"),
                RoutineFate::Delete => "deleted".to_owned(),
            },
            "memories_deleted": memories,
        }));
        crate::ledger_repo::append_on(&tx, &e)?;
        tx.commit()?;
        Ok(true)
    }

    // ---- channels ----

    /// What topic channel `id` holds.
    pub fn channel_use(&self, id: &str) -> Result<ChannelUse> {
        let conn = self.conn()?;
        let count = |sql: &str| -> Result<usize> {
            let n: i64 = conn.query_row(sql, [id], |r| r.get(0))?;
            Ok(n.max(0) as usize)
        };
        let mut stmt = conn.prepare("SELECT id, prompt FROM schedules WHERE channel_id = ?1")?;
        let rows = stmt.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let routines = rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(i, p)| (i, first_line(&p)))
            .collect();
        Ok(ChannelUse {
            posts: count("SELECT count(*) FROM posts WHERE channel_id = ?1")?,
            conversations: count(
                "SELECT count(*) FROM sessions WHERE channel_id = ?1 AND source != 'subagent'",
            )?,
            routines,
            members: count("SELECT count(*) FROM channel_agents WHERE channel_id = ?1")?,
            running_runs: count(
                "SELECT count(*) FROM runs r JOIN sessions s ON s.id = r.session_id
                 WHERE r.status = 'running' AND s.channel_id = ?1",
            )?,
        })
    }

    /// Give topic channel `id` a new slug (its `#name`). The caller has
    /// checked the slug's form and that it is free; this refuses Home and
    /// DMs, whose slugs follow their agent. False if there is no such
    /// channel.
    pub fn set_channel_slug(&self, id: &str, slug: &str) -> Result<bool> {
        let conn = self.conn()?;
        let kind: Option<String> = conn
            .query_row("SELECT kind FROM channels WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        match kind.as_deref() {
            None => Ok(false),
            Some("topic") => {
                conn.execute(
                    "UPDATE channels SET slug = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, slug, now_ms()],
                )?;
                Ok(true)
            }
            Some(_) => Err(StoreError::Invalid(
                "Home and DMs keep their names: rename the agent instead".into(),
            )),
        }
    }

    /// Delete topic channel `id`: its posts (and the pointers to them in
    /// Home), its member rows and its slug go. Its conversations stay,
    /// moved under Home, so History still shows them; routines that posted
    /// there post to their agent's DM. False if there is no such channel;
    /// Home and DMs can't be deleted.
    pub fn delete_channel(&self, id: &str) -> Result<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT kind, slug FROM channels WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((kind, slug)) = row else {
            return Ok(false);
        };
        if kind != "topic" {
            return Err(StoreError::Invalid(
                "Home and DMs can't be deleted; a DM goes with its agent".into(),
            ));
        }
        let (posts, conversations): (i64, i64) = (
            tx.query_row(
                "SELECT count(*) FROM posts WHERE channel_id = ?1",
                [id],
                |r| r.get(0),
            )?,
            tx.query_row(
                "SELECT count(*) FROM sessions WHERE channel_id = ?1 AND source != 'subagent'",
                [id],
                |r| r.get(0),
            )?,
        );
        // The posts to delete: the channel's, and Home's pointers to them.
        let doomed = "SELECT id FROM posts WHERE channel_id = ?1
                      UNION SELECT id FROM posts WHERE points_to IN
                        (SELECT id FROM posts WHERE channel_id = ?1)";
        tx.execute(
            &format!("UPDATE runs SET woken_by = NULL WHERE woken_by IN ({doomed})"),
            [id],
        )?;
        tx.execute(
            &format!(
                "UPDATE posts SET reply_to = NULL WHERE reply_to IN ({doomed})
                   AND id NOT IN ({doomed})"
            ),
            [id],
        )?;
        tx.execute(
            &format!("DELETE FROM posts WHERE id IN ({doomed})"),
            [id],
        )?;
        tx.execute("DELETE FROM channel_agents WHERE channel_id = ?1", [id])?;
        tx.execute(
            "UPDATE sessions SET channel_id = 'home' WHERE channel_id = ?1",
            [id],
        )?;
        tx.execute(
            "UPDATE schedules SET channel_id = COALESCE(
               (SELECT c.id FROM channels c WHERE c.agent_id = COALESCE(schedules.agent_id, 'tern')
                  AND c.kind = 'main' ORDER BY c.created_at LIMIT 1), 'home')
             WHERE channel_id = ?1",
            [id],
        )?;
        tx.execute(
            "UPDATE channel_suggestions SET channel_id = NULL WHERE channel_id = ?1",
            [id],
        )?;
        tx.execute("DELETE FROM channels WHERE id = ?1", [id])?;
        let e = LedgerEntry::new(
            "channel.delete",
            id,
            format!("deleted channel #{slug}; its conversations stay in History"),
        )
        .channel(id)
        .detail(serde_json::json!({ "posts": posts, "conversations": conversations }));
        crate::ledger_repo::append_on(&tx, &e)?;
        tx.commit()?;
        Ok(true)
    }
}
