//! Waits: one-shot "wait until" checks tied to a conversation. The daemon
//! polls them with no model call and starts one continuation run when the
//! condition matches or the wait times out; the row is then deleted. Every
//! create, fire and cancel is in the ledger, on the same connection.

use rusqlite::{OptionalExtension, Row, params};

use crate::{LedgerEntry, Result, Store, now_ms};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WaitRow {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub agent_id: String,
    pub note: String,
    /// `connector`, `url` or `time`.
    pub check_kind: String,
    /// The check's arguments, JSON.
    pub check_spec: String,
    /// `{path, equals | in}` as JSON; `None` for a time wait.
    pub until_spec: Option<String>,
    pub every_secs: i64,
    pub next_check_at: i64,
    pub expires_at: i64,
    pub tainted: bool,
    pub deliver_to: Option<String>,
    pub created_at: i64,
    pub checks: i64,
    pub last_checked_at: Option<i64>,
    pub last_check: Option<String>,
    pub last_value: Option<String>,
}

/// A wait to create; the ids and times come from the caller.
#[derive(Debug, Clone, Default)]
pub struct NewWait<'a> {
    pub id: &'a str,
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub agent_id: &'a str,
    pub note: &'a str,
    pub check_kind: &'a str,
    pub check_spec: &'a str,
    pub until_spec: Option<&'a str>,
    pub every_secs: i64,
    pub next_check_at: i64,
    pub expires_at: i64,
    pub tainted: bool,
    pub deliver_to: Option<&'a str>,
}

const COLS: &str = "id, session_id, run_id, agent_id, note, check_kind, check_spec, until_spec,
    every_secs, next_check_at, expires_at, tainted, deliver_to, created_at, checks,
    last_checked_at, last_check, last_value";

fn wait_row(r: &Row<'_>) -> rusqlite::Result<WaitRow> {
    Ok(WaitRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        run_id: r.get(2)?,
        agent_id: r.get(3)?,
        note: r.get(4)?,
        check_kind: r.get(5)?,
        check_spec: r.get(6)?,
        until_spec: r.get(7)?,
        every_secs: r.get(8)?,
        next_check_at: r.get(9)?,
        expires_at: r.get(10)?,
        tainted: r.get(11)?,
        deliver_to: r.get(12)?,
        created_at: r.get(13)?,
        checks: r.get(14)?,
        last_checked_at: r.get(15)?,
        last_check: r.get(16)?,
        last_value: r.get(17)?,
    })
}

impl Store {
    /// Save a wait, unless `agent` already has `max_active`: `None` then.
    /// The count and the insert share one connection, so two runs of an
    /// agent can't both take the last slot.
    pub fn create_wait(&self, w: &NewWait<'_>, max_active: usize) -> Result<Option<WaitRow>> {
        let conn = self.conn()?;
        let active: i64 = conn.query_row(
            "SELECT count(*) FROM waits WHERE agent_id = ?1",
            [w.agent_id],
            |r| r.get(0),
        )?;
        if active >= max_active as i64 {
            return Ok(None);
        }
        conn.execute(
            "INSERT INTO waits (id, session_id, run_id, agent_id, note, check_kind, check_spec,
               until_spec, every_secs, next_check_at, expires_at, tainted, deliver_to, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                w.id,
                w.session_id,
                w.run_id,
                w.agent_id,
                w.note,
                w.check_kind,
                w.check_spec,
                w.until_spec,
                w.every_secs,
                w.next_check_at,
                w.expires_at,
                w.tainted,
                w.deliver_to,
                now_ms()
            ],
        )?;
        let e = LedgerEntry::new("wait.create", w.id, format!("waiting: {}", w.note))
            .actor(crate::ledger_repo::ACTOR_TERN)
            .run(w.run_id, w.session_id)
            .detail(serde_json::json!({
                "check": w.check_kind,
                "every_secs": w.every_secs,
                "expires_at": w.expires_at,
                "tainted": w.tainted,
            }));
        crate::ledger_repo::append_on(&conn, &e)?;
        let sql = format!("SELECT {COLS} FROM waits WHERE id = ?1");
        Ok(conn.query_row(&sql, [w.id], wait_row).optional()?)
    }

    pub fn wait(&self, id: &str) -> Result<Option<WaitRow>> {
        let sql = format!("SELECT {COLS} FROM waits WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], wait_row).optional()?)
    }

    /// Every wait, soonest to expire first; `agent` limits them to one agent's.
    pub fn waits(&self, agent: Option<&str>) -> Result<Vec<WaitRow>> {
        let sql = format!(
            "SELECT {COLS} FROM waits WHERE (?1 IS NULL OR agent_id = ?1)
             ORDER BY expires_at, created_at"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([agent], wait_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Waits to check now: due, or past their expiry.
    pub fn due_waits(&self, now: i64) -> Result<Vec<WaitRow>> {
        let sql = format!(
            "SELECT {COLS} FROM waits WHERE next_check_at <= ?1 OR expires_at <= ?1
             ORDER BY next_check_at"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([now], wait_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record a check that did not end the wait, and when to look next.
    pub fn record_wait_check(
        &self,
        id: &str,
        now: i64,
        next_check_at: i64,
        found: &str,
        value: Option<&str>,
    ) -> Result<()> {
        self.conn()?.execute(
            "UPDATE waits SET checks = checks + 1, last_checked_at = ?2, next_check_at = ?3,
               last_check = ?4, last_value = COALESCE(?5, last_value) WHERE id = ?1",
            params![id, now, next_check_at, found, value],
        )?;
        Ok(())
    }

    /// Delete wait `id` and record why (`wait.fire` or `wait.cancel`).
    /// False if it was already gone, so a fire and a cancel can't both win.
    pub fn end_wait(&self, id: &str, kind: &str, actor: &str, why: &str) -> Result<bool> {
        let conn = self.conn()?;
        let sql = format!("SELECT {COLS} FROM waits WHERE id = ?1");
        let Some(w) = conn.query_row(&sql, [id], wait_row).optional()? else {
            return Ok(false);
        };
        if conn.execute("DELETE FROM waits WHERE id = ?1", [id])? == 0 {
            return Ok(false);
        }
        let e = LedgerEntry::new(kind, id, format!("{why}: {}", w.note))
            .actor(actor)
            .run(&w.run_id, &w.session_id)
            .detail(serde_json::json!({ "why": why, "checks": w.checks }));
        crate::ledger_repo::append_on(&conn, &e)?;
        Ok(true)
    }

    /// Waits ended by `kind` for `agent` since `since`: the daily count.
    pub fn waits_fired_since(&self, agent: &str, since: i64) -> Result<u32> {
        Ok(self.conn()?.query_row(
            "SELECT count(*) FROM ledger l JOIN sessions s ON s.id = l.session_id
             WHERE l.kind = 'wait.fire' AND l.at >= ?2 AND COALESCE(s.agent_id, ?3) = ?1",
            params![agent, since, crate::DEFAULT_AGENT],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new<'a>(id: &'a str, agent: &'a str) -> NewWait<'a> {
        NewWait {
            id,
            session_id: "s",
            run_id: "r",
            agent_id: agent,
            note: "CI",
            check_kind: "url",
            check_spec: "{}",
            until_spec: Some("{}"),
            every_secs: 60,
            next_check_at: 100,
            expires_at: 1000,
            ..NewWait::default()
        }
    }

    #[test]
    fn create_cap_due_and_end_are_ledgered() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.create_wait(&new("a", "x"), 2).unwrap().is_some());
        assert!(s.create_wait(&new("b", "x"), 2).unwrap().is_some());
        // The cap is per agent.
        assert!(s.create_wait(&new("c", "x"), 2).unwrap().is_none());
        assert!(s.create_wait(&new("d", "y"), 2).unwrap().is_some());
        assert_eq!(s.waits(Some("x")).unwrap().len(), 2);
        assert_eq!(s.waits(None).unwrap().len(), 3);

        assert_eq!(s.due_waits(99).unwrap().len(), 0);
        assert_eq!(s.due_waits(100).unwrap().len(), 3);
        s.record_wait_check("a", 100, 160, "status: queued", Some("queued"))
            .unwrap();
        assert_eq!(s.due_waits(100).unwrap().len(), 2);
        let a = s.wait("a").unwrap().unwrap();
        assert_eq!((a.checks, a.last_value.as_deref()), (1, Some("queued")));
        // Expired waits are due whatever their next check says.
        assert_eq!(s.due_waits(1000).unwrap().len(), 3);

        assert!(s.end_wait("a", "wait.fire", "tern", "matched").unwrap());
        assert!(
            !s.end_wait("a", "wait.cancel", "owner", "cancelled")
                .unwrap()
        );
        assert!(
            s.end_wait("b", "wait.cancel", "owner", "cancelled")
                .unwrap()
        );
        // Freed slots can be used again.
        assert!(s.create_wait(&new("e", "x"), 2).unwrap().is_some());
        let kinds: Vec<String> = s
            .ledger(&crate::LedgerFilter::default())
            .unwrap()
            .into_iter()
            .map(|r| r.entry.kind)
            .filter(|k| k.starts_with("wait."))
            .collect();
        assert_eq!(kinds.iter().filter(|k| *k == "wait.create").count(), 4);
        assert_eq!(kinds.iter().filter(|k| *k == "wait.fire").count(), 1);
        assert_eq!(kinds.iter().filter(|k| *k == "wait.cancel").count(), 1);
    }
}
