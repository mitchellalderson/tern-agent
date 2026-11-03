//! Forward-only schema migrations tracked in `PRAGMA user_version`.
//!
//! Each entry upgrades the schema from version `i` to `i + 1`. Never edit a
//! shipped migration; append a new one.

use rusqlite::Connection;

use crate::{Result, StoreError};

const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_run_context.sql"),
];

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
        tx.pragma_update(None, "user_version", i as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}
