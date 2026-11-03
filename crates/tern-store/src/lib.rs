//! SQLite storage: connection setup, schema migrations, and repositories.

mod daemon_repo;
mod devices_repo;
mod migrations;
mod repo;
mod subagents_repo;

pub use daemon_repo::ScheduleRow;
pub use devices_repo::{DeviceGrant, DeviceRow};
pub use subagents_repo::{MailRow, NewMail};

pub use repo::{
    MemoryRow, MessageHit, MessageRow, NewMessage, NewRun, RunRow, SessionRow, SessionSummary,
    StepRow, fts_query, now_ms,
};

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;

pub use migrations::LATEST_VERSION;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(
        "database schema version {found} is newer than this binary supports ({supported}); upgrade tern"
    )]
    TooNew { found: u32, supported: u32 },
    #[error("database connection lock poisoned")]
    Poisoned,
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// Shared handle to the tern database. Cheap to clone.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Open (creating if needed) the database at `path` and migrate it.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrations::migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }

    pub fn schema_version(&self) -> Result<u32> {
        migrations::user_version(&*self.conn()?)
    }

    /// Run `PRAGMA integrity_check`; returns the problems found (empty if OK).
    pub fn integrity_check(&self) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("PRAGMA integrity_check")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let problems: Vec<String> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(problems.into_iter().filter(|r| r != "ok").collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_db_is_migrated_and_healthy() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap(), LATEST_VERSION);
        assert!(s.integrity_check().unwrap().is_empty());
    }

    #[test]
    fn reopening_file_db_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.db");
        drop(Store::open(&p).unwrap());
        let s = Store::open(&p).unwrap();
        assert_eq!(s.schema_version().unwrap(), LATEST_VERSION);
        let mode: String = s
            .conn()
            .unwrap()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn refuses_newer_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.db");
        Connection::open(&p)
            .unwrap()
            .pragma_update(None, "user_version", LATEST_VERSION + 1)
            .unwrap();
        assert!(matches!(Store::open(&p), Err(StoreError::TooNew { .. })));
    }

    #[test]
    fn fts_triggers_track_messages_and_memories() {
        let s = Store::open_in_memory().unwrap();
        let c = s.conn().unwrap();
        c.execute_batch(
            "INSERT INTO sessions VALUES ('s1', NULL, 'cli', NULL, NULL, 0, 0);
             INSERT INTO messages (session_id, role, content_json, text, created_at)
               VALUES ('s1', 'user', '[]', 'the quick brown fox', 0);
             INSERT INTO memories (content, tags, created_at)
               VALUES ('user prefers dark roast coffee', 'prefs', 0);",
        )
        .unwrap();
        let hits = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            hits("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'brown'"),
            1
        );
        assert_eq!(
            hits("SELECT count(*) FROM memories_fts WHERE memories_fts MATCH 'coffee'"),
            1
        );

        c.execute("UPDATE messages SET text = 'a lazy dog' WHERE id = 1", [])
            .unwrap();
        assert_eq!(
            hits("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'brown'"),
            0
        );
        assert_eq!(
            hits("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'lazy'"),
            1
        );

        c.execute("DELETE FROM memories", []).unwrap();
        assert_eq!(
            hits("SELECT count(*) FROM memories_fts WHERE memories_fts MATCH 'coffee'"),
            0
        );
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let s = Store::open_in_memory().unwrap();
        let err = s
            .conn()
            .unwrap()
            .execute(
                "INSERT INTO runs (id, session_id, status, trigger, started_at, updated_at) VALUES ('r', 'missing', 'running', 'user', 0, 0)",
                [],
            )
            .unwrap_err();
        assert!(err.to_string().contains("FOREIGN KEY"), "{err}");
    }
}
