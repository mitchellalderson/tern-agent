//! Forward-only schema migrations tracked in `PRAGMA user_version`.
//!
//! Each entry upgrades the schema from version `i` to `i + 1`. Never edit a
//! shipped migration; append a new one.

use rusqlite::Connection;

use crate::{Result, StoreError};

const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_run_context.sql"),
    include_str!("../migrations/0003_compaction.sql"),
    include_str!("../migrations/0004_daemon.sql"),
    include_str!("../migrations/0005_devices.sql"),
    include_str!("../migrations/0006_subagents.sql"),
    include_str!("../migrations/0007_schedule_limits.sql"),
    include_str!("../migrations/0008_channels.sql"),
    include_str!("../migrations/0009_taint.sql"),
    include_str!("../migrations/0010_channel_c4.sql"),
    include_str!("../migrations/0011_channel_moves.sql"),
    include_str!("../migrations/0012_outbox.sql"),
    include_str!("../migrations/0013_agents.sql"),
    include_str!("../migrations/0014_egress.sql"),
    include_str!("../migrations/0015_routines.sql"),
    include_str!("../migrations/0016_channel_agents.sql"),
    include_str!("../migrations/0017_webhooks.sql"),
    include_str!("../migrations/0018_connections.sql"),
    include_str!("../migrations/0019_channel_suggestions.sql"),
    include_str!("../migrations/0020_ledger.sql"),
    include_str!("../migrations/0021_skill_offers.sql"),
    include_str!("../migrations/0022_agent_autonomy.sql"),
    include_str!("../migrations/0023_let_agents_decide.sql"),
    include_str!("../migrations/0024_agent_tombstones.sql"),
    include_str!("../migrations/0025_starter_seed.sql"),
];

/// The migration that introduced first-run seeding.
const SEED_VERSION: usize = 25;

pub const LATEST_VERSION: u32 = MIGRATIONS.len() as u32;

pub(crate) fn user_version(conn: &Connection) -> Result<u32> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    let current = user_version(conn)?;
    if current > LATEST_VERSION {
        return Err(StoreError::TooNew {
            found: current,
            supported: LATEST_VERSION,
        });
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        // An install that predates first-run seeding never gets seeded.
        if i + 1 == SEED_VERSION && current > 0 {
            tx.execute(
                "INSERT OR IGNORE INTO kv (namespace, key, value, updated_at)
                 VALUES ('tern', 'starters_seeded', 'existing install', 0)",
                [],
            )?;
        }
        tx.pragma_update(None, "user_version", i as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upgrade an in-memory database to `version` only.
    fn migrate_to(conn: &mut Connection, version: usize) {
        for (i, sql) in MIGRATIONS.iter().enumerate().take(version) {
            let tx = conn.transaction().unwrap();
            tx.execute_batch(sql).unwrap();
            tx.pragma_update(None, "user_version", i as u32 + 1)
                .unwrap();
            tx.commit().unwrap();
        }
    }

    #[test]
    fn channels_backfill_puts_everything_in_home() {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_to(&mut c, 7);
        c.execute_batch(
            "INSERT INTO sessions (id, source, created_at, updated_at) VALUES
               ('old-api', 'api', 0, 10), ('new-api', 'api', 0, 30), ('tg', 'telegram', 0, 50),
               ('cron', 'cron', 0, 40), ('child', 'subagent', 0, 60);
             INSERT INTO runs (id, session_id, status, trigger, started_at, updated_at)
               VALUES ('r', 'new-api', 'completed', 'user', 0, 0);
             INSERT INTO runs (id, session_id, status, trigger, started_at, updated_at, parent_run_id)
               VALUES ('rc', 'child', 'completed', 'subagent', 0, 0, 'r');
             INSERT INTO schedules (id, cron, prompt, session_id, next_run_at)
               VALUES ('s', '0 8 * * *', 'brief', 'cron', 0);",
        )
        .unwrap();
        migrate(&mut c).unwrap();
        let one = |sql: &str| -> String { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(one("SELECT kind FROM channels WHERE id = 'home'"), "main");
        assert_eq!(
            count("SELECT count(*) FROM sessions WHERE channel_id IS NOT 'home'"),
            0
        );
        assert_eq!(
            count("SELECT count(*) FROM schedules WHERE channel_id = 'home'"),
            1
        );
        // The newest desktop conversation is Home's first segment; the
        // Telegram chat, though newer, is not the desktop's.
        assert_eq!(
            one("SELECT main_session_id FROM channels WHERE id = 'home'"),
            "new-api"
        );
        assert_eq!(
            one("SELECT segment_reason FROM sessions WHERE id = 'new-api'"),
            "migrated"
        );
        assert_eq!(
            count("SELECT count(*) FROM sessions WHERE segment_reason IS NOT NULL"),
            1
        );
        // Messages aren't turned into posts.
        assert_eq!(count("SELECT count(*) FROM posts"), 0);
    }

    #[test]
    fn channels_backfill_without_a_desktop_conversation_leaves_home_empty() {
        let mut c = Connection::open_in_memory().unwrap();
        migrate(&mut c).unwrap();
        let main: Option<String> = c
            .query_row(
                "SELECT main_session_id FROM channels WHERE id = 'home'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(main, None);
    }

    #[test]
    fn agents_backfill_makes_the_existing_setup_the_tern_agent() {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_to(&mut c, 12);
        c.execute_batch(
            "INSERT INTO sessions (id, source, created_at, updated_at, channel_id)
               VALUES ('a', 'api', 0, 10, 'home'), ('cron', 'cron', 0, 20, 'home');
             INSERT INTO channels (id, slug, title, kind, created_by, created_at, updated_at)
               VALUES ('c1', 'finances', 'Finances', 'topic', 'user', 0, 0);
             INSERT INTO schedules (id, cron, prompt, session_id, next_run_at, channel_id)
               VALUES ('s', '0 8 * * *', 'brief', 'cron', 0, 'home');
             INSERT INTO memories (content, created_at) VALUES ('likes tea', 0);",
        )
        .unwrap();
        migrate(&mut c).unwrap();
        let one = |sql: &str| -> String { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(one("SELECT memory FROM agents WHERE id = 'tern'"), "shared");
        assert_eq!(count("SELECT count(*) FROM agents"), 1);
        assert_eq!(
            one("SELECT agent_id FROM channels WHERE id = 'home'"),
            "tern"
        );
        // No backfill writes: NULL is read as tern everywhere else.
        for sql in [
            "SELECT count(*) FROM sessions WHERE agent_id IS NOT NULL",
            "SELECT count(*) FROM schedules WHERE agent_id IS NOT NULL",
            "SELECT count(*) FROM memories WHERE agent_id IS NOT NULL",
            "SELECT count(*) FROM channels WHERE id <> 'home' AND agent_id IS NOT NULL",
            "SELECT count(*) FROM channel_agents",
        ] {
            assert_eq!(count(sql), 0, "{sql}");
        }
    }

    #[test]
    fn routines_backfill_makes_every_schedule_a_cron_routine_that_pushes() {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_to(&mut c, 13);
        c.execute_batch(
            "INSERT INTO sessions (id, source, created_at, updated_at, channel_id)
               VALUES ('cron', 'cron', 0, 20, 'home');
             INSERT INTO schedules (id, cron, prompt, session_id, next_run_at, channel_id, urgent)
               VALUES ('s', '0 8 * * *', 'brief', 'cron', 0, 'home', 1);
             INSERT INTO held_deliveries (target, text, release_at, created_at)
               VALUES ('telegram:1', 'held', 0, 0);",
        )
        .unwrap();
        migrate(&mut c).unwrap();
        let row: (String, String, Option<String>, i64, i64) = c
            .query_row(
                "SELECT trigger_kind, initiative, watch_cursor, urgent, backfill FROM schedules WHERE id = 's'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        // Before routines, every result was pushed: that is `interrupt`.
        assert_eq!(row, ("cron".into(), "interrupt".into(), None, 1, 0));
        let kind: String = c
            .query_row("SELECT kind FROM held_deliveries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "quiet");
        let events: i64 = c
            .query_row("SELECT count(*) FROM routine_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 0);
    }
}
