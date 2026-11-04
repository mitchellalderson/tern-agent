//! Queries behind the Tasks view (tooling spec §8.2): it is a query over
//! what exists, not a new model.

use rusqlite::params;

use crate::repo::{RUN_COLS, run_row};
use crate::{Result, RunRow, Store};

impl Store {
    /// Top-level runs that failed since `since`, newest first.
    pub fn failed_runs_since(&self, since: i64, limit: u32) -> Result<Vec<RunRow>> {
        let sql = format!(
            "SELECT {RUN_COLS} FROM runs
             WHERE status = 'failed' AND parent_run_id IS NULL
               AND COALESCE(finished_at, updated_at) >= ?1
             ORDER BY COALESCE(finished_at, updated_at) DESC LIMIT ?2"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![since, limit], run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Watch events waiting for a run, by routine: `(routine, count)`.
    pub fn waiting_routine_events(&self) -> Result<Vec<(String, u32)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT routine_id, count(*) FROM routine_events
             WHERE state IN ('pending', 'batch') AND run_id IS NULL
             GROUP BY routine_id ORDER BY routine_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_runs_and_waiting_events() {
        let s = Store::open_in_memory().unwrap();
        s.conn()
            .unwrap()
            .execute_batch(
                "INSERT INTO sessions (id, source, created_at, updated_at) VALUES ('s', 'cron', 0, 0);
                 INSERT INTO runs (id, session_id, status, trigger, started_at, updated_at, finished_at, error)
                   VALUES ('old', 's', 'failed', 'cron', 0, 0, 10, 'x'),
                          ('new', 's', 'failed', 'cron', 0, 0, 500, 'boom'),
                          ('ok', 's', 'completed', 'cron', 0, 0, 600, NULL);
                 INSERT INTO runs (id, session_id, status, trigger, started_at, updated_at, finished_at, parent_run_id)
                   VALUES ('kid', 's', 'failed', 'cron', 0, 0, 700, 'new');
                 INSERT INTO schedules (id, cron, prompt, session_id, next_run_at)
                   VALUES ('w', '', 'p', 's', 0);
                 INSERT INTO routine_events (routine_id, event_key, payload, state, created_at)
                   VALUES ('w', 'a', '{}', 'pending', 0), ('w', 'b', '{}', 'batch', 0),
                          ('w', 'c', '{}', 'baseline', 0);",
            )
            .unwrap();
        let failed = s.failed_runs_since(100, 10).unwrap();
        assert_eq!(
            failed.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["new"]
        );
        assert_eq!(s.waiting_routine_events().unwrap(), [("w".to_owned(), 2)]);
    }
}
