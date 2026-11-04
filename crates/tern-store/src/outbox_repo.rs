//! The outbox: writes nobody could approve in time, waiting for the user
//! (docs/personal-agent-tooling-spec.md §5.2).

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store, now_ms};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    pub id: i64,
    pub run_id: String,
    pub session_id: String,
    pub call_id: String,
    pub tool: String,
    pub input_json: String,
    pub summary: String,
    pub effect: String,
    pub taint: Option<String>,
    pub local_only: bool,
    /// `pending`, `approved`, `done`, `failed`, `rejected` or `expired`.
    pub state: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub decided_at: Option<i64>,
    pub note: Option<String>,
    pub edited_input_json: Option<String>,
    pub exec_run_id: Option<String>,
    pub result_json: Option<String>,
}

impl OutboxRow {
    /// The arguments it runs with: the user's edit, else the model's.
    pub fn effective_input(&self) -> &str {
        self.edited_input_json
            .as_deref()
            .unwrap_or(&self.input_json)
    }
}

#[derive(Debug, Clone, Default)]
pub struct NewOutbox<'a> {
    pub run_id: &'a str,
    pub session_id: &'a str,
    pub call_id: &'a str,
    pub tool: &'a str,
    pub input_json: &'a str,
    pub summary: &'a str,
    pub effect: &'a str,
    pub taint: Option<&'a str>,
    pub local_only: bool,
    pub expires_at: i64,
}

const COLS: &str = "id, run_id, session_id, call_id, tool, input_json, summary, effect, taint, local_only, \
    state, created_at, expires_at, decided_at, note, edited_input_json, exec_run_id, result_json";

fn row(r: &Row<'_>) -> rusqlite::Result<OutboxRow> {
    Ok(OutboxRow {
        id: r.get(0)?,
        run_id: r.get(1)?,
        session_id: r.get(2)?,
        call_id: r.get(3)?,
        tool: r.get(4)?,
        input_json: r.get(5)?,
        summary: r.get(6)?,
        effect: r.get(7)?,
        taint: r.get(8)?,
        local_only: r.get::<_, i64>(9)? != 0,
        state: r.get(10)?,
        created_at: r.get(11)?,
        expires_at: r.get(12)?,
        decided_at: r.get(13)?,
        note: r.get(14)?,
        edited_input_json: r.get(15)?,
        exec_run_id: r.get(16)?,
        result_json: r.get(17)?,
    })
}

impl Store {
    /// Queue a call. Idempotent per `(run_id, call_id)`: a retried approval
    /// step finds the item it made before.
    pub fn add_outbox(&self, o: &NewOutbox<'_>) -> Result<OutboxRow> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO outbox (run_id, session_id, call_id, tool, input_json, summary, effect, taint,
               local_only, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (run_id, call_id) DO NOTHING",
            params![
                o.run_id,
                o.session_id,
                o.call_id,
                o.tool,
                o.input_json,
                o.summary,
                o.effect,
                o.taint,
                o.local_only,
                now_ms(),
                o.expires_at
            ],
        )?;
        let sql = format!("SELECT {COLS} FROM outbox WHERE run_id = ?1 AND call_id = ?2");
        Ok(conn.query_row(&sql, params![o.run_id, o.call_id], row)?)
    }

    pub fn outbox_item(&self, id: i64) -> Result<Option<OutboxRow>> {
        let sql = format!("SELECT {COLS} FROM outbox WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], row).optional()?)
    }

    /// The item a system run executes.
    pub fn outbox_by_exec_run(&self, run_id: &str) -> Result<Option<OutboxRow>> {
        let sql = format!("SELECT {COLS} FROM outbox WHERE exec_run_id = ?1");
        Ok(self.conn()?.query_row(&sql, [run_id], row).optional()?)
    }

    /// Items in `state` (all when `None`), newest first.
    pub fn outbox(&self, state: Option<&str>, limit: u32) -> Result<Vec<OutboxRow>> {
        let sql = format!(
            "SELECT {COLS} FROM outbox WHERE ?1 IS NULL OR state = ?1 ORDER BY id DESC LIMIT ?2"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![state, limit], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A run's items still waiting for the user, oldest first.
    pub fn pending_outbox_for_run(&self, run_id: &str) -> Result<Vec<OutboxRow>> {
        let sql = format!(
            "SELECT {COLS} FROM outbox WHERE run_id = ?1 AND state = 'pending' ORDER BY id"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([run_id], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Approve a pending item, naming the run that will execute it. Only
    /// one caller wins; the rest get `false`. `edited_input` replaces the
    /// arguments.
    pub fn approve_outbox(
        &self,
        id: i64,
        exec_run_id: &str,
        edited_input: Option<&str>,
    ) -> Result<bool> {
        Ok(self.conn()?.execute(
            "UPDATE outbox SET state = 'approved', decided_at = ?2, exec_run_id = ?3,
               edited_input_json = ?4
             WHERE id = ?1 AND state = 'pending' AND expires_at > ?2",
            params![id, now_ms(), exec_run_id, edited_input],
        )? > 0)
    }

    /// Reject a pending item. Returns whether it was pending.
    pub fn reject_outbox(&self, id: i64, note: Option<&str>) -> Result<bool> {
        Ok(self.conn()?.execute(
            "UPDATE outbox SET state = 'rejected', decided_at = ?2, note = ?3
             WHERE id = ?1 AND state = 'pending'",
            params![id, now_ms(), note],
        )? > 0)
    }

    /// Record an approved item's result: `done`, or `failed` for an error.
    pub fn finish_outbox(&self, id: i64, failed: bool, result_json: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE outbox SET state = ?2, result_json = ?3 WHERE id = ?1 AND state = 'approved'",
            params![id, if failed { "failed" } else { "done" }, result_json],
        )?;
        Ok(())
    }

    /// Expire pending items past their time, returning them (each once).
    pub fn expire_outbox(&self, now: i64) -> Result<Vec<OutboxRow>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let sql = format!(
            "SELECT {COLS} FROM outbox WHERE state = 'pending' AND expires_at <= ?1 ORDER BY id"
        );
        let rows: Vec<OutboxRow> = {
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map([now], row)?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        tx.execute(
            "UPDATE outbox SET state = 'expired', decided_at = ?1
             WHERE state = 'pending' AND expires_at <= ?1",
            [now],
        )?;
        tx.commit()?;
        Ok(rows)
    }

    /// Approved items whose system run was never created (a crash between
    /// approving and starting it).
    pub fn approved_outbox_without_run(&self) -> Result<Vec<OutboxRow>> {
        let sql = format!(
            "SELECT {COLS} FROM outbox o WHERE state = 'approved'
               AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.id = o.exec_run_id) ORDER BY id"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewMessage, NewRun};

    fn store() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.create_session("s", "cli", None).unwrap();
        s.create_run(
            &NewRun {
                id: "r",
                session_id: "s",
                trigger: "cron",
                model: "m:x",
                system_prompt: "",
                ..NewRun::default()
            },
            &NewMessage {
                role: "user".into(),
                content_json: "[]".into(),
                text: "go".into(),
                summary_of: None,
            },
        )
        .unwrap();
        s
    }

    fn item(call: &str, expires_at: i64) -> NewOutbox<'_> {
        NewOutbox {
            run_id: "r",
            session_id: "s",
            call_id: call,
            tool: "send",
            input_json: "{}",
            summary: "send hi",
            effect: "write",
            expires_at,
            ..NewOutbox::default()
        }
    }

    #[test]
    fn items_are_unique_per_call_and_decided_once() {
        let s = store();
        let far = now_ms() + 60_000;
        let a = s.add_outbox(&item("c1", far)).unwrap();
        assert_eq!(
            s.add_outbox(&item("c1", far)).unwrap().id,
            a.id,
            "idempotent"
        );
        assert_eq!(a.state, "pending");
        assert_eq!(s.pending_outbox_for_run("r").unwrap().len(), 1);

        assert!(
            s.approve_outbox(a.id, "x1", Some(r#"{"to":"bob"}"#))
                .unwrap()
        );
        assert!(!s.approve_outbox(a.id, "x2", None).unwrap(), "only once");
        assert!(!s.reject_outbox(a.id, None).unwrap());
        let a = s.outbox_item(a.id).unwrap().unwrap();
        assert_eq!(a.effective_input(), r#"{"to":"bob"}"#);
        assert_eq!(s.approved_outbox_without_run().unwrap().len(), 1);
        s.finish_outbox(a.id, false, "{}").unwrap();
        assert_eq!(s.outbox_item(a.id).unwrap().unwrap().state, "done");

        let b = s.add_outbox(&item("c2", far)).unwrap();
        assert!(s.reject_outbox(b.id, Some("not now")).unwrap());
        assert_eq!(
            s.outbox(Some("rejected"), 10).unwrap()[0].note.as_deref(),
            Some("not now")
        );
    }

    #[test]
    fn pending_items_expire_once_and_cant_be_approved_after() {
        let s = store();
        let c = s.add_outbox(&item("c3", now_ms() - 1)).unwrap();
        assert!(!s.approve_outbox(c.id, "x", None).unwrap(), "already past");
        let gone = s.expire_outbox(now_ms()).unwrap();
        assert_eq!(gone.len(), 1);
        assert!(s.expire_outbox(now_ms()).unwrap().is_empty());
        assert_eq!(s.outbox_item(c.id).unwrap().unwrap().state, "expired");
    }
}
