//! The weekly memory tidy-up (docs/innbot-learnings.md P9): merged
//! duplicates and flagged contradictions, each logged with the rows as they
//! were, so the desktop can undo it. The store enforces the trust rules, not
//! the caller: an unverified memory never replaces or removes a verified one,
//! and the user's own words are never rewritten.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::ledger_repo::append_on;
use crate::memory_repo::{SOURCE_UNVERIFIED, SOURCE_USER};
use crate::{ACTOR_OWNER, ACTOR_TERN, LedgerEntry, MemoryRow, Result, Store, now_ms};

pub const MERGE: &str = "merge";
pub const CONFLICT: &str = "conflict";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TidyRow {
    pub id: i64,
    pub at: i64,
    /// `merge` or `conflict`.
    pub action: String,
    /// `shared` or an agent id.
    pub namespace: String,
    pub note: String,
    /// JSON: the rows as they were (and, for a merge, what was written).
    pub before: String,
    pub kept_id: Option<i64>,
    /// `applied`, `undone` or `dismissed`.
    pub state: String,
}

/// How an undo ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undo {
    Done,
    /// Already undone or dismissed, or no such entry.
    Nothing,
}

const COLS: &str = "id, at, action, namespace, note, before, kept_id, state";

fn tidy_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TidyRow> {
    Ok(TidyRow {
        id: r.get(0)?,
        at: r.get(1)?,
        action: r.get(2)?,
        namespace: r.get(3)?,
        note: r.get(4)?,
        before: r.get(5)?,
        kept_id: r.get(6)?,
        state: r.get(7)?,
    })
}

fn ns_label(agent: Option<&str>) -> &str {
    agent.unwrap_or("shared")
}

fn mem(conn: &Connection, id: i64) -> rusqlite::Result<Option<MemoryRow>> {
    conn.query_row(
        "SELECT id, content, COALESCE(tags, ''), created_at, agent_id, source, run_id
         FROM memories WHERE id = ?1",
        [id],
        crate::agents_repo::memory_row,
    )
    .optional()
}

fn snapshot(m: &MemoryRow) -> Value {
    json!({
        "id": m.id, "content": m.content, "tags": m.tags, "created_at": m.created_at,
        "agent_id": m.agent_id, "source": m.source, "run_id": m.run_id,
    })
}

/// Trust, low to high.
fn rank(source: &str) -> u8 {
    match source {
        SOURCE_UNVERIFIED => 0,
        SOURCE_USER => 2,
        _ => 1,
    }
}

impl Store {
    /// Every memory of a namespace (`None` = shared), oldest first.
    pub fn memories_in(&self, agent: Option<&str>) -> Result<Vec<MemoryRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, content, COALESCE(tags, ''), created_at, agent_id, source, run_id
             FROM memories WHERE agent_id IS ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([agent], crate::agents_repo::memory_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The namespaces that hold memories: `None` = shared.
    pub fn memory_namespaces(&self) -> Result<Vec<Option<String>>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT DISTINCT agent_id FROM memories ORDER BY agent_id")?;
        let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A cheap fingerprint of a namespace: changes when a memory is added,
    /// removed or rewritten. Equal twice means nothing to tidy.
    pub fn memory_fingerprint(&self, agent: Option<&str>) -> Result<String> {
        let (n, max, len, sum): (i64, i64, i64, i64) = self.conn()?.query_row(
            "SELECT count(*), COALESCE(max(id), 0), COALESCE(sum(length(content)), 0),
                    COALESCE(sum(id * length(content)), 0)
             FROM memories WHERE agent_id IS ?1",
            [agent],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        Ok(format!("{n}:{max}:{len}:{sum}"))
    }

    /// Merge `remove` into `keep` (all in `agent`'s namespace): the others
    /// go, and `keep` takes `text` when that is allowed (every row has the
    /// same source and it isn't the user's). Refused (`None`) when a verified
    /// memory would be lost to an unverified one, or the group is not whole.
    /// One transaction: the rows, the ledger entry and the log entry.
    pub fn tidy_merge(
        &self,
        agent: Option<&str>,
        keep: i64,
        remove: &[i64],
        text: Option<&str>,
        note: &str,
    ) -> Result<Option<i64>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let Some(kept) = mem(&tx, keep)?.filter(|m| m.agent_id.as_deref() == agent) else {
            return Ok(None);
        };
        let mut gone = Vec::new();
        for id in remove {
            match mem(&tx, *id)?.filter(|m| m.agent_id.as_deref() == agent && m.id != keep) {
                Some(m) => gone.push(m),
                None => return Ok(None),
            }
        }
        if gone.is_empty() || gone.iter().any(|m| rank(&m.source) > rank(&kept.source)) {
            return Ok(None);
        }
        let same_source = gone.iter().all(|m| m.source == kept.source);
        let rewrite = text
            .map(str::trim)
            .filter(|t| !t.is_empty() && same_source && kept.source != SOURCE_USER)
            .filter(|t| *t != kept.content);
        if let Some(t) = rewrite {
            tx.execute(
                "UPDATE memories SET content = ?2 WHERE id = ?1",
                params![keep, t],
            )?;
        }
        for m in &gone {
            tx.execute("DELETE FROM memories WHERE id = ?1", [m.id])?;
        }
        let before = json!({
            "kept": snapshot(&kept),
            "removed": gone.iter().map(snapshot).collect::<Vec<_>>(),
            "after": rewrite,
        });
        let ns = ns_label(agent);
        let e = LedgerEntry::new(
            "memory.tidy",
            ns,
            format!("merged memories into #{keep}: {note}"),
        )
        .actor(ACTOR_TERN)
        .detail(json!({ "kept": keep, "removed": remove, "namespace": ns }));
        append_on(&tx, &e)?;
        tx.execute(
            "INSERT INTO memory_tidy (at, action, namespace, note, before, kept_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now_ms(), MERGE, ns, note, before.to_string(), keep],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Some(id))
    }

    /// Flag memories that disagree. Nothing is changed.
    pub fn tidy_flag(&self, agent: Option<&str>, ids: &[i64], note: &str) -> Result<Option<i64>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut rows = Vec::new();
        for id in ids {
            match mem(&tx, *id)?.filter(|m| m.agent_id.as_deref() == agent) {
                Some(m) => rows.push(snapshot(&m)),
                None => return Ok(None),
            }
        }
        // The same pair flagged again is not news.
        let key = json!(ids).to_string();
        let dup: i64 = tx.query_row(
            "SELECT count(*) FROM memory_tidy WHERE action = ?1 AND kept_id IS NULL
               AND state = 'applied' AND json_extract(before, '$.ids') = json(?2)",
            params![CONFLICT, key],
            |r| r.get(0),
        )?;
        if dup > 0 {
            return Ok(None);
        }
        let ns = ns_label(agent);
        let before = json!({ "ids": ids, "rows": rows });
        let e = LedgerEntry::new(
            "memory.tidy",
            ns,
            format!("flagged memories that disagree: {note}"),
        )
        .actor(ACTOR_TERN)
        .detail(json!({ "conflict": ids, "namespace": ns }));
        append_on(&tx, &e)?;
        tx.execute(
            "INSERT INTO memory_tidy (at, action, namespace, note, before) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![now_ms(), CONFLICT, ns, note, before.to_string()],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Some(id))
    }

    pub fn tidy_log(&self, limit: u32) -> Result<Vec<TidyRow>> {
        let sql = format!("SELECT {COLS} FROM memory_tidy ORDER BY id DESC LIMIT ?1");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([limit], tidy_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tidy_entry(&self, id: i64) -> Result<Option<TidyRow>> {
        let sql = format!("SELECT {COLS} FROM memory_tidy WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], tidy_row).optional()?)
    }

    /// Undo a merge (the removed memories come back, and the kept one's text
    /// as it was, unless it was edited since), or dismiss a flag.
    pub fn tidy_undo(&self, id: i64) -> Result<Undo> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let sql = format!("SELECT {COLS} FROM memory_tidy WHERE id = ?1");
        let Some(row) = tx.query_row(&sql, [id], tidy_row).optional()? else {
            return Ok(Undo::Nothing);
        };
        if row.state != "applied" {
            return Ok(Undo::Nothing);
        }
        let before: Value = serde_json::from_str(&row.before).unwrap_or(Value::Null);
        let state = if row.action == MERGE {
            restore(&tx, &before)?;
            "undone"
        } else {
            "dismissed"
        };
        tx.execute(
            "UPDATE memory_tidy SET state = ?2 WHERE id = ?1",
            params![id, state],
        )?;
        let e = LedgerEntry::new(
            "memory.tidy_undo",
            &row.namespace,
            format!("{state} a memory tidy-up: {}", row.note),
        )
        .actor(ACTOR_OWNER)
        .detail(json!({ "tidy_id": id, "action": row.action }));
        append_on(&tx, &e)?;
        tx.commit()?;
        Ok(Undo::Done)
    }
}

fn restore(tx: &Connection, before: &Value) -> rusqlite::Result<()> {
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let int = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64);
    for r in before["removed"].as_array().into_iter().flatten() {
        let id = int(r, "id");
        // The id is free unless a later memory took it: then it gets a new one.
        let taken: bool = match id {
            Some(i) => {
                tx.query_row("SELECT count(*) FROM memories WHERE id = ?1", [i], |x| {
                    x.get::<_, i64>(0)
                })? > 0
            }
            None => true,
        };
        tx.execute(
            "INSERT INTO memories (id, content, tags, created_at, agent_id, source, run_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                if taken { None } else { id },
                text(r, "content").unwrap_or_default(),
                text(r, "tags").unwrap_or_default(),
                int(r, "created_at").unwrap_or_else(now_ms),
                text(r, "agent_id"),
                text(r, "source").unwrap_or_else(|| "agent".into()),
                text(r, "run_id"),
            ],
        )?;
    }
    // The kept memory goes back to its old text, if nobody changed it since.
    let kept = &before["kept"];
    if let (Some(id), Some(after)) = (int(kept, "id"), text(before, "after")) {
        tx.execute(
            "UPDATE memories SET content = ?2, tags = ?3 WHERE id = ?1 AND content = ?4",
            params![
                id,
                text(kept, "content").unwrap_or_default(),
                text(kept, "tags").unwrap_or_default(),
                after
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryLimits, NewMemory, SOURCE_AGENT};

    fn save(s: &Store, text: &str, source: &str) -> i64 {
        s.remember(
            &NewMemory {
                content: text,
                tags: &[],
                agent: None,
                source,
                run_id: "r",
                session_id: "s",
                actor_agent: "tern",
            },
            &MemoryLimits::default(),
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn a_merge_is_logged_and_undone() {
        let s = Store::open_in_memory().unwrap();
        let a = save(&s, "Mitch likes tea", SOURCE_AGENT);
        let b = save(&s, "Mitch prefers tea to coffee", SOURCE_AGENT);
        let id = s
            .tidy_merge(None, a, &[b], Some("Mitch prefers tea to coffee."), "tea")
            .unwrap()
            .unwrap();
        assert_eq!(s.memories_in(None).unwrap().len(), 1);
        assert_eq!(
            s.memory(a).unwrap().unwrap().content,
            "Mitch prefers tea to coffee."
        );
        assert_eq!(s.tidy_undo(id).unwrap(), Undo::Done);
        let all = s.memories_in(None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(s.memory(a).unwrap().unwrap().content, "Mitch likes tea");
        assert_eq!(
            s.memory(b).unwrap().unwrap().content,
            "Mitch prefers tea to coffee"
        );
        assert_eq!(s.tidy_undo(id).unwrap(), Undo::Nothing, "once");
        assert_eq!(s.tidy_log(10).unwrap()[0].state, "undone");
    }

    #[test]
    fn unverified_never_overwrites_or_removes_verified() {
        let s = Store::open_in_memory().unwrap();
        let v = save(&s, "Works at Acme", SOURCE_AGENT);
        let u = save(&s, "Works at Acme Corp", SOURCE_UNVERIFIED);
        // An unverified keeper can't absorb a verified memory.
        assert!(
            s.tidy_merge(None, u, &[v], Some("x"), "n")
                .unwrap()
                .is_none()
        );
        // A verified keeper absorbs the unverified one but keeps its words.
        let id = s
            .tidy_merge(None, v, &[u], Some("Works at Acme Corp"), "n")
            .unwrap();
        assert!(id.is_some());
        assert_eq!(s.memory(v).unwrap().unwrap().content, "Works at Acme");
        // The user's words are never rewritten.
        let mine = save(&s, "I live in Leeds", SOURCE_USER);
        let dup = save(&s, "Lives in Leeds", SOURCE_USER);
        s.tidy_merge(None, mine, &[dup], Some("Leeds"), "n")
            .unwrap()
            .unwrap();
        assert_eq!(s.memory(mine).unwrap().unwrap().content, "I live in Leeds");
        // An agent's memory can't stand in for the user's.
        let theirs = save(&s, "Based in Leeds", SOURCE_AGENT);
        let more = save(&s, "Lives in Leeds UK", SOURCE_USER);
        assert!(
            s.tidy_merge(None, theirs, &[more], None, "n")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_conflict_changes_nothing_and_is_flagged_once() {
        let s = Store::open_in_memory().unwrap();
        let a = save(&s, "Standup is at 9", SOURCE_AGENT);
        let b = save(&s, "Standup is at 10", SOURCE_AGENT);
        let id = s.tidy_flag(None, &[a, b], "times differ").unwrap().unwrap();
        assert_eq!(s.memories_in(None).unwrap().len(), 2);
        assert!(s.tidy_flag(None, &[a, b], "again").unwrap().is_none());
        assert_eq!(s.tidy_undo(id).unwrap(), Undo::Done);
        assert_eq!(s.tidy_log(5).unwrap()[0].state, "dismissed");
    }

    #[test]
    fn the_fingerprint_moves_with_the_memories() {
        let s = Store::open_in_memory().unwrap();
        let f0 = s.memory_fingerprint(None).unwrap();
        let a = save(&s, "one", SOURCE_AGENT);
        let f1 = s.memory_fingerprint(None).unwrap();
        assert_ne!(f0, f1);
        s.edit_memory(a, "two!", None).unwrap();
        assert_ne!(f1, s.memory_fingerprint(None).unwrap());
    }
}
