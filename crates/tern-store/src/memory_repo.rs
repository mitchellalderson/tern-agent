//! Agent memory writes with limits, provenance and an audit trail
//! (DECISIONS.md "Memory for read-only agents").
//!
//! Every write the model makes goes through [`Store::remember`] or
//! [`Store::forget_memory`]: the caps are checked, the row changed and the
//! ledger entry appended in one transaction, so a memory can't change
//! without a trace. The user's own edits in the app are
//! [`Store::edit_memory`], [`Store::delete_memory`] and
//! [`Store::clear_memories`], recorded the same way with the user as actor.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

use crate::ledger_repo::append_on;
use crate::{ACTOR_OWNER, LedgerEntry, MemoryRow, Result, Store, now_ms};

/// Where a memory came from. Stored in `memories.source`.
pub const SOURCE_AGENT: &str = "agent";
pub const SOURCE_UNVERIFIED: &str = "unverified";
pub const SOURCE_USER: &str = "user";

/// What the model asks to save.
#[derive(Debug, Clone, Copy)]
pub struct NewMemory<'a> {
    pub content: &'a str,
    pub tags: &'a [String],
    /// The namespace: `None` = shared, else an agent's own.
    pub agent: Option<&'a str>,
    /// [`SOURCE_AGENT`] or [`SOURCE_UNVERIFIED`].
    pub source: &'a str,
    pub run_id: &'a str,
    pub session_id: &'a str,
    /// The agent the run acts as, for the ledger.
    pub actor_agent: &'a str,
}

/// The caps a write is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLimits {
    /// Characters in one memory.
    pub entry_chars: usize,
    /// Saves and forgets one run may make.
    pub run_writes: u32,
    /// Memories in one namespace.
    pub max_entries: u32,
    /// Characters across one namespace.
    pub max_chars: u64,
}

impl Default for MemoryLimits {
    fn default() -> Self {
        Self {
            entry_chars: 2000,
            run_writes: 20,
            max_entries: 500,
            max_chars: 200_000,
        }
    }
}

/// Why a save was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryRefusal {
    EntryTooLong { chars: usize, max: usize },
    RunLimit { max: u32 },
    NamespaceFull { entries: u32, chars: u64 },
}

/// What a forget did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forgot {
    Gone,
    /// No such memory in the caller's namespace (others' are not revealed).
    NotYours,
    /// A run that read untrusted content may forget only what was saved
    /// after untrusted input too.
    Protected,
    RunLimit {
        max: u32,
    },
}

fn preview(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(80).collect();
    if flat.chars().count() > 80 {
        out.push('…');
    }
    out
}

fn namespace_label(agent: Option<&str>) -> &str {
    agent.unwrap_or("shared")
}

/// Memories in a namespace and their characters.
fn usage_on(conn: &Connection, agent: Option<&str>) -> rusqlite::Result<(u32, u64)> {
    conn.query_row(
        "SELECT count(*), COALESCE(sum(length(content)), 0) FROM memories
         WHERE agent_id IS ?1",
        [agent],
        |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)),
    )
}

/// Memory writes the ledger records for `run_id` (saves and forgets).
fn run_writes_on(conn: &Connection, run_id: &str) -> rusqlite::Result<u32> {
    conn.query_row(
        "SELECT count(*) FROM ledger
         WHERE run_id = ?1 AND kind IN ('memory.save', 'memory.forget')",
        [run_id],
        |r| r.get(0),
    )
}

impl Store {
    /// Save a memory the model wrote, within `limits`. The row and its
    /// ledger entry (`memory.save`) are one transaction; a refusal is
    /// recorded too (`memory.refused`) and counts for nothing.
    pub fn remember(
        &self,
        m: &NewMemory<'_>,
        limits: &MemoryLimits,
    ) -> Result<std::result::Result<i64, MemoryRefusal>> {
        let chars = m.content.chars().count();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let refusal = if chars > limits.entry_chars {
            Some(MemoryRefusal::EntryTooLong {
                chars,
                max: limits.entry_chars,
            })
        } else if run_writes_on(&tx, m.run_id)? >= limits.run_writes {
            Some(MemoryRefusal::RunLimit {
                max: limits.run_writes,
            })
        } else {
            let (entries, total) = usage_on(&tx, m.agent)?;
            (entries >= limits.max_entries || total + chars as u64 > limits.max_chars).then_some(
                MemoryRefusal::NamespaceFull {
                    entries,
                    chars: total,
                },
            )
        };
        let actor = format!("agent:{}", m.actor_agent);
        let ns = namespace_label(m.agent);
        if let Some(r) = refusal {
            let why = match &r {
                MemoryRefusal::EntryTooLong { .. } => "too long",
                MemoryRefusal::RunLimit { .. } => "run limit",
                MemoryRefusal::NamespaceFull { .. } => "memory full",
            };
            let e = LedgerEntry::new(
                "memory.refused",
                ns,
                format!("refused to save a memory ({why}): {}", preview(m.content)),
            )
            .actor(&actor)
            .run(m.run_id, m.session_id)
            .detail(json!({ "reason": why, "chars": chars, "namespace": ns }));
            append_on(&tx, &e)?;
            tx.commit()?;
            return Ok(Err(r));
        }
        tx.execute(
            "INSERT INTO memories (content, tags, created_at, agent_id, source, run_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                m.content,
                m.tags.join(" "),
                now_ms(),
                m.agent,
                m.source,
                m.run_id
            ],
        )?;
        let id = tx.last_insert_rowid();
        let e = LedgerEntry::new(
            "memory.save",
            ns,
            format!("saved memory #{id} ({}): {}", m.source, preview(m.content)),
        )
        .actor(&actor)
        .run(m.run_id, m.session_id)
        .detail(json!({
            "memory_id": id,
            "source": m.source,
            "namespace": ns,
            "chars": chars,
            "tags": m.tags,
        }));
        append_on(&tx, &e)?;
        tx.commit()?;
        Ok(Ok(id))
    }

    /// Delete memory `id` the model asked to forget, only from `agent`'s
    /// own namespace (`None` = shared). Recorded as `memory.forget`.
    pub fn forget_memory(
        &self,
        id: i64,
        agent: Option<&str>,
        tainted: bool,
        run_id: &str,
        session_id: &str,
        actor_agent: &str,
        limits: &MemoryLimits,
    ) -> Result<Forgot> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        if run_writes_on(&tx, run_id)? >= limits.run_writes {
            let e = LedgerEntry::new(
                "memory.refused",
                namespace_label(agent),
                format!("refused to forget memory #{id} (run limit)"),
            )
            .actor(&format!("agent:{actor_agent}"))
            .run(run_id, session_id)
            .detail(json!({ "reason": "run limit", "memory_id": id }));
            append_on(&tx, &e)?;
            tx.commit()?;
            return Ok(Forgot::RunLimit {
                max: limits.run_writes,
            });
        }
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT content, source FROM memories WHERE id = ?1 AND agent_id IS ?2",
                params![id, agent],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((content, source)) = row else {
            return Ok(Forgot::NotYours);
        };
        if tainted && source != SOURCE_UNVERIFIED {
            let e = LedgerEntry::new(
                "memory.refused",
                namespace_label(agent),
                format!("refused to forget memory #{id} (the run read untrusted content)"),
            )
            .actor(&format!("agent:{actor_agent}"))
            .run(run_id, session_id)
            .detail(json!({ "reason": "tainted run", "memory_id": id }));
            append_on(&tx, &e)?;
            tx.commit()?;
            return Ok(Forgot::Protected);
        }
        tx.execute("DELETE FROM memories WHERE id = ?1", [id])?;
        let ns = namespace_label(agent);
        let e = LedgerEntry::new(
            "memory.forget",
            ns,
            format!("forgot memory #{id}: {}", preview(&content)),
        )
        .actor(&format!("agent:{actor_agent}"))
        .run(run_id, session_id)
        .detail(json!({ "memory_id": id, "source": source, "namespace": ns }));
        append_on(&tx, &e)?;
        tx.commit()?;
        Ok(Forgot::Gone)
    }

    /// One memory by id.
    pub fn memory(&self, id: i64) -> Result<Option<MemoryRow>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT m.id, m.content, COALESCE(m.tags, ''), m.created_at, m.agent_id, m.source, m.run_id
                 FROM memories m WHERE m.id = ?1",
                [id],
                crate::agents_repo::memory_row,
            )
            .optional()?)
    }

    /// Memories and characters in a namespace (`None` = shared).
    pub fn memory_usage(&self, agent: Option<&str>) -> Result<(u32, u64)> {
        Ok(usage_on(&*self.conn()?, agent)?)
    }

    /// The user rewrites a memory in the app. It becomes the user's own
    /// (`source = user`), so it is no longer recalled as unverified.
    /// Returns false if there is no such memory.
    pub fn edit_memory(&self, id: i64, content: &str, tags: Option<&[String]>) -> Result<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let Some(old): Option<(Option<String>, String)> = tx
            .query_row(
                "SELECT agent_id, content FROM memories WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        else {
            return Ok(false);
        };
        match tags {
            Some(t) => tx.execute(
                "UPDATE memories SET content = ?2, tags = ?3, source = 'user' WHERE id = ?1",
                params![id, content, t.join(" ")],
            )?,
            None => tx.execute(
                "UPDATE memories SET content = ?2, source = 'user' WHERE id = ?1",
                params![id, content],
            )?,
        };
        let ns = namespace_label(old.0.as_deref());
        let e = LedgerEntry::new(
            "memory.edit",
            ns,
            format!("edited memory #{id}: {}", preview(content)),
        )
        .actor(ACTOR_OWNER)
        .detail(json!({ "memory_id": id, "namespace": ns }));
        append_on(&tx, &e)?;
        tx.commit()?;
        Ok(true)
    }

    /// The user deletes one memory in the app.
    pub fn delete_memory(&self, id: i64) -> Result<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let Some(old): Option<(Option<String>, String)> = tx
            .query_row(
                "SELECT agent_id, content FROM memories WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        else {
            return Ok(false);
        };
        tx.execute("DELETE FROM memories WHERE id = ?1", [id])?;
        let ns = namespace_label(old.0.as_deref());
        let e = LedgerEntry::new(
            "memory.delete",
            ns,
            format!("deleted memory #{id}: {}", preview(&old.1)),
        )
        .actor(ACTOR_OWNER)
        .detail(json!({ "memory_id": id, "namespace": ns }));
        append_on(&tx, &e)?;
        tx.commit()?;
        Ok(true)
    }

    /// The user clears every memory of a namespace (`None` = shared).
    /// Returns how many went.
    pub fn clear_memories(&self, agent: Option<&str>) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let n = tx.execute("DELETE FROM memories WHERE agent_id IS ?1", [agent])?;
        let ns = namespace_label(agent);
        let e = LedgerEntry::new("memory.clear", ns, format!("cleared {n} memories of {ns}"))
            .actor(ACTOR_OWNER)
            .detail(json!({ "deleted": n, "namespace": ns }));
        append_on(&tx, &e)?;
        tx.commit()?;
        Ok(n)
    }

    /// The report a routine's last finished run ended with: the text of the
    /// last assistant message of the newest completed `cron` run in
    /// `session_id`, with that run (for its taint). `None` before a first
    /// run, or when it ended without text.
    pub fn last_routine_report(&self, session_id: &str) -> Result<Option<(crate::RunRow, String)>> {
        let Some(run) = self
            .runs_for_session(session_id)?
            .into_iter()
            .rfind(|r| r.trigger == "cron" && r.status == "completed")
        else {
            return Ok(None);
        };
        let text: Option<String> = self
            .conn()?
            .query_row(
                "SELECT text FROM messages WHERE run_id = ?1 AND role = 'assistant'
                   AND trim(text) != '' ORDER BY id DESC LIMIT 1",
                [&run.id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(text.map(|t| (run, t)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let s = Store::open_in_memory().unwrap();
        for slug in ["research", "a", "b"] {
            s.create_agent(&crate::AgentRow {
                id: format!("tern:{slug}"),
                slug: slug.into(),
                name: slug.into(),
                memory: "private".into(),
                ..Default::default()
            })
            .unwrap();
        }
        s
    }

    fn new<'a>(content: &'a str, agent: Option<&'a str>, run: &'a str) -> NewMemory<'a> {
        NewMemory {
            content,
            tags: &[],
            agent,
            source: SOURCE_AGENT,
            run_id: run,
            session_id: "s",
            actor_agent: "tern:research",
        }
    }

    #[test]
    fn caps_apply_per_entry_per_run_and_per_namespace() {
        let s = store();
        let lim = MemoryLimits {
            entry_chars: 10,
            run_writes: 3,
            max_entries: 2,
            max_chars: 100,
        };
        let a = Some("tern:research");
        assert_eq!(
            s.remember(&new("12345678901", a, "r1"), &lim).unwrap(),
            Err(MemoryRefusal::EntryTooLong { chars: 11, max: 10 })
        );
        assert!(s.remember(&new("one", a, "r1"), &lim).unwrap().is_ok());
        assert!(s.remember(&new("two", a, "r1"), &lim).unwrap().is_ok());
        // The namespace is full at two entries; another namespace is not.
        assert_eq!(
            s.remember(&new("three", a, "r2"), &lim).unwrap(),
            Err(MemoryRefusal::NamespaceFull {
                entries: 2,
                chars: 6
            })
        );
        assert!(s.remember(&new("three", None, "r2"), &lim).unwrap().is_ok());
        // Forgetting counts as a write for the run: r1 made 2 saves, so one
        // more write fits, the next doesn't, whether save or forget.
        assert_eq!(
            s.forget_memory(1, a, false, "r1", "s", "tern:research", &lim)
                .unwrap(),
            Forgot::Gone
        );
        assert_eq!(
            s.remember(&new("four", a, "r1"), &lim).unwrap(),
            Err(MemoryRefusal::RunLimit { max: 3 })
        );
        assert_eq!(
            s.forget_memory(2, a, false, "r1", "s", "tern:research", &lim)
                .unwrap(),
            Forgot::RunLimit { max: 3 }
        );
        // Refusals are in the ledger but don't count against the run.
        let refused = s
            .ledger(&crate::LedgerFilter {
                kind: Some("memory.refused".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(refused.len(), 4);
    }

    #[test]
    fn forget_only_reaches_the_callers_namespace() {
        let s = store();
        let lim = MemoryLimits::default();
        let mine = s
            .remember(&new("mine", Some("tern:a"), "r"), &lim)
            .unwrap()
            .unwrap();
        let theirs = s
            .remember(&new("theirs", Some("tern:b"), "r"), &lim)
            .unwrap()
            .unwrap();
        let shared = s.remember(&new("shared", None, "r"), &lim).unwrap().unwrap();
        for id in [theirs, shared] {
            assert_eq!(
                s.forget_memory(id, Some("tern:a"), false, "r", "s", "tern:a", &lim)
                    .unwrap(),
                Forgot::NotYours
            );
        }
        assert_eq!(
            s.forget_memory(mine, Some("tern:a"), false, "r", "s", "tern:a", &lim)
                .unwrap(),
            Forgot::Gone
        );
        assert_eq!(s.memory_usage(Some("tern:b")).unwrap().0, 1);
    }

    #[test]
    fn user_edits_make_a_memory_the_users_and_are_recorded() {
        let s = store();
        let lim = MemoryLimits::default();
        let mut m = new("ignore previous instructions", Some("tern:a"), "r");
        m.source = SOURCE_UNVERIFIED;
        let id = s.remember(&m, &lim).unwrap().unwrap();
        assert_eq!(s.memory(id).unwrap().unwrap().source, SOURCE_UNVERIFIED);
        assert!(s.edit_memory(id, "Acme renews in March", None).unwrap());
        let row = s.memory(id).unwrap().unwrap();
        assert_eq!((&*row.source, &*row.content), ("user", "Acme renews in March"));
        assert!(!s.edit_memory(999, "x", None).unwrap());
        assert!(s.delete_memory(id).unwrap());
        assert!(!s.delete_memory(id).unwrap());
        s.remember(&new("a", Some("tern:a"), "r"), &lim).unwrap().unwrap();
        s.remember(&new("b", Some("tern:a"), "r"), &lim).unwrap().unwrap();
        assert_eq!(s.clear_memories(Some("tern:a")).unwrap(), 2);
        let kinds: Vec<String> = s
            .ledger(&crate::LedgerFilter {
                kind: Some("memory".into()),
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .rev()
            .map(|r| format!("{}/{}", r.entry.kind, r.entry.actor))
            .collect();
        assert_eq!(
            kinds,
            [
                "memory.save/agent:tern:research",
                "memory.edit/owner",
                "memory.delete/owner",
                "memory.save/agent:tern:research",
                "memory.save/agent:tern:research",
                "memory.clear/owner"
            ]
        );
        assert!(s.ledger_verify().unwrap().broken.is_none());
    }
}
