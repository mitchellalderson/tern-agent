//! Schedules, gateway pairing, chat sessions, and run delivery targets.

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, SessionRow, Store, now_ms};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleRow {
    pub id: String,
    pub cron: String,
    pub prompt: String,
    pub session_id: String,
    pub deliver_to: Option<String>,
    pub enabled: bool,
    pub next_run_at: i64,
    pub last_run_at: Option<i64>,
}

const SCHEDULE_COLS: &str =
    "id, cron, prompt, session_id, deliver_to, enabled, next_run_at, last_run_at";

fn schedule_row(r: &Row<'_>) -> rusqlite::Result<ScheduleRow> {
    Ok(ScheduleRow {
        id: r.get(0)?,
        cron: r.get(1)?,
        prompt: r.get(2)?,
        session_id: r.get(3)?,
        deliver_to: r.get(4)?,
        enabled: r.get::<_, i64>(5)? != 0,
        next_run_at: r.get(6)?,
        last_run_at: r.get(7)?,
    })
}

impl Store {
    // ---- schedules ----

    pub fn create_schedule(&self, s: &ScheduleRow) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO schedules (id, cron, prompt, session_id, deliver_to, enabled, next_run_at, last_run_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![s.id, s.cron, s.prompt, s.session_id, s.deliver_to, s.enabled as i64, s.next_run_at, s.last_run_at],
        )?;
        Ok(())
    }

    pub fn schedules(&self) -> Result<Vec<ScheduleRow>> {
        let sql = format!("SELECT {SCHEDULE_COLS} FROM schedules ORDER BY next_run_at");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], schedule_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn schedule(&self, id: &str) -> Result<Option<ScheduleRow>> {
        let sql = format!("SELECT {SCHEDULE_COLS} FROM schedules WHERE id = ?1");
        Ok(self
            .conn()?
            .query_row(&sql, [id], schedule_row)
            .optional()?)
    }

    /// Enabled schedules whose next run is at or before `now`.
    pub fn due_schedules(&self, now: i64) -> Result<Vec<ScheduleRow>> {
        let sql = format!(
            "SELECT {SCHEDULE_COLS} FROM schedules WHERE enabled = 1 AND next_run_at <= ?1 ORDER BY next_run_at"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([now], schedule_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mark_schedule_ran(&self, id: &str, ran_at: i64, next_run_at: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE schedules SET last_run_at = ?2, next_run_at = ?3 WHERE id = ?1",
            params![id, ran_at, next_run_at],
        )?;
        Ok(())
    }

    /// Replace a schedule's editable fields (everything but its id and session).
    pub fn update_schedule(&self, s: &ScheduleRow) -> Result<bool> {
        Ok(self.conn()?.execute(
            "UPDATE schedules SET cron = ?2, prompt = ?3, deliver_to = ?4, enabled = ?5, next_run_at = ?6
             WHERE id = ?1",
            params![s.id, s.cron, s.prompt, s.deliver_to, s.enabled as i64, s.next_run_at],
        )? > 0)
    }

    pub fn delete_schedule(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn()?
            .execute("DELETE FROM schedules WHERE id = ?1", [id])?
            > 0)
    }

    // ---- chat sessions ----

    pub fn create_session_with_ref(
        &self,
        id: &str,
        source: &str,
        external_ref: &str,
        model: Option<&str>,
    ) -> Result<SessionRow> {
        let mut s = self.create_session(id, source, model)?;
        self.conn()?.execute(
            "UPDATE sessions SET external_ref = ?2 WHERE id = ?1",
            params![id, external_ref],
        )?;
        s.external_ref = Some(external_ref.to_owned());
        Ok(s)
    }

    /// The most recently active session for a chat (e.g. a Telegram chat id).
    pub fn latest_session_for(
        &self,
        source: &str,
        external_ref: &str,
    ) -> Result<Option<SessionRow>> {
        let id: Option<String> = self
            .conn()?
            .query_row(
                "SELECT id FROM sessions WHERE source = ?1 AND external_ref = ?2 ORDER BY updated_at DESC, rowid DESC LIMIT 1",
                params![source, external_ref],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.session(&id),
            None => Ok(None),
        }
    }

    // ---- run delivery ----

    pub fn set_run_deliver_to(&self, run_id: &str, target: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE runs SET deliver_to = ?2 WHERE id = ?1",
            params![run_id, target],
        )?;
        Ok(())
    }

    pub fn run_deliver_to(&self, run_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row("SELECT deliver_to FROM runs WHERE id = ?1", [run_id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten())
    }

    // ---- pairing ----

    pub fn create_pairing_code(&self, code: &str, ttl_ms: i64) -> Result<()> {
        let now = now_ms();
        let conn = self.conn()?;
        conn.execute("DELETE FROM pairing_codes WHERE expires_at < ?1", [now])?;
        conn.execute(
            "INSERT INTO pairing_codes (code, expires_at) VALUES (?1, ?2)",
            params![code, now + ttl_ms],
        )?;
        Ok(())
    }

    /// Consume a pairing code: true if it existed and had not expired.
    pub fn consume_pairing_code(&self, code: &str) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "DELETE FROM pairing_codes WHERE code = ?1 AND expires_at >= ?2",
            params![code, now_ms()],
        )?;
        Ok(n > 0)
    }

    pub fn pair_telegram_user(&self, user_id: i64) -> Result<()> {
        self.conn()?.execute(
            "INSERT OR IGNORE INTO telegram_pairings (user_id, paired_at) VALUES (?1, ?2)",
            params![user_id, now_ms()],
        )?;
        Ok(())
    }

    pub fn is_telegram_user_paired(&self, user_id: i64) -> Result<bool> {
        Ok(self.conn()?.query_row(
            "SELECT EXISTS (SELECT 1 FROM telegram_pairings WHERE user_id = ?1)",
            [user_id],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_roundtrip_and_due() {
        let s = Store::open_in_memory().unwrap();
        s.create_session("sess", "cron", None).unwrap();
        let mk = |id: &str, next| ScheduleRow {
            id: id.into(),
            cron: "0 8 * * 1-5".into(),
            prompt: "p".into(),
            session_id: "sess".into(),
            deliver_to: Some("telegram:1".into()),
            enabled: true,
            next_run_at: next,
            last_run_at: None,
        };
        s.create_schedule(&mk("a", 100)).unwrap();
        s.create_schedule(&mk("b", 500)).unwrap();
        assert_eq!(
            s.due_schedules(200)
                .unwrap()
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["a"]
        );
        s.mark_schedule_ran("a", 200, 1000).unwrap();
        assert!(s.due_schedules(200).unwrap().is_empty());
        assert_eq!(s.schedule("a").unwrap().unwrap().last_run_at, Some(200));
        assert!(s.delete_schedule("b").unwrap());
        assert!(!s.delete_schedule("b").unwrap());
        assert_eq!(s.schedules().unwrap().len(), 1);
    }

    #[test]
    fn pairing_codes_are_single_use_and_expire() {
        let s = Store::open_in_memory().unwrap();
        s.create_pairing_code("ABC", 60_000).unwrap();
        s.create_pairing_code("OLD", -1).unwrap();
        assert!(s.consume_pairing_code("ABC").unwrap());
        assert!(!s.consume_pairing_code("ABC").unwrap());
        assert!(!s.consume_pairing_code("OLD").unwrap());
        assert!(!s.is_telegram_user_paired(42).unwrap());
        s.pair_telegram_user(42).unwrap();
        s.pair_telegram_user(42).unwrap();
        assert!(s.is_telegram_user_paired(42).unwrap());
    }

    #[test]
    fn latest_chat_session_wins() {
        let s = Store::open_in_memory().unwrap();
        s.create_session_with_ref("old", "telegram", "99", None)
            .unwrap();
        s.create_session_with_ref("new", "telegram", "99", None)
            .unwrap();
        s.create_session_with_ref("other", "telegram", "7", None)
            .unwrap();
        assert_eq!(
            s.latest_session_for("telegram", "99").unwrap().unwrap().id,
            "new"
        );
        assert!(s.latest_session_for("telegram", "1").unwrap().is_none());
    }
}
