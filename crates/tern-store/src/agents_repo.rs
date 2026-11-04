//! Agent profiles (docs/agent-profiles-design.md §2): the `agents` table,
//! each agent's DM channel, and who owns sessions, schedules and memories.

use rusqlite::{OptionalExtension, Row, params};

use crate::{MemoryRow, Result, Store, fts_query, now_ms};

/// The default agent's id (and slug): today's setup, with Home as its DM.
pub const DEFAULT_AGENT: &str = "tern";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentRow {
    /// `tern`, or `tern:<slug at creation>`; never changes.
    pub id: String,
    pub slug: String,
    pub name: String,
    /// JSON `{glyph, color}`; `None` = generated from the id.
    pub avatar: Option<String>,
    /// A model chain: `provider:model` or a JSON list; `None` = `default_model`.
    pub model: Option<String>,
    pub subagent_model: Option<String>,
    /// JSON tool scope; `None` = every tool.
    pub tools: Option<String>,
    /// `shared` or `private`.
    pub memory: String,
    /// JSON list of agent ids it may delegate to (AP3).
    pub delegates: Option<String>,
    /// The tainted host limit for its runs: `ask` or `allow`; `None` = the
    /// configured `[taint] egress`.
    pub egress: Option<String>,
    /// The most autonomy its runs may have: `readonly`, `supervised`,
    /// `autonomous` or `yolo`; `None` = no ceiling beyond the configured one.
    pub autonomy: Option<String>,
    /// JSON `{bot_token_secret, api_url?}`: a Telegram bot of its own
    /// (AP3); `None` = it is reached through the main bot's `/agent`.
    pub telegram: Option<String>,
    pub archived_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Set when the agent was deleted: the row stays so that ids in
    /// history still resolve, but nothing else sees it.
    pub deleted_at: Option<i64>,
}

impl AgentRow {
    pub fn is_default(&self) -> bool {
        self.id == DEFAULT_AGENT
    }

    pub fn private_memory(&self) -> bool {
        self.memory == "private"
    }

    /// The agents this one lists in `delegates`; `None` when unset.
    pub fn delegate_list(&self) -> Option<Vec<String>> {
        self.delegates
            .as_deref()
            .and_then(|d| serde_json::from_str(d).ok())
    }

    /// Whether this agent may hand work to `target` with `agent_spawn
    /// {agent}` (agent profiles §6.1): an active other agent its
    /// `delegates` lists (by id or slug). Unset, the default agent may
    /// delegate to every agent and others to none.
    pub fn may_delegate_to(&self, target: &AgentRow) -> bool {
        if target.id == self.id || target.archived_at.is_some() {
            return false;
        }
        match self.delegate_list() {
            Some(list) => list.iter().any(|x| *x == target.id || *x == target.slug),
            None => self.is_default(),
        }
    }
}

/// Changes to an agent; `None` leaves a field, `Some(None)` clears it.
#[derive(Debug, Clone, Default)]
pub struct AgentChange {
    pub slug: Option<String>,
    pub name: Option<String>,
    pub avatar: Option<Option<String>>,
    pub model: Option<Option<String>>,
    pub subagent_model: Option<Option<String>>,
    pub tools: Option<Option<String>>,
    pub memory: Option<String>,
    pub delegates: Option<Option<String>>,
    pub egress: Option<Option<String>>,
    pub autonomy: Option<Option<String>>,
    pub telegram: Option<Option<String>>,
    pub archived: Option<bool>,
}

const AGENT_COLS: &str = "id, slug, name, avatar, model, subagent_model, tools, memory, delegates, \
    archived_at, created_at, updated_at, egress, autonomy, telegram, deleted_at";

fn agent_row(r: &Row<'_>) -> rusqlite::Result<AgentRow> {
    Ok(AgentRow {
        id: r.get(0)?,
        slug: r.get(1)?,
        name: r.get(2)?,
        avatar: r.get(3)?,
        model: r.get(4)?,
        subagent_model: r.get(5)?,
        tools: r.get(6)?,
        memory: r.get(7)?,
        delegates: r.get(8)?,
        archived_at: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        egress: r.get(12)?,
        autonomy: r.get(13)?,
        telegram: r.get(14)?,
        deleted_at: r.get(15)?,
    })
}

/// The DM channel's slug for an agent slug.
pub fn dm_slug(agent_slug: &str) -> String {
    format!("dm-{agent_slug}")
}

impl Store {
    /// Create an agent and its DM channel (`dm-<slug>`, kind `main`) in
    /// one transaction. Returns the agent and the channel id.
    pub fn create_agent(&self, a: &AgentRow) -> Result<(AgentRow, String)> {
        let now = now_ms();
        let channel_id = format!("dm:{}", a.id.strip_prefix("tern:").unwrap_or(&a.id));
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            &format!(
                "INSERT INTO agents ({AGENT_COLS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?10, ?11, ?12, ?13, NULL)"
            ),
            params![
                a.id,
                a.slug,
                a.name,
                a.avatar,
                a.model,
                a.subagent_model,
                a.tools,
                a.memory,
                a.delegates,
                now,
                a.egress,
                a.autonomy,
                a.telegram
            ],
        )?;
        tx.execute(
            "INSERT INTO channels (id, slug, title, kind, created_by, created_at, updated_at, agent_id)
             VALUES (?1, ?2, ?3, 'main', 'user', ?4, ?4, ?5)",
            params![channel_id, dm_slug(&a.slug), a.name, now, a.id],
        )?;
        tx.commit()?;
        Ok((
            AgentRow {
                created_at: now,
                updated_at: now,
                archived_at: None,
                ..a.clone()
            },
            channel_id,
        ))
    }

    /// Agents, the default first, then by name; archived ones only if asked.
    pub fn agents(&self, include_archived: bool) -> Result<Vec<AgentRow>> {
        let sql = format!(
            "SELECT {AGENT_COLS} FROM agents WHERE (?1 OR archived_at IS NULL) AND deleted_at IS NULL
             ORDER BY id = 'tern' DESC, name COLLATE NOCASE"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([include_archived], agent_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// An agent by id or slug. Deleted agents aren't found; see
    /// [`Self::agent_or_deleted`].
    pub fn agent(&self, id_or_slug: &str) -> Result<Option<AgentRow>> {
        let sql = format!(
            "SELECT {AGENT_COLS} FROM agents WHERE (id = ?1 OR slug = ?1) AND deleted_at IS NULL"
        );
        Ok(self
            .conn()?
            .query_row(&sql, [id_or_slug], agent_row)
            .optional()?)
    }

    /// An agent by id, deleted ones included: for showing who wrote
    /// something in history.
    pub fn agent_or_deleted(&self, id: &str) -> Result<Option<AgentRow>> {
        let sql = format!("SELECT {AGENT_COLS} FROM agents WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], agent_row).optional()?)
    }

    /// Every deleted agent, newest first: history resolves their ids to
    /// names with these.
    pub fn deleted_agents(&self) -> Result<Vec<AgentRow>> {
        let sql = format!(
            "SELECT {AGENT_COLS} FROM agents WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], agent_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Apply `ch` to agent `id`; a new slug renames its DM channel too, a
    /// new name retitles it. Archiving pauses its schedules and archives
    /// its DM; unarchiving brings the DM back (schedules stay paused).
    /// False if there is no such agent.
    pub fn update_agent(&self, id: &str, ch: &AgentChange) -> Result<bool> {
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE agents SET updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        if n == 0 {
            return Ok(false);
        }
        let set = |col: &str, v: Option<&str>| {
            tx.execute(
                &format!("UPDATE agents SET {col} = ?2 WHERE id = ?1"),
                params![id, v],
            )
        };
        if let Some(s) = &ch.slug {
            set("slug", Some(s))?;
            tx.execute(
                "UPDATE channels SET slug = ?2, updated_at = ?3 WHERE agent_id = ?1 AND kind = 'main'
                   AND id != 'home'",
                params![id, dm_slug(s), now],
            )?;
        }
        if let Some(name) = &ch.name {
            set("name", Some(name))?;
            tx.execute(
                "UPDATE channels SET title = ?2, updated_at = ?3 WHERE agent_id = ?1 AND kind = 'main'
                   AND id != 'home'",
                params![id, name, now],
            )?;
        }
        if let Some(v) = &ch.avatar {
            set("avatar", v.as_deref())?;
        }
        if let Some(v) = &ch.model {
            set("model", v.as_deref())?;
        }
        if let Some(v) = &ch.subagent_model {
            set("subagent_model", v.as_deref())?;
        }
        if let Some(v) = &ch.tools {
            set("tools", v.as_deref())?;
            let e = crate::LedgerEntry::new(
                "agent.tools",
                id,
                format!("agent {id}: tools {}", v.as_deref().unwrap_or("all")),
            )
            .detail(serde_json::json!({ "value": v }));
            crate::ledger_repo::append_on(&tx, &e)?;
        }
        if let Some(v) = &ch.memory {
            set("memory", Some(v))?;
        }
        if let Some(v) = &ch.delegates {
            set("delegates", v.as_deref())?;
        }
        if let Some(v) = &ch.egress {
            set("egress", v.as_deref())?;
            let e = crate::LedgerEntry::new(
                "egress.agent",
                id,
                format!(
                    "agent {id}: new sites in a tainted run {}",
                    match v.as_deref() {
                        Some("allow") => "are allowed",
                        Some(_) => "ask",
                        None => "follow the configured setting",
                    }
                ),
            )
            .detail(serde_json::json!({ "value": v }));
            crate::ledger_repo::append_on(&tx, &e)?;
        }
        if let Some(v) = &ch.autonomy {
            set("autonomy", v.as_deref())?;
            let e = crate::LedgerEntry::new(
                "autonomy.agent",
                id,
                format!(
                    "agent {id}: autonomy ceiling {}",
                    v.as_deref().unwrap_or("none (the configured autonomy)")
                ),
            )
            .detail(serde_json::json!({ "value": v }));
            crate::ledger_repo::append_on(&tx, &e)?;
        }
        if let Some(v) = &ch.telegram {
            set("telegram", v.as_deref())?;
            // The secret's name only, never a token.
            let e = crate::LedgerEntry::new(
                "telegram.agent",
                id,
                format!(
                    "agent {id}: telegram bot {}",
                    match v.as_deref() {
                        Some(_) => "set (takes effect when the daemon restarts)",
                        None => "removed",
                    }
                ),
            )
            .detail(serde_json::json!({ "value": v }));
            crate::ledger_repo::append_on(&tx, &e)?;
        }
        if let Some(a) = ch.archived {
            tx.execute(
                "UPDATE agents SET archived_at = ?2 WHERE id = ?1",
                params![id, a.then_some(now)],
            )?;
            tx.execute(
                "UPDATE channels SET archived_at = ?2, updated_at = ?3
                 WHERE agent_id = ?1 AND kind = 'main' AND id != 'home'",
                params![id, a.then_some(now), now],
            )?;
            if a {
                tx.execute("UPDATE schedules SET enabled = 0 WHERE agent_id = ?1", [id])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// An agent's DM: Home for the default agent.
    pub fn agent_channel(&self, agent_id: &str) -> Result<Option<crate::ChannelRow>> {
        if agent_id == DEFAULT_AGENT {
            return self.channel(crate::HOME);
        }
        let id: Option<String> = self
            .conn()?
            .query_row(
                "SELECT id FROM channels WHERE agent_id = ?1 AND kind = 'main' ORDER BY created_at LIMIT 1",
                [agent_id],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.channel(&id),
            None => Ok(None),
        }
    }

    /// The agent whose thread `session_id` is (the default agent for rows
    /// from before profiles, or an unknown session).
    pub fn session_agent(&self, session_id: &str) -> Result<String> {
        let id: Option<Option<String>> = self
            .conn()?
            .query_row(
                "SELECT agent_id FROM sessions WHERE id = ?1",
                [session_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.flatten().unwrap_or_else(|| DEFAULT_AGENT.into()))
    }

    pub fn set_session_agent(&self, session_id: &str, agent_id: &str) -> Result<()> {
        let v = (agent_id != DEFAULT_AGENT).then_some(agent_id);
        self.conn()?.execute(
            "UPDATE sessions SET agent_id = ?2 WHERE id = ?1",
            params![session_id, v],
        )?;
        Ok(())
    }

    /// The agent a run acts as, and whether its memory is private (frozen
    /// on the run when it started).
    pub fn run_agent(&self, run_id: &str) -> Result<(String, bool)> {
        let row: Option<(Option<String>, Option<String>)> = self
            .conn()?
            .query_row(
                "SELECT s.agent_id, r.memory_scope FROM runs r JOIN sessions s ON s.id = r.session_id
                 WHERE r.id = ?1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(match row {
            Some((agent, scope)) => (
                agent.unwrap_or_else(|| DEFAULT_AGENT.into()),
                scope.as_deref() == Some("private"),
            ),
            None => (DEFAULT_AGENT.into(), false),
        })
    }

    /// Schedules owned by `agent_id` (`tern` includes rows from before
    /// profiles).
    pub fn agent_schedule_ids(&self, agent_id: &str) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM schedules WHERE COALESCE(agent_id, 'tern') = ?1 ORDER BY next_run_at",
        )?;
        let rows = stmt.query_map([agent_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Who owns a schedule, and its session follows: a scheduled run acts as
    /// the agent of its session.
    pub fn set_schedule_agent(&self, schedule_id: &str, agent_id: &str) -> Result<()> {
        let v = (agent_id != DEFAULT_AGENT).then_some(agent_id);
        let conn = self.conn()?;
        conn.execute(
            "UPDATE schedules SET agent_id = ?2 WHERE id = ?1",
            params![schedule_id, v],
        )?;
        conn.execute(
            "UPDATE sessions SET agent_id = ?2 WHERE id = (SELECT session_id FROM schedules WHERE id = ?1)",
            params![schedule_id, v],
        )?;
        Ok(())
    }

    /// Top-level sessions of an agent's, newest first, with last activity.
    pub fn agent_last_activity(&self, agent_id: &str) -> Result<Option<i64>> {
        Ok(self.conn()?.query_row(
            "SELECT max(updated_at) FROM sessions WHERE COALESCE(agent_id, 'tern') = ?1",
            [agent_id],
            |r| r.get(0),
        )?)
    }

    // ---- scoped memories (§1.3) ----

    /// Save a memory: shared (`agent = None`) or an agent's own.
    pub fn save_agent_memory(
        &self,
        content: &str,
        tags: &[String],
        agent: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO memories (content, tags, created_at, agent_id) VALUES (?1, ?2, ?3, ?4)",
            params![content, tags.join(" "), now_ms(), agent],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Search shared memories, plus `agent`'s own when given.
    pub fn search_agent_memories(
        &self,
        query: &str,
        limit: usize,
        agent: Option<&str>,
    ) -> Result<Vec<MemoryRow>> {
        let Some(q) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.content, COALESCE(m.tags, ''), m.created_at, m.agent_id
             FROM memories_fts f JOIN memories m ON m.id = f.rowid
             WHERE memories_fts MATCH ?1 AND (m.agent_id IS NULL OR m.agent_id = ?3)
             ORDER BY bm25(memories_fts) LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![q, limit as i64, agent], memory_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Memories for the app's Memory view, newest first: `scope` is
    /// `shared`, an agent id (its own), or `None` for all. `query` searches.
    pub fn list_memories(
        &self,
        scope: Option<&str>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryRow>> {
        let conn = self.conn()?;
        let filter = "(?1 IS NULL OR (?1 = 'shared' AND m.agent_id IS NULL) OR m.agent_id = ?1)";
        let rows = match query.and_then(fts_query) {
            Some(q) => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT m.id, m.content, COALESCE(m.tags, ''), m.created_at, m.agent_id
                     FROM memories_fts f JOIN memories m ON m.id = f.rowid
                     WHERE memories_fts MATCH ?2 AND {filter}
                     ORDER BY bm25(memories_fts) LIMIT ?3"
                ))?;
                stmt.query_map(params![scope, q, limit as i64], memory_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT m.id, m.content, COALESCE(m.tags, ''), m.created_at, m.agent_id
                     FROM memories m WHERE {filter} ORDER BY m.id DESC LIMIT ?2"
                ))?;
                stmt.query_map(params![scope, limit as i64], memory_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    /// Full-text search over messages of `agent_id`'s sessions except
    /// `exclude_session`: never another agent's DM (§1.3).
    pub fn search_agent_messages(
        &self,
        query: &str,
        limit: usize,
        exclude_session: Option<&str>,
        agent_id: &str,
    ) -> Result<Vec<crate::MessageHit>> {
        let Some(q) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, s.title, m.role, m.created_at,
                    snippet(messages_fts, 0, '[', ']', ' … ', 24)
             FROM messages_fts f
             JOIN messages m ON m.id = f.rowid
             JOIN sessions s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1 AND m.session_id IS NOT ?3
               AND COALESCE(s.agent_id, 'tern') = ?4
             ORDER BY bm25(messages_fts) LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![q, limit as i64, exclude_session, agent_id], |r| {
            Ok(crate::MessageHit {
                message_id: r.get(0)?,
                session_id: r.get(1)?,
                session_title: r.get(2)?,
                role: r.get(3)?,
                created_at: r.get(4)?,
                snippet: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn memory_row(r: &Row<'_>) -> rusqlite::Result<MemoryRow> {
    Ok(MemoryRow {
        id: r.get(0)?,
        content: r.get(1)?,
        tags: r.get(2)?,
        created_at: r.get(3)?,
        agent_id: r.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn research(s: &Store) -> (AgentRow, String) {
        s.create_agent(&AgentRow {
            id: "tern:research".into(),
            slug: "research".into(),
            name: "Research".into(),
            memory: "private".into(),
            ..AgentRow::default()
        })
        .unwrap()
    }

    #[test]
    fn tern_may_delegate_to_all_others_only_to_their_list() {
        let s = Store::open_in_memory().unwrap();
        let (research, _) = research(&s);
        let tern = s.agent(DEFAULT_AGENT).unwrap().unwrap();
        assert!(tern.may_delegate_to(&research));
        assert!(!tern.may_delegate_to(&tern), "not to itself");
        assert!(!research.may_delegate_to(&tern), "others: none by default");
        let listed = AgentRow {
            delegates: Some(r#"["tern"]"#.into()),
            ..research.clone()
        };
        assert!(listed.may_delegate_to(&tern));
        let restricted = AgentRow {
            delegates: Some("[]".into()),
            ..tern.clone()
        };
        assert!(!restricted.may_delegate_to(&research));
        let archived = AgentRow {
            archived_at: Some(1),
            ..research
        };
        assert!(!tern.may_delegate_to(&archived));
    }

    #[test]
    fn the_default_agent_exists_and_home_is_its_dm() {
        let s = Store::open_in_memory().unwrap();
        let all = s.agents(false).unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].is_default());
        assert_eq!(all[0].memory, "shared");
        let home = s.agent_channel(DEFAULT_AGENT).unwrap().unwrap();
        assert_eq!(home.id, crate::HOME);
        assert_eq!(home.agent_id.as_deref(), Some(DEFAULT_AGENT));
    }

    #[test]
    fn an_agent_gets_a_dm_and_its_threads_are_its_own() {
        let s = Store::open_in_memory().unwrap();
        let (a, dm) = research(&s);
        let ch = s.channel(&dm).unwrap().unwrap();
        assert_eq!(
            (ch.kind.as_str(), ch.slug.as_str(), ch.title.as_str()),
            ("main", "dm-research", "Research")
        );
        assert_eq!(s.agent_channel(&a.id).unwrap().unwrap().id, dm);
        let t = s
            .create_thread(&crate::NewThread {
                id: "t",
                source: "api",
                channel_id: &dm,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(t.agent(), "tern:research");
        assert_eq!(s.session_agent("t").unwrap(), "tern:research");
        // Home's threads stay NULL, read as tern.
        s.create_session("h", "api", None).unwrap();
        assert_eq!(s.session("h").unwrap().unwrap().agent_id, None);
        assert_eq!(s.session_agent("h").unwrap(), DEFAULT_AGENT);
        // Renaming moves the DM's slug and title; the id stays.
        s.update_agent(
            &a.id,
            &AgentChange {
                slug: Some("scout".into()),
                name: Some("Scout".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let ch = s.channel(&dm).unwrap().unwrap();
        assert_eq!((ch.slug.as_str(), ch.title.as_str()), ("dm-scout", "Scout"));
        assert_eq!(s.agent("scout").unwrap().unwrap().id, "tern:research");
    }

    #[test]
    fn archiving_hides_the_agent_its_dm_and_pauses_its_schedules() {
        let s = Store::open_in_memory().unwrap();
        let (a, dm) = research(&s);
        s.create_session("cron", "cron", None).unwrap();
        s.create_schedule(&crate::ScheduleRow {
            id: "sch".into(),
            cron: "0 8 * * *".into(),
            prompt: "p".into(),
            session_id: "cron".into(),
            enabled: true,
            ..Default::default()
        })
        .unwrap();
        s.set_schedule_agent("sch", &a.id).unwrap();
        assert_eq!(s.session_agent("cron").unwrap(), a.id);
        assert_eq!(s.agent_schedule_ids(&a.id).unwrap(), ["sch"]);
        assert!(s.agent_schedule_ids(DEFAULT_AGENT).unwrap().is_empty());
        s.update_agent(
            &a.id,
            &AgentChange {
                archived: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.agents(false).unwrap().len(), 1);
        assert_eq!(s.agents(true).unwrap().len(), 2);
        assert!(s.channel(&dm).unwrap().unwrap().archived_at.is_some());
        assert!(!s.schedule("sch").unwrap().unwrap().enabled);
    }

    #[test]
    fn private_memories_are_seen_only_by_their_agent() {
        let s = Store::open_in_memory().unwrap();
        research(&s);
        s.save_agent_memory("shared northwind fact", &[], None)
            .unwrap();
        s.save_agent_memory("research northwind note", &[], Some("tern:research"))
            .unwrap();
        let seen = |agent: Option<&str>| -> Vec<String> {
            s.search_agent_memories("northwind", 10, agent)
                .unwrap()
                .into_iter()
                .map(|m| m.content)
                .collect()
        };
        assert_eq!(seen(None), ["shared northwind fact"]);
        assert_eq!(seen(Some("tern:research")).len(), 2);
        assert_eq!(s.list_memories(Some("shared"), None, 10).unwrap().len(), 1);
        assert_eq!(
            s.list_memories(Some("tern:research"), None, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(s.list_memories(None, None, 10).unwrap().len(), 2);
        // The old search (shared view) sees only shared rows.
        assert_eq!(s.search_memories("northwind", 10).unwrap().len(), 1);
    }

    #[test]
    fn a_sub_agent_takes_its_parents_agent_and_memory_scope() {
        let s = Store::open_in_memory().unwrap();
        let (a, dm) = research(&s);
        s.create_thread(&crate::NewThread {
            id: "p",
            source: "api",
            channel_id: &dm,
            ..Default::default()
        })
        .unwrap();
        let msg = crate::NewMessage {
            role: "user".into(),
            content_json: "[]".into(),
            text: "hi".into(),
            summary_of: None,
        };
        s.create_run(
            &crate::NewRun {
                id: "rp",
                session_id: "p",
                trigger: "user",
                model: "m:m",
                system_prompt: "",
                memory_scope: Some("private"),
                ..Default::default()
            },
            &msg,
        )
        .unwrap();
        s.create_session("c", "subagent", None).unwrap();
        s.create_run(
            &crate::NewRun {
                id: "rc",
                session_id: "c",
                trigger: "subagent",
                model: "m:m",
                system_prompt: "",
                parent_run_id: Some("rp"),
                depth: 1,
                ..Default::default()
            },
            &msg,
        )
        .unwrap();
        assert_eq!(s.run_agent("rc").unwrap(), (a.id.clone(), true));
        assert_eq!(
            s.session("c").unwrap().unwrap().channel_id.as_deref(),
            Some(dm.as_str())
        );
    }
}
