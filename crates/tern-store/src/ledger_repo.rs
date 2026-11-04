//! The activity ledger (tooling spec §8.1): an append-only, hash-chained
//! log of security-relevant events. Each row stores
//! `hash = sha256(prev_hash || row)`, where `row` is the canonical JSON of
//! its other columns, so editing, deleting or reordering rows in the
//! database is detected by [`Store::ledger_verify`]. Appends happen inside
//! the store calls that make the change (a grant, an outbox decision, a
//! connection), on the same connection, so nothing that changes skips it.
//!
//! What a chain can't show on its own: rows cut off the end. `verify`
//! returns the head hash so it can be noted elsewhere.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Result, Store, now_ms};

/// `prev_hash` of the first entry.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Who caused an entry: the user, the host itself, or an agent.
pub const ACTOR_OWNER: &str = "owner";
pub const ACTOR_TERN: &str = "tern";

/// An entry to append. Build with [`LedgerEntry::new`] and the setters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LedgerEntry {
    pub kind: String,
    pub actor: String,
    pub subject: String,
    pub summary: String,
    pub detail: Option<Value>,
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    pub channel_id: Option<String>,
    pub routine_id: Option<String>,
    pub connector: Option<String>,
}

impl LedgerEntry {
    /// An entry by the user (`owner`); change it with [`Self::actor`].
    pub fn new(kind: &str, subject: &str, summary: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            actor: ACTOR_OWNER.into(),
            subject: subject.into(),
            summary: summary.into(),
            ..Self::default()
        }
    }

    pub fn actor(mut self, actor: &str) -> Self {
        self.actor = actor.into();
        self
    }

    pub fn detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }

    /// The run and its session; the channel and routine are filled in from
    /// them when the entry is appended.
    pub fn run(mut self, run_id: &str, session_id: &str) -> Self {
        self.run_id = Some(run_id.into());
        self.session_id = Some(session_id.into());
        self
    }

    pub fn channel(mut self, channel_id: &str) -> Self {
        self.channel_id = Some(channel_id.into());
        self
    }

    pub fn routine(mut self, routine_id: &str) -> Self {
        self.routine_id = Some(routine_id.into());
        self
    }

    pub fn connector(mut self, connector: &str) -> Self {
        self.connector = Some(connector.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LedgerRow {
    pub seq: i64,
    pub at: i64,
    pub entry: LedgerEntry,
    pub prev_hash: String,
    pub hash: String,
}

/// Which entries to list, newest first.
#[derive(Debug, Clone, Default)]
pub struct LedgerFilter {
    /// A kind (`grant.add`) or a family (`grant`, matching `grant.*`).
    pub kind: Option<String>,
    pub channel: Option<String>,
    pub routine: Option<String>,
    pub connector: Option<String>,
    pub run: Option<String>,
    /// Only entries with `seq` below this (paging back).
    pub before: Option<i64>,
    /// 0 = 100.
    pub limit: u32,
}

/// The result of checking the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerCheck {
    pub entries: u64,
    /// The last entry's hash (`GENESIS` when empty).
    pub head: String,
    /// The first problem found, if any.
    pub broken: Option<LedgerBreak>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerBreak {
    pub seq: i64,
    pub reason: String,
}

const COLS: &str = "seq, at, kind, actor, subject, summary, detail, run_id, session_id, \
                    channel_id, routine_id, connector, prev_hash, hash";

fn row(r: &Row<'_>) -> rusqlite::Result<LedgerRow> {
    let detail: Option<String> = r.get(6)?;
    Ok(LedgerRow {
        seq: r.get(0)?,
        at: r.get(1)?,
        entry: LedgerEntry {
            kind: r.get(2)?,
            actor: r.get(3)?,
            subject: r.get(4)?,
            summary: r.get(5)?,
            // A detail that isn't JSON (tampered) is kept as a string so
            // the hash still sees exactly what is stored.
            detail: detail.map(|d| serde_json::from_str(&d).unwrap_or(Value::String(d))),
            run_id: r.get(7)?,
            session_id: r.get(8)?,
            channel_id: r.get(9)?,
            routine_id: r.get(10)?,
            connector: r.get(11)?,
        },
        prev_hash: r.get(12)?,
        hash: r.get(13)?,
    })
}

/// The stored form of `detail`.
fn detail_text(detail: &Option<Value>) -> Option<String> {
    detail.as_ref().map(|d| match d {
        Value::String(s) if serde_json::from_str::<Value>(s).is_err() => s.clone(),
        d => d.to_string(),
    })
}

/// `sha256(prev_hash || row)`, lowercase hex.
pub fn entry_hash(seq: i64, at: i64, e: &LedgerEntry, prev_hash: &str) -> String {
    let canonical = serde_json::json!([
        seq,
        at,
        e.kind,
        e.actor,
        e.subject,
        e.summary,
        detail_text(&e.detail),
        e.run_id,
        e.session_id,
        e.channel_id,
        e.routine_id,
        e.connector,
    ])
    .to_string();
    let mut h = Sha256::new();
    h.update(prev_hash.as_bytes());
    h.update(canonical.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Append on `conn`, filling the channel and routine from the run's
/// session. Callers hold the store's lock (or a transaction on it).
pub(crate) fn append_on(conn: &Connection, e: &LedgerEntry) -> Result<i64> {
    let mut e = e.clone();
    if let Some(session) = &e.session_id {
        if e.channel_id.is_none() {
            e.channel_id = conn
                .query_row(
                    "SELECT channel_id FROM sessions WHERE id = ?1",
                    [session],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
        }
        if e.routine_id.is_none() {
            e.routine_id = conn
                .query_row(
                    "SELECT id FROM schedules WHERE session_id = ?1 ORDER BY id LIMIT 1",
                    [session],
                    |r| r.get(0),
                )
                .optional()?;
        }
    }
    // Another process (a CLI command beside the daemon) may append between
    // the read and the insert; `prev_hash` is unique, so retry.
    let mut attempt = 0;
    loop {
        let (seq, prev): (i64, String) = conn
            .query_row(
                "SELECT seq, hash FROM ledger ORDER BY seq DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, GENESIS.to_owned()));
        let seq = seq + 1;
        let at = now_ms();
        let hash = entry_hash(seq, at, &e, &prev);
        let inserted = conn.execute(
            &format!(
                "INSERT INTO ledger ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
            ),
            params![
                seq,
                at,
                e.kind,
                e.actor,
                e.subject,
                e.summary,
                detail_text(&e.detail),
                e.run_id,
                e.session_id,
                e.channel_id,
                e.routine_id,
                e.connector,
                prev,
                hash
            ],
        );
        match inserted {
            Ok(_) => return Ok(seq),
            Err(rusqlite::Error::SqliteFailure(f, _))
                if f.code == rusqlite::ErrorCode::ConstraintViolation && attempt < 5 =>
            {
                attempt += 1;
            }
            Err(err) => return Err(err.into()),
        }
    }
}

impl Store {
    /// Append an entry. Returns its `seq`.
    pub fn ledger_append(&self, e: &LedgerEntry) -> Result<i64> {
        append_on(&*self.conn()?, e)
    }

    /// Record a setting kept outside the database (`config.toml`'s
    /// `autonomy`) when it differs from the last value recorded for it, so
    /// a hand edit shows up the next time tern reads the file. Returns
    /// whether an entry was added.
    pub fn ledger_note_setting(&self, kind: &str, subject: &str, value: &str) -> Result<bool> {
        let conn = self.conn()?;
        let last: Option<String> = conn
            .query_row(
                "SELECT json_extract(detail, '$.value') FROM ledger
                 WHERE kind = ?1 AND subject = ?2 ORDER BY seq DESC LIMIT 1",
                params![kind, subject],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if last.as_deref() == Some(value) {
            return Ok(false);
        }
        let summary = match &last {
            Some(old) => format!("{subject}: {old} → {value}"),
            None => format!("{subject} is {value}"),
        };
        let e = LedgerEntry::new(kind, subject, summary)
            .actor(ACTOR_OWNER)
            .detail(serde_json::json!({ "value": value, "was": last }));
        append_on(&conn, &e)?;
        Ok(true)
    }

    /// Entries matching `f`, newest first.
    pub fn ledger(&self, f: &LedgerFilter) -> Result<Vec<LedgerRow>> {
        let mut sql = format!("SELECT {COLS} FROM ledger WHERE 1 = 1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut arg = |sql: &mut String, clause: &str, v: Box<dyn rusqlite::ToSql>| {
            args.push(v);
            sql.push_str(&clause.replace('?', &format!("?{}", args.len())));
        };
        if let Some(k) = &f.kind {
            if k.contains('.') {
                arg(&mut sql, " AND kind = ?", Box::new(k.clone()));
            } else {
                arg(&mut sql, " AND (kind = ? ", Box::new(k.clone()));
                arg(&mut sql, "OR kind LIKE ?)", Box::new(format!("{k}.%")));
            }
        }
        if let Some(c) = &f.channel {
            arg(&mut sql, " AND channel_id = ?", Box::new(c.clone()));
        }
        if let Some(r) = &f.routine {
            arg(&mut sql, " AND routine_id = ?", Box::new(r.clone()));
        }
        if let Some(c) = &f.connector {
            arg(&mut sql, " AND connector = ?", Box::new(c.clone()));
        }
        if let Some(r) = &f.run {
            arg(&mut sql, " AND run_id = ?", Box::new(r.clone()));
        }
        if let Some(b) = f.before {
            arg(&mut sql, " AND seq < ?", Box::new(b));
        }
        let limit = if f.limit == 0 { 100 } else { f.limit };
        sql.push_str(&format!(" ORDER BY seq DESC LIMIT {limit}"));
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Recompute the chain from the first entry and report the first
    /// entry whose hash, link or sequence doesn't hold.
    pub fn ledger_verify(&self) -> Result<LedgerCheck> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM ledger ORDER BY seq"))?;
        let mut rows = stmt.query([])?;
        let mut prev = GENESIS.to_owned();
        let mut expected_seq = 1;
        let mut entries = 0;
        let mut broken = None;
        while let Some(r) = rows.next()? {
            let r = row(r)?;
            entries += 1;
            if broken.is_some() {
                continue;
            }
            let reason = if r.seq != expected_seq {
                Some(format!(
                    "entry {expected_seq} is missing (the next one is {})",
                    r.seq
                ))
            } else if r.prev_hash != prev {
                Some("it doesn't link to the entry before it".to_owned())
            } else if entry_hash(r.seq, r.at, &r.entry, &r.prev_hash) != r.hash {
                Some("its contents don't match its hash".to_owned())
            } else {
                None
            };
            if let Some(reason) = reason {
                broken = Some(LedgerBreak { seq: r.seq, reason });
            }
            prev = r.hash;
            expected_seq = r.seq + 1;
        }
        Ok(LedgerCheck {
            entries,
            head: prev,
            broken,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(n: usize) -> Store {
        let s = Store::open_in_memory().unwrap();
        for i in 0..n {
            s.ledger_append(
                &LedgerEntry::new("grant.add", "shell", format!("always allow git {i}"))
                    .detail(serde_json::json!({ "pattern": format!("git {i}") })),
            )
            .unwrap();
        }
        s
    }

    /// Lift the append-only triggers, as someone editing the file would.
    fn unguard(s: &Store) {
        s.conn()
            .unwrap()
            .execute_batch("DROP TRIGGER ledger_no_update; DROP TRIGGER ledger_no_delete;")
            .unwrap();
    }

    #[test]
    fn the_chain_verifies() {
        let s = store_with(5);
        let check = s.ledger_verify().unwrap();
        assert_eq!(check.entries, 5);
        assert_eq!(check.broken, None);
        let rows = s.ledger(&LedgerFilter::default()).unwrap();
        assert_eq!(rows[0].hash, check.head);
        assert_eq!(rows[4].prev_hash, GENESIS);
        assert_eq!(rows[3].prev_hash, rows[4].hash);
    }

    #[test]
    fn an_empty_ledger_verifies() {
        let s = Store::open_in_memory().unwrap();
        let check = s.ledger_verify().unwrap();
        assert_eq!((check.entries, check.head.as_str()), (0, GENESIS));
        assert_eq!(check.broken, None);
    }

    #[test]
    fn the_database_refuses_edits() {
        let s = store_with(2);
        let c = s.conn().unwrap();
        let err = c
            .execute("UPDATE ledger SET summary = 'x' WHERE seq = 1", [])
            .unwrap_err();
        assert!(err.to_string().contains("append-only"), "{err}");
        assert!(c.execute("DELETE FROM ledger", []).is_err());
    }

    #[test]
    fn an_edited_entry_is_detected() {
        let s = store_with(4);
        unguard(&s);
        s.conn()
            .unwrap()
            .execute(
                "UPDATE ledger SET summary = 'always allow rm -rf' WHERE seq = 2",
                [],
            )
            .unwrap();
        let broken = s.ledger_verify().unwrap().broken.unwrap();
        assert_eq!(broken.seq, 2);
        assert!(broken.reason.contains("don't match"), "{}", broken.reason);
    }

    #[test]
    fn an_edited_detail_is_detected() {
        let s = store_with(3);
        unguard(&s);
        s.conn()
            .unwrap()
            .execute(
                "UPDATE ledger SET detail = '{\"pattern\":\"*\"}' WHERE seq = 3",
                [],
            )
            .unwrap();
        assert_eq!(s.ledger_verify().unwrap().broken.unwrap().seq, 3);
    }

    #[test]
    fn a_deleted_entry_is_detected() {
        let s = store_with(4);
        unguard(&s);
        s.conn()
            .unwrap()
            .execute("DELETE FROM ledger WHERE seq = 2", [])
            .unwrap();
        let broken = s.ledger_verify().unwrap().broken.unwrap();
        assert_eq!(broken.seq, 3);
        assert!(broken.reason.contains("missing"), "{}", broken.reason);
    }

    #[test]
    fn a_rehashed_entry_breaks_the_next_link() {
        // Rewriting an entry and its own hash still breaks the entry after.
        let s = store_with(3);
        unguard(&s);
        let rows = s.ledger(&LedgerFilter::default()).unwrap();
        let mut forged = rows[1].clone(); // seq 2
        forged.entry.summary = "nothing to see".into();
        let hash = entry_hash(forged.seq, forged.at, &forged.entry, &forged.prev_hash);
        s.conn()
            .unwrap()
            .execute(
                "UPDATE ledger SET summary = ?1, hash = ?2 WHERE seq = 2",
                params![forged.entry.summary, hash],
            )
            .unwrap();
        let broken = s.ledger_verify().unwrap().broken.unwrap();
        assert_eq!(broken.seq, 3);
        assert!(broken.reason.contains("link"), "{}", broken.reason);
    }

    #[test]
    fn entries_take_the_runs_channel_and_routine() {
        let s = Store::open_in_memory().unwrap();
        s.conn()
            .unwrap()
            .execute_batch(
                "INSERT INTO sessions (id, source, created_at, updated_at, channel_id)
                   VALUES ('s1', 'cron', 0, 0, 'home');
                 INSERT INTO schedules (id, cron, prompt, session_id, next_run_at)
                   VALUES ('brief', '0 7 * * *', 'p', 's1', 0);",
            )
            .unwrap();
        s.ledger_append(&LedgerEntry::new("effect", "write_file", "wrote x").run("r1", "s1"))
            .unwrap();
        s.ledger_append(&LedgerEntry::new("grant.add", "shell", "always").connector("gmail"))
            .unwrap();
        let by = |f: LedgerFilter| s.ledger(&f).unwrap().len();
        let row = &s.ledger(&LedgerFilter::default()).unwrap()[1];
        assert_eq!(row.entry.channel_id.as_deref(), Some("home"));
        assert_eq!(row.entry.routine_id.as_deref(), Some("brief"));
        let f = |k: &str| LedgerFilter {
            kind: Some(k.into()),
            ..LedgerFilter::default()
        };
        assert_eq!(by(f("grant")), 1);
        assert_eq!(by(f("grant.add")), 1);
        assert_eq!(by(f("gra")), 0);
        assert_eq!(
            by(LedgerFilter {
                routine: Some("brief".into()),
                ..LedgerFilter::default()
            }),
            1
        );
        assert_eq!(
            by(LedgerFilter {
                connector: Some("gmail".into()),
                ..LedgerFilter::default()
            }),
            1
        );
        assert_eq!(
            by(LedgerFilter {
                before: Some(2),
                ..LedgerFilter::default()
            }),
            1
        );
        assert_eq!(s.ledger_verify().unwrap().broken, None);
    }

    #[test]
    fn settings_are_noted_only_when_they_change() {
        let s = Store::open_in_memory().unwrap();
        assert!(
            s.ledger_note_setting("autonomy", "config", "supervised")
                .unwrap()
        );
        assert!(
            !s.ledger_note_setting("autonomy", "config", "supervised")
                .unwrap()
        );
        assert!(s.ledger_note_setting("autonomy", "config", "yolo").unwrap());
        let rows = s.ledger(&LedgerFilter::default()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].entry.summary, "config: supervised → yolo");
    }

    #[test]
    fn a_second_writer_extends_the_chain() {
        // Two handles on one file (the daemon and a CLI command).
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.db");
        let a = Store::open(&p).unwrap();
        let b = Store::open(&p).unwrap();
        for i in 0..4 {
            let s = if i % 2 == 0 { &a } else { &b };
            s.ledger_append(&LedgerEntry::new("grant.remove", "shell", "x"))
                .unwrap();
        }
        let check = a.ledger_verify().unwrap();
        assert_eq!((check.entries, check.broken), (4, None));
    }

    #[test]
    fn store_changes_are_recorded() {
        let s = Store::open_in_memory().unwrap();
        s.conn()
            .unwrap()
            .execute_batch(
                "INSERT INTO sessions (id, source, created_at, updated_at) VALUES ('s1', 'cron', 0, 0);
                 INSERT INTO schedules (id, cron, prompt, session_id, next_run_at)
                   VALUES ('hook', '', 'p', 's1', 0);",
            )
            .unwrap();
        s.add_allowlist("lua__gmail__send", "*").unwrap();
        s.add_allowlist("lua__gmail__send", "*").unwrap(); // already there: nothing
        s.remove_allowlist("lua__gmail__send", "*").unwrap();
        let c = s
            .add_connection(&crate::NewConnection {
                kind: "connector".into(),
                target: "gmail".into(),
                slug: "work".into(),
                account: Some("me@work.com".into()),
                auth_method: "oauth".into(),
                ..Default::default()
            })
            .unwrap();
        s.set_default_connection(&c.id).unwrap();
        s.remove_connection(&c.id).unwrap();
        s.set_webhook_secret("hook", "hmac", "tern_whk_1").unwrap();
        s.set_webhook_secret("hook", "hmac", "tern_whk_2").unwrap();
        s.revoke_webhook("hook").unwrap();
        s.set_schedule_autonomy("hook", Some("yolo")).unwrap();
        s.set_schedule_autonomy("hook", Some("yolo")).unwrap(); // unchanged: nothing
        let d = crate::DeviceRow {
            id: "d1".into(),
            name: "phone".into(),
            platform: None,
            scope: "chat".into(),
            created_at: 0,
            last_seen_at: None,
            last_ip: None,
            revoked_at: None,
        };
        s.create_device(&d, "h").unwrap();
        s.revoke_device("d1").unwrap();

        let mut rows = s.ledger(&LedgerFilter::default()).unwrap();
        rows.reverse();
        let kinds: Vec<&str> = rows.iter().map(|r| r.entry.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "grant.add",
                "grant.remove",
                "connection.add",
                "connection.default",
                "connection.remove",
                "webhook.create",
                "webhook.rotate",
                "webhook.revoke",
                "autonomy.routine",
                "device.pair",
                "device.revoke",
            ]
        );
        assert_eq!(rows[0].entry.connector.as_deref(), Some("gmail"));
        assert_eq!(rows[2].entry.summary, "added gmail · work (me@work.com)");
        assert_eq!(rows[7].entry.routine_id.as_deref(), Some("hook"));
        // Secrets never reach the ledger.
        assert!(
            rows.iter()
                .all(|r| !format!("{:?}", r.entry).contains("tern_whk"))
        );
        assert_eq!(s.ledger_verify().unwrap().broken, None);
    }
}
