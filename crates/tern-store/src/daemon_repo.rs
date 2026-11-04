//! Schedules, gateway pairing, chat sessions, and run delivery targets.

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, SessionRow, Store, now_ms};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScheduleRow {
    pub id: String,
    pub cron: String,
    pub prompt: String,
    pub session_id: String,
    pub deliver_to: Option<String>,
    pub enabled: bool,
    pub next_run_at: i64,
    pub last_run_at: Option<i64>,
    /// `None` = the configured default; else `HH:MM-HH:MM` or `none`.
    pub quiet_hours: Option<String>,
    /// Delivers even inside quiet hours.
    pub urgent: bool,
    /// Caps; `None` = the configured default.
    pub runs_per_day: Option<u32>,
    pub usd_per_day: Option<f64>,
    pub tokens_per_run: Option<u64>,
    /// Set while a daily cap has paused it (unix ms).
    pub paused_until: Option<i64>,
    pub paused_reason: Option<String>,
    /// When the user last resumed it; daily caps count from here or the
    /// start of the day, whichever is later.
    pub budget_since: Option<i64>,
    /// The channel its results are posted to; `None` = Home.
    pub channel_id: Option<String>,
    /// Its autonomy profile; `None` = the configured `autonomy`.
    pub autonomy: Option<String>,
    /// The agent whose job it is; `None` = the default agent (agent
    /// profiles §4). Its session belongs to the same agent.
    pub agent_id: Option<String>,
}

impl ScheduleRow {
    pub fn agent(&self) -> &str {
        self.agent_id.as_deref().unwrap_or(crate::DEFAULT_AGENT)
    }
}

const SCHEDULE_COLS: &str = "id, cron, prompt, session_id, deliver_to, enabled, next_run_at, last_run_at, \
    quiet_hours, urgent, runs_per_day, usd_per_day, tokens_per_run, paused_until, paused_reason, budget_since, \
    channel_id, autonomy, agent_id";

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
        quiet_hours: r.get(8)?,
        urgent: r.get::<_, i64>(9)? != 0,
        runs_per_day: r.get(10)?,
        usd_per_day: r.get(11)?,
        tokens_per_run: r.get::<_, Option<i64>>(12)?.map(|v| v.max(0) as u64),
        paused_until: r.get(13)?,
        paused_reason: r.get(14)?,
        budget_since: r.get(15)?,
        channel_id: r.get(16)?,
        autonomy: r.get(17)?,
        agent_id: r.get(18)?,
    })
}

/// A delivery held during quiet hours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldDelivery {
    pub id: i64,
    pub target: String,
    pub run_id: Option<String>,
    /// What it is from, e.g. the schedule's first prompt line.
    pub label: Option<String>,
    pub text: String,
    pub release_at: i64,
}

/// Tokens used: (input, output, cache read, cache write).
pub type TokenCounts = (u64, u64, u64, u64);

fn tokens(v: Option<u64>) -> Option<i64> {
    v.map(|t| t.min(i64::MAX as u64) as i64)
}

impl Store {
    // ---- schedules ----

    pub fn create_schedule(&self, s: &ScheduleRow) -> Result<()> {
        self.conn()?.execute(
            &format!(
                "INSERT INTO schedules ({SCHEDULE_COLS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                   COALESCE(?17, 'home'), ?18, NULLIF(?19, 'tern'))"
            ),
            params![
                s.id,
                s.cron,
                s.prompt,
                s.session_id,
                s.deliver_to,
                s.enabled as i64,
                s.next_run_at,
                s.last_run_at,
                s.quiet_hours,
                s.urgent as i64,
                s.runs_per_day,
                s.usd_per_day,
                tokens(s.tokens_per_run),
                s.paused_until,
                s.paused_reason,
                s.budget_since,
                s.channel_id,
                s.autonomy,
                s.agent_id
            ],
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

    /// The schedule that runs in `session_id`, if any.
    pub fn schedule_for_session(&self, session_id: &str) -> Result<Option<ScheduleRow>> {
        let sql = format!("SELECT {SCHEDULE_COLS} FROM schedules WHERE session_id = ?1");
        Ok(self
            .conn()?
            .query_row(&sql, [session_id], schedule_row)
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

    /// Move a skipped schedule's next run without recording a run.
    pub fn advance_schedule(&self, id: &str, next_run_at: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE schedules SET next_run_at = ?2 WHERE id = ?1",
            params![id, next_run_at],
        )?;
        Ok(())
    }

    /// Pause a schedule until `until` (unix ms) because a daily cap was
    /// hit. False if it was already paused (so the caller notifies once).
    pub fn pause_schedule(&self, id: &str, until: i64, reason: &str) -> Result<bool> {
        Ok(self.conn()?.execute(
            "UPDATE schedules SET paused_until = ?2, paused_reason = ?3
             WHERE id = ?1 AND (paused_until IS NULL OR paused_until <= ?4)",
            params![id, until, reason, now_ms()],
        )? > 0)
    }

    /// Lift a budget pause. `by_user` also restarts the daily count, so the
    /// cap that paused it doesn't pause it again at once.
    pub fn unpause_schedule(&self, id: &str, by_user: bool) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE schedules SET paused_until = NULL, paused_reason = NULL WHERE id = ?1",
            [id],
        )?;
        if by_user {
            conn.execute(
                "UPDATE schedules SET budget_since = ?2 WHERE id = ?1",
                params![id, now_ms()],
            )?;
        }
        Ok(())
    }

    /// Replace a schedule's editable fields (everything but its id, session
    /// and pause state).
    pub fn update_schedule(&self, s: &ScheduleRow) -> Result<bool> {
        Ok(self.conn()?.execute(
            "UPDATE schedules SET cron = ?2, prompt = ?3, deliver_to = ?4, enabled = ?5, next_run_at = ?6,
               quiet_hours = ?7, urgent = ?8, runs_per_day = ?9, usd_per_day = ?10, tokens_per_run = ?11,
               autonomy = ?12
             WHERE id = ?1",
            params![
                s.id,
                s.cron,
                s.prompt,
                s.deliver_to,
                s.enabled as i64,
                s.next_run_at,
                s.quiet_hours,
                s.urgent as i64,
                s.runs_per_day,
                s.usd_per_day,
                tokens(s.tokens_per_run),
                s.autonomy
            ],
        )? > 0)
    }

    /// Top-level scheduled runs started in `session_id` since `since`.
    pub fn scheduled_runs_since(&self, session_id: &str, since: i64) -> Result<u32> {
        Ok(self.conn()?.query_row(
            "SELECT count(*) FROM runs
             WHERE session_id = ?1 AND trigger = 'cron' AND parent_run_id IS NULL AND started_at >= ?2",
            params![session_id, since],
            |r| r.get(0),
        )?)
    }

    /// Tokens used per model by runs started in `session_id` since `since`,
    /// their sub-agents included: `(model, (input, output, cache_read,
    /// cache_write))`. A step a fallback answered counts against that
    /// model (`model` in its usage, T6a), not the run's.
    pub fn usage_by_model_since(
        &self,
        session_id: &str,
        since: i64,
    ) -> Result<Vec<(Option<String>, TokenCounts)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "WITH RECURSIVE tree(id) AS (
               SELECT id FROM runs WHERE session_id = ?1 AND parent_run_id IS NULL AND started_at >= ?2
               UNION ALL
               SELECT r.id FROM runs r JOIN tree t ON r.parent_run_id = t.id
             )
             SELECT COALESCE(json_extract(s.usage_json, '$.model'), r.model) AS m,
               COALESCE(SUM(json_extract(s.usage_json, '$.input_tokens')), 0),
               COALESCE(SUM(json_extract(s.usage_json, '$.output_tokens')), 0),
               COALESCE(SUM(json_extract(s.usage_json, '$.cache_read_tokens')), 0),
               COALESCE(SUM(json_extract(s.usage_json, '$.cache_write_tokens')), 0)
             FROM tree JOIN runs r ON r.id = tree.id JOIN steps s ON s.run_id = r.id
             WHERE s.usage_json IS NOT NULL
             GROUP BY m",
        )?;
        let rows = stmt.query_map(params![session_id, since], |r| {
            let n = |i| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
            Ok((r.get(0)?, (n(1)?, n(2)?, n(3)?, n(4)?)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Cap a run's tokens (input + output + cache) before it is driven.
    pub fn set_run_budget_tokens(&self, run_id: &str, budget: u64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE runs SET budget_tokens = ?2 WHERE id = ?1",
            params![run_id, tokens(Some(budget))],
        )?;
        Ok(())
    }

    /// A routine's own autonomy profile; `None` = the configured one. Only
    /// the user sets it (the API), never the agent's schedule tools.
    pub fn set_schedule_autonomy(&self, id: &str, autonomy: Option<&str>) -> Result<bool> {
        Ok(self.conn()?.execute(
            "UPDATE schedules SET autonomy = ?2 WHERE id = ?1",
            params![id, autonomy],
        )? > 0)
    }

    /// Freeze the run's host-limit setting (before it is driven).
    pub fn set_run_egress(&self, run_id: &str, egress: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE runs SET egress = ?2 WHERE id = ?1",
            params![run_id, egress],
        )?;
        Ok(())
    }

    /// Freeze the run's autonomy profile (before it is driven).
    pub fn set_run_autonomy(&self, run_id: &str, autonomy: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE runs SET autonomy = ?2 WHERE id = ?1",
            params![run_id, autonomy],
        )?;
        Ok(())
    }

    pub fn set_run_quiet(&self, run_id: &str) -> Result<()> {
        self.conn()?
            .execute("UPDATE runs SET quiet = 1 WHERE id = ?1", [run_id])?;
        Ok(())
    }

    // ---- deliveries held during quiet hours ----

    pub fn hold_delivery(
        &self,
        target: &str,
        run_id: Option<&str>,
        label: Option<&str>,
        text: &str,
        release_at: i64,
    ) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO held_deliveries (target, run_id, label, text, release_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![target, run_id, label, text, release_at, now_ms()],
        )?;
        Ok(())
    }

    /// Held deliveries due by `now`, oldest first.
    pub fn due_held_deliveries(&self, now: i64) -> Result<Vec<HeldDelivery>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, target, run_id, label, text, release_at FROM held_deliveries
             WHERE release_at <= ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([now], |r| {
            Ok(HeldDelivery {
                id: r.get(0)?,
                target: r.get(1)?,
                run_id: r.get(2)?,
                label: r.get(3)?,
                text: r.get(4)?,
                release_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn held_delivery_count(&self) -> Result<u32> {
        Ok(self
            .conn()?
            .query_row("SELECT count(*) FROM held_deliveries", [], |r| r.get(0))?)
    }

    pub fn delete_held_deliveries(&self, ids: &[i64]) -> Result<()> {
        let conn = self.conn()?;
        for id in ids {
            conn.execute("DELETE FROM held_deliveries WHERE id = ?1", [id])?;
        }
        Ok(())
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
            usd_per_day: Some(0.5),
            ..ScheduleRow::default()
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
    fn budget_pause_counts_and_held_deliveries() {
        let s = Store::open_in_memory().unwrap();
        s.create_session("sess", "cron", None).unwrap();
        s.create_schedule(&ScheduleRow {
            id: "a".into(),
            cron: "* * * * *".into(),
            prompt: "p".into(),
            session_id: "sess".into(),
            enabled: true,
            quiet_hours: Some("none".into()),
            urgent: true,
            runs_per_day: Some(2),
            tokens_per_run: Some(1000),
            ..ScheduleRow::default()
        })
        .unwrap();
        let row = s.schedule_for_session("sess").unwrap().unwrap();
        assert_eq!(row.quiet_hours.as_deref(), Some("none"));
        assert!(row.urgent);
        assert_eq!(
            (row.runs_per_day, row.tokens_per_run),
            (Some(2), Some(1000))
        );

        assert!(
            s.pause_schedule("a", now_ms() + 60_000, "runs_per_day")
                .unwrap()
        );
        assert!(
            !s.pause_schedule("a", now_ms() + 60_000, "runs_per_day")
                .unwrap(),
            "already paused: no second notice"
        );
        s.unpause_schedule("a", true).unwrap();
        let row = s.schedule("a").unwrap().unwrap();
        assert_eq!(row.paused_until, None);
        assert!(row.budget_since.is_some());
        assert_eq!(s.scheduled_runs_since("sess", 0).unwrap(), 0);

        s.hold_delivery("telegram:1", Some("r1"), Some("p"), "one", 100)
            .unwrap();
        s.hold_delivery("telegram:1", None, None, "two", 900)
            .unwrap();
        let due = s.due_held_deliveries(500).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "one");
        s.delete_held_deliveries(&[due[0].id]).unwrap();
        assert_eq!(s.held_delivery_count().unwrap(), 1);
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
