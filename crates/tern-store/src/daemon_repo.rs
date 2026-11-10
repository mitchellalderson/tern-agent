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
    /// What starts it (T1b): `cron`, `once`, `watch` or `webhook`; empty = `cron`.
    pub trigger: String,
    /// `<connector>.<watch>` for a watch trigger.
    pub watch: Option<String>,
    /// How often a watch is polled.
    pub every_secs: Option<i64>,
    /// Start runs for a watch's first poll instead of only recording it.
    pub backfill: bool,
    /// Keep watch events marked as caused by tern itself.
    pub include_self: bool,
    /// Luau run before a cron or once slot; false skips the run.
    pub gate: Option<String>,
    /// The active connector whose capabilities the gate runs with.
    pub gate_connector: Option<String>,
    /// `silent`, `digest`, `notify` or `interrupt`; `None` = the default.
    pub initiative: Option<String>,
    /// What the last watch poll or gate found, and when.
    pub last_check: Option<String>,
    pub last_check_at: Option<i64>,
}

impl ScheduleRow {
    pub fn agent(&self) -> &str {
        self.agent_id.as_deref().unwrap_or(crate::DEFAULT_AGENT)
    }

    /// `cron`, `once` or `watch`.
    pub fn trigger(&self) -> &str {
        if self.trigger.is_empty() {
            "cron"
        } else {
            &self.trigger
        }
    }
}

/// An event a watch reported (T1b), waiting for or past its run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineEvent {
    pub routine_id: String,
    pub key: String,
    /// JSON as the watch returned it; empty once a run has it.
    pub payload: String,
    /// `pending`, `batch`, `started`, `baseline` or `self`.
    pub state: String,
    pub run_id: Option<String>,
    pub created_at: i64,
}

/// An event from one poll, before it is recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub key: String,
    pub payload: String,
    /// `pending`, `baseline` or `self`. Pending events past the poll's
    /// limit are recorded as `batch`.
    pub state: &'static str,
}

/// What recording a poll changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PollRecord {
    /// Events not seen before.
    pub new: usize,
    /// Of those, the ones that will start runs (`pending` or `batch`).
    pub to_run: usize,
}

const SCHEDULE_COLS: &str = "id, cron, prompt, session_id, deliver_to, enabled, next_run_at, last_run_at, \
    quiet_hours, urgent, runs_per_day, usd_per_day, tokens_per_run, paused_until, paused_reason, budget_since, \
    channel_id, autonomy, agent_id, trigger_kind, watch, every_secs, backfill, include_self, gate, \
    gate_connector, initiative, last_check, last_check_at";

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
        trigger: r.get(19)?,
        watch: r.get(20)?,
        every_secs: r.get(21)?,
        backfill: r.get::<_, i64>(22)? != 0,
        include_self: r.get::<_, i64>(23)? != 0,
        gate: r.get(24)?,
        gate_connector: r.get(25)?,
        initiative: r.get(26)?,
        last_check: r.get(27)?,
        last_check_at: r.get(28)?,
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
    /// `quiet` (held for quiet hours) or `digest` (an initiative digest).
    pub kind: String,
}

fn event_row(r: &Row<'_>) -> rusqlite::Result<RoutineEvent> {
    Ok(RoutineEvent {
        routine_id: r.get(0)?,
        key: r.get(1)?,
        payload: r.get(2)?,
        state: r.get(3)?,
        run_id: r.get(4)?,
        created_at: r.get(5)?,
    })
}

/// Tokens used: (input, output, cache read, cache write).
pub type TokenCounts = (u64, u64, u64, u64);

fn tokens(v: Option<u64>) -> Option<i64> {
    v.map(|t| t.min(i64::MAX as u64) as i64)
}

impl Store {
    // ---- schedules ----

    pub fn create_schedule(&self, s: &ScheduleRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            &format!(
                "INSERT INTO schedules ({SCHEDULE_COLS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                   COALESCE(?17, 'home'), ?18, NULLIF(?19, 'tern'), COALESCE(NULLIF(?20, ''), 'cron'),
                   ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29)"
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
                s.agent_id,
                s.trigger,
                s.watch,
                s.every_secs,
                s.backfill as i64,
                s.include_self as i64,
                s.gate,
                s.gate_connector,
                s.initiative,
                s.last_check,
                s.last_check_at
            ],
        )?;
        note_routine_autonomy(&conn, &s.id, None, s.autonomy.as_deref())?;
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
    /// and pause state). A different watch starts over from a baseline.
    pub fn update_schedule(&self, s: &ScheduleRow) -> Result<bool> {
        let conn = self.conn()?;
        let old = schedule_autonomy(&conn, &s.id)?;
        let n = conn.execute(
            "UPDATE schedules SET cron = ?2, prompt = ?3, deliver_to = ?4, enabled = ?5, next_run_at = ?6,
               quiet_hours = ?7, urgent = ?8, runs_per_day = ?9, usd_per_day = ?10, tokens_per_run = ?11,
               autonomy = ?12,
               watch_cursor = CASE WHEN watch IS ?14 AND trigger_kind = COALESCE(NULLIF(?13, ''), 'cron')
                 THEN watch_cursor END,
               trigger_kind = COALESCE(NULLIF(?13, ''), 'cron'), watch = ?14, every_secs = ?15,
               backfill = ?16, include_self = ?17, gate = ?18, gate_connector = ?19, initiative = ?20
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
                s.autonomy,
                s.trigger,
                s.watch,
                s.every_secs,
                s.backfill as i64,
                s.include_self as i64,
                s.gate,
                s.gate_connector,
                s.initiative
            ],
        )?;
        if let Some(old) = old {
            note_routine_autonomy(&conn, &s.id, old.as_deref(), s.autonomy.as_deref())?;
        }
        Ok(n > 0)
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
    /// cache_write), cache_write_1h)`, the last being the part of the cache
    /// writes made with the 1-hour lifetime. A step a fallback answered
    /// counts against that model (`model` in its usage, T6a), not the run's.
    pub fn usage_by_model_since(
        &self,
        session_id: &str,
        since: i64,
    ) -> Result<Vec<(Option<String>, TokenCounts, u64)>> {
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
               COALESCE(SUM(json_extract(s.usage_json, '$.cache_write_tokens')), 0),
               COALESCE(SUM(json_extract(s.usage_json, '$.cache_write_1h_tokens')), 0)
             FROM tree JOIN runs r ON r.id = tree.id JOIN steps s ON s.run_id = r.id
             WHERE s.usage_json IS NOT NULL
             GROUP BY m",
        )?;
        let rows = stmt.query_map(params![session_id, since], |r| {
            let n = |i| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
            Ok((r.get(0)?, (n(1)?, n(2)?, n(3)?, n(4)?), n(5)?))
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
        let conn = self.conn()?;
        let Some(old) = schedule_autonomy(&conn, id)? else {
            return Ok(false);
        };
        conn.execute(
            "UPDATE schedules SET autonomy = ?2 WHERE id = ?1",
            params![id, autonomy],
        )?;
        note_routine_autonomy(&conn, id, old.as_deref(), autonomy)?;
        Ok(true)
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
        self.hold_delivery_as(target, run_id, label, text, release_at, "quiet")
    }

    /// Hold a delivery until `release_at`, for `kind`: `quiet` or `digest`.
    pub fn hold_delivery_as(
        &self,
        target: &str,
        run_id: Option<&str>,
        label: Option<&str>,
        text: &str,
        release_at: i64,
        kind: &str,
    ) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO held_deliveries (target, run_id, label, text, release_at, created_at, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![target, run_id, label, text, release_at, now_ms(), kind],
        )?;
        Ok(())
    }

    /// Held deliveries due by `now`, oldest first.
    pub fn due_held_deliveries(&self, now: i64) -> Result<Vec<HeldDelivery>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, target, run_id, label, text, release_at, kind FROM held_deliveries
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
                kind: r.get(6)?,
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

    // ---- routines (T1b) ----

    /// A `once` routine after its run: off, never due again.
    pub fn finish_once(&self, id: &str, ran_at: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE schedules SET last_run_at = ?2, next_run_at = ?3, enabled = 0 WHERE id = ?1",
            params![id, ran_at, i64::MAX],
        )?;
        Ok(())
    }

    /// Record what a poll or gate found (`last_check`).
    pub fn set_routine_check(&self, id: &str, check: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE schedules SET last_check = ?2, last_check_at = ?3 WHERE id = ?1",
            params![id, check, now_ms()],
        )?;
        Ok(())
    }

    /// A watch routine's cursor: `None` before its first poll, else the
    /// JSON its watch last returned (`null` included).
    pub fn watch_cursor(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT watch_cursor FROM schedules WHERE id = ?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Record one poll in one transaction: the events (a key seen before is
    /// ignored), then the cursor and `check`. A crash before the commit
    /// keeps the old cursor, so the next poll finds the same events and
    /// the dedup key absorbs them. New `pending` events past `max` become
    /// `batch`: one run for all of them.
    pub fn record_poll(
        &self,
        id: &str,
        events: &[NewEvent],
        cursor: &str,
        check: &str,
        max: usize,
    ) -> Result<PollRecord> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let now = now_ms();
        let mut rec = PollRecord::default();
        for e in events {
            let state = if e.state == "pending" && rec.to_run >= max {
                "batch"
            } else {
                e.state
            };
            let added = tx.execute(
                "INSERT OR IGNORE INTO routine_events (routine_id, event_key, payload, state, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, e.key, e.payload, state, now],
            )?;
            if added > 0 {
                rec.new += 1;
                if e.state == "pending" {
                    rec.to_run += 1;
                }
            }
        }
        tx.execute(
            "UPDATE schedules SET watch_cursor = ?2, last_check = ?3, last_check_at = ?4 WHERE id = ?1",
            params![id, cursor, check, now],
        )?;
        tx.commit()?;
        Ok(rec)
    }

    /// Routines with events waiting for a run, oldest event first.
    pub fn routines_with_waiting_events(&self) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT routine_id FROM routine_events WHERE state IN ('pending', 'batch')
             GROUP BY routine_id ORDER BY min(created_at)",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The events the next run of routine `id` takes: its oldest `pending`
    /// event alone, or else every `batch` event.
    pub fn next_routine_events(&self, id: &str) -> Result<Vec<RoutineEvent>> {
        let conn = self.conn()?;
        let one = |state: &str, limit: i64| -> Result<Vec<RoutineEvent>> {
            let mut stmt = conn.prepare(
                "SELECT routine_id, event_key, payload, state, run_id, created_at FROM routine_events
                 WHERE routine_id = ?1 AND state = ?2 ORDER BY created_at, rowid LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![id, state, limit], event_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        let pending = one("pending", 1)?;
        if !pending.is_empty() {
            return Ok(pending);
        }
        one("batch", i64::MAX)
    }

    /// Events of routine `id`, newest first.
    pub fn routine_events(&self, id: &str, limit: u32) -> Result<Vec<RoutineEvent>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT routine_id, event_key, payload, state, run_id, created_at FROM routine_events
             WHERE routine_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![id, limit], event_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Hand events to run `run_id`; their payloads are in its input now.
    pub fn mark_events_started(&self, id: &str, keys: &[String], run_id: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        for k in keys {
            tx.execute(
                "UPDATE routine_events SET state = 'started', run_id = ?3, payload = ''
                 WHERE routine_id = ?1 AND event_key = ?2",
                params![id, k, run_id],
            )?;
        }
        tx.commit()?;
        Ok(())
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

/// A routine's own autonomy: `None` if there is no such routine.
fn schedule_autonomy(conn: &rusqlite::Connection, id: &str) -> Result<Option<Option<String>>> {
    Ok(conn
        .query_row("SELECT autonomy FROM schedules WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?)
}

/// Record a routine's autonomy change (the ledger, tooling spec §8.1).
fn note_routine_autonomy(
    conn: &rusqlite::Connection,
    id: &str,
    old: Option<&str>,
    new: Option<&str>,
) -> Result<()> {
    if old == new {
        return Ok(());
    }
    let shown = |a: Option<&str>| a.unwrap_or("the configured profile").to_owned();
    let e = crate::LedgerEntry::new(
        "autonomy.routine",
        id,
        format!("routine {id}: {} → {}", shown(old), shown(new)),
    )
    .routine(id)
    .detail(serde_json::json!({ "value": new, "was": old }));
    crate::ledger_repo::append_on(conn, &e)?;
    Ok(())
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

    fn watch_routine(s: &Store) {
        s.create_session("wsess", "cron", None).unwrap();
        s.create_schedule(&ScheduleRow {
            id: "w".into(),
            prompt: "new PR".into(),
            session_id: "wsess".into(),
            enabled: true,
            trigger: "watch".into(),
            watch: Some("github.reviews".into()),
            every_secs: Some(300),
            ..ScheduleRow::default()
        })
        .unwrap();
    }

    fn ev(key: &str, state: &'static str) -> NewEvent {
        NewEvent {
            key: key.into(),
            payload: format!("{{\"id\":\"{key}\"}}"),
            state,
        }
    }

    #[test]
    fn a_new_routine_is_a_cron_routine_unless_it_says_otherwise() {
        let s = Store::open_in_memory().unwrap();
        s.create_session("sess", "cron", None).unwrap();
        s.create_schedule(&ScheduleRow {
            id: "c".into(),
            cron: "0 8 * * *".into(),
            prompt: "brief".into(),
            session_id: "sess".into(),
            ..ScheduleRow::default()
        })
        .unwrap();
        let c = s.schedule("c").unwrap().unwrap();
        assert_eq!((c.trigger.as_str(), c.trigger()), ("cron", "cron"));
        assert_eq!(c.initiative, None, "new routines take the default");
        s.finish_once("c", 5).unwrap();
        let c = s.schedule("c").unwrap().unwrap();
        assert!(!c.enabled && c.next_run_at == i64::MAX && c.last_run_at == Some(5));
    }

    #[test]
    fn polls_dedup_events_and_save_the_cursor_with_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let s = Store::open(&path).unwrap();
            watch_routine(&s);
            assert_eq!(s.watch_cursor("w").unwrap(), None, "never polled");
            let first = s
                .record_poll(
                    "w",
                    &[ev("1", "baseline"), ev("2", "baseline")],
                    "{\"n\":2}",
                    "baseline",
                    20,
                )
                .unwrap();
            assert_eq!(first, PollRecord { new: 2, to_run: 0 });
            assert!(s.routines_with_waiting_events().unwrap().is_empty());
        }
        // Across a restart, a key seen before never runs again.
        let s = Store::open(&path).unwrap();
        assert_eq!(s.watch_cursor("w").unwrap().as_deref(), Some("{\"n\":2}"));
        let again = s
            .record_poll(
                "w",
                &[ev("2", "pending"), ev("3", "pending"), ev("3", "pending")],
                "{\"n\":3}",
                "1 new",
                20,
            )
            .unwrap();
        assert_eq!(again, PollRecord { new: 1, to_run: 1 });
        assert_eq!(s.routines_with_waiting_events().unwrap(), ["w"]);
        let next = s.next_routine_events("w").unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(
            (next[0].key.as_str(), next[0].state.as_str()),
            ("3", "pending")
        );
        s.mark_events_started("w", &["3".into()], "run1").unwrap();
        assert!(s.next_routine_events("w").unwrap().is_empty());
        let all = s.routine_events("w", 10).unwrap();
        let three = all.iter().find(|e| e.key == "3").unwrap();
        assert_eq!(
            (
                three.state.as_str(),
                three.run_id.as_deref(),
                three.payload.as_str()
            ),
            ("started", Some("run1"), "")
        );
        let sched = s.schedule("w").unwrap().unwrap();
        assert_eq!(sched.last_check.as_deref(), Some("1 new"));
        // Deleting the routine deletes its events.
        assert!(s.delete_schedule("w").unwrap());
        assert!(s.routine_events("w", 10).unwrap().is_empty());
    }

    #[test]
    fn events_past_the_limit_collapse_into_one_batch() {
        let s = Store::open_in_memory().unwrap();
        watch_routine(&s);
        let evs: Vec<NewEvent> = (0..5).map(|i| ev(&i.to_string(), "pending")).collect();
        let rec = s.record_poll("w", &evs, "null", "5 new", 2).unwrap();
        assert_eq!(rec, PollRecord { new: 5, to_run: 5 });
        let mut keys = Vec::new();
        loop {
            let next = s.next_routine_events("w").unwrap();
            if next.is_empty() {
                break;
            }
            let k: Vec<String> = next.iter().map(|e| e.key.clone()).collect();
            s.mark_events_started("w", &k, "r").unwrap();
            keys.push(k);
        }
        assert_eq!(keys, [vec!["0"], vec!["1"], vec!["2", "3", "4"]]);
    }

    #[test]
    fn digests_are_held_with_their_kind() {
        let s = Store::open_in_memory().unwrap();
        s.hold_delivery_as("telegram:1", None, None, "d", 10, "digest")
            .unwrap();
        s.hold_delivery("telegram:1", None, None, "q", 10).unwrap();
        let kinds: Vec<String> = s
            .due_held_deliveries(20)
            .unwrap()
            .into_iter()
            .map(|h| h.kind)
            .collect();
        assert_eq!(kinds, ["digest", "quiet"]);
    }
}
