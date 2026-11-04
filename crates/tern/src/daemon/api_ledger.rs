//! The activity ledger over the API (tooling spec §8.1). Owner-only: it
//! names every account, grant and approval.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/ledger[?kind=&channel=&routine=&connector=&run=&before=&limit=]` | entries, newest first; `kind` is a kind (`grant.add`) or a family (`grant`); page back with `before=<seq>` |
//! | GET | `/v1/ledger/verify` | `{ok, entries, head, broken?: {seq, reason}}`: the hash chain recomputed |
//! | GET | `/v1/security/audit` | `tern security audit --json`: `{findings: [{severity, area, title, fix?, subject?}], high, medium, low}` |

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use tern_store::LedgerFilter;

use super::api::{ApiResult, Shared, internal};
use crate::ledger_cmd::{check_json, row_json};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/ledger", get(list))
        .route("/v1/ledger/verify", get(verify))
        .route("/v1/security/audit", get(audit))
}

/// `tern security audit` against the live config (tooling spec §8.4).
async fn audit(State(m): State<Shared>) -> ApiResult<crate::security_cmd::Audit> {
    let home = m.rt.home.clone();
    let config = m.rt.live();
    let store = m.rt.store.clone();
    // Loading connectors and gateways blocks; keep it off the runtime.
    tokio::task::spawn_blocking(move || crate::security_cmd::audit(&home, &config, Some(&store)))
        .await
        .map(Json)
        .map_err(internal)
}

#[derive(Deserialize)]
struct ListQuery {
    kind: Option<String>,
    channel: Option<String>,
    routine: Option<String>,
    connector: Option<String>,
    run: Option<String>,
    before: Option<i64>,
    limit: Option<u32>,
}

async fn list(State(m): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Vec<Value>> {
    let f = LedgerFilter {
        kind: q.kind.filter(|k| !k.is_empty()),
        channel: q.channel.filter(|c| !c.is_empty()),
        routine: q.routine.filter(|r| !r.is_empty()),
        connector: q.connector.filter(|c| !c.is_empty()),
        run: q.run.filter(|r| !r.is_empty()),
        before: q.before,
        limit: q.limit.unwrap_or(100).min(500),
    };
    let rows = m.rt.store.ledger(&f).map_err(internal)?;
    Ok(Json(rows.iter().map(row_json).collect()))
}

async fn verify(State(m): State<Shared>) -> ApiResult<Value> {
    let check = m.rt.store.ledger_verify().map_err(internal)?;
    Ok(Json(check_json(&check)))
}
