//! `tern ledger`: the activity ledger (tooling spec §8.1), read straight
//! from the database so it works with tern stopped, and `tern ledger
//! verify`, which recomputes the hash chain. Also [`note_config`], which
//! records settings that live in `config.toml` when they change.

use std::process::ExitCode;

use anyhow::Context;
use serde_json::{Value, json};
use tern_core::{Config, TernHome};
use tern_store::{LedgerCheck, LedgerFilter, LedgerRow, Store};

use crate::cli::LedgerCommand;

/// Record `config.toml`'s security settings when they differ from what the
/// ledger last saw: a hand edit shows up the next time tern reads it.
pub fn note_config(store: &Store, config: &Config) {
    let settings = [
        ("autonomy.config", "autonomy", config.autonomy.as_str()),
        (
            "egress.config",
            "taint.egress",
            config.taint.egress.as_str(),
        ),
        (
            "sandbox.network",
            "sandbox.network",
            if config.sandbox.network { "on" } else { "off" },
        ),
    ];
    for (kind, subject, value) in settings {
        if let Err(e) = store.ledger_note_setting(kind, subject, value) {
            tracing::warn!(error = %e, kind, "recording a setting in the ledger");
        }
    }
}

pub fn run(home: &TernHome, command: Option<LedgerCommand>) -> anyhow::Result<ExitCode> {
    let path = home.db_path();
    if !path.exists() {
        println!("The ledger is empty.");
        return Ok(ExitCode::SUCCESS);
    }
    let store = Store::open(&path).with_context(|| format!("opening {}", path.display()))?;
    match command.unwrap_or(LedgerCommand::List(Default::default())) {
        LedgerCommand::List(a) => {
            let f = LedgerFilter {
                kind: a.kind,
                channel: a.channel,
                routine: a.routine,
                connector: a.connector,
                run: a.run,
                before: None,
                limit: a.limit,
            };
            let rows = store.ledger(&f)?;
            if a.json {
                let rows: Vec<Value> = rows.iter().map(row_json).collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!("Nothing in the ledger matches.");
            } else {
                // Oldest at the top, like a log.
                for r in rows.iter().rev() {
                    println!("{}", line(r));
                }
            }
        }
        LedgerCommand::Verify { json } => {
            let check = store.ledger_verify()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&check_json(&check))?);
            } else {
                println!("{}", verdict(&check));
            }
            if check.broken.is_some() {
                return Ok(ExitCode::FAILURE);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `2026-11-04 09:12:03  #41  approval      owner     always allowed: write · …`
pub fn line(r: &LedgerRow) -> String {
    let at = chrono::DateTime::from_timestamp_millis(r.at)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "-".into());
    format!(
        "{at}  #{:<5} {:<18} {:<12} {}",
        r.seq, r.entry.kind, r.entry.actor, r.entry.summary
    )
}

pub fn verdict(c: &LedgerCheck) -> String {
    match &c.broken {
        None => format!(
            "✓ the ledger's {} entries are intact. Head: {}\n  (Note the head somewhere else: entries cut off the end can only be noticed against it.)",
            c.entries, c.head
        ),
        Some(b) => format!(
            "✗ the ledger was changed outside tern at entry #{}: {}. Entries before it are intact.",
            b.seq, b.reason
        ),
    }
}

pub fn row_json(r: &LedgerRow) -> Value {
    let e = &r.entry;
    json!({
        "seq": r.seq,
        "at": r.at,
        "kind": e.kind,
        "actor": e.actor,
        "subject": e.subject,
        "summary": e.summary,
        "detail": e.detail,
        "run_id": e.run_id,
        "session_id": e.session_id,
        "channel_id": e.channel_id,
        "routine_id": e.routine_id,
        "connector": e.connector,
        "prev_hash": r.prev_hash,
        "hash": r.hash,
    })
}

pub fn check_json(c: &LedgerCheck) -> Value {
    json!({
        "ok": c.broken.is_none(),
        "entries": c.entries,
        "head": c.head,
        "broken": c.broken.as_ref().map(|b| json!({ "seq": b.seq, "reason": b.reason })),
    })
}
