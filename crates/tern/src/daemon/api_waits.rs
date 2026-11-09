//! Waits over the API (docs/wait-until-design.md): what `wait_until` made,
//! so the app can show and cancel it.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/waits[?session=][&agent=]` | active waits, soonest to end first |
//! | DELETE | `/v1/waits/{id}` | cancel one: nothing continues; recorded in the ledger |

use axum::extract::{Path as UrlPath, Query, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_store::WaitRow;
use tern_tools::wait::{Check, Until};

use super::api::{ApiResult, Shared, internal, not_found};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/waits", get(list))
        .route("/v1/waits/{id}", delete(cancel))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wait {
    pub id: String,
    pub note: String,
    pub agent_id: String,
    pub session_id: String,
    /// The conversation's channel: Home, a topic or an agent's DM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// `connector`, `url` or `time`.
    pub check: String,
    /// What it checks: `github.run_status`, `example.com`, `time`.
    pub target: String,
    /// `status == completed`; absent for a time wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    pub every_secs: i64,
    /// When it gives up (ms).
    pub expires_at: i64,
    pub created_at: i64,
    pub checks: i64,
    /// What the last check found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tainted: bool,
}

pub fn view(store: &tern_store::Store, w: &WaitRow) -> Wait {
    let target = Check::from_row(&w.check_kind, &w.check_spec)
        .map(|c| c.label())
        .unwrap_or_else(|_| w.check_kind.clone());
    let until = w
        .until_spec
        .as_deref()
        .and_then(|u| serde_json::from_str::<Until>(u).ok())
        .map(|u| u.describe());
    Wait {
        id: w.id.clone(),
        note: w.note.clone(),
        agent_id: w.agent_id.clone(),
        session_id: w.session_id.clone(),
        channel_id: store
            .session(&w.session_id)
            .ok()
            .flatten()
            .and_then(|s| s.channel_id),
        check: w.check_kind.clone(),
        target,
        until,
        every_secs: w.every_secs,
        expires_at: w.expires_at,
        created_at: w.created_at,
        checks: w.checks,
        last_check: w.last_check.clone(),
        tainted: w.tainted,
    }
}

#[derive(Deserialize)]
struct Filter {
    session: Option<String>,
    agent: Option<String>,
}

async fn list(State(m): State<Shared>, Query(f): Query<Filter>) -> ApiResult<Vec<Wait>> {
    let rows = m.rt.store.waits(f.agent.as_deref()).map_err(internal)?;
    Ok(Json(
        rows.iter()
            .filter(|w| f.session.as_deref().is_none_or(|s| w.session_id == s))
            .map(|w| view(&m.rt.store, w))
            .collect(),
    ))
}

async fn cancel(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Value> {
    let ended =
        m.rt.store
            .end_wait(&id, "wait.cancel", tern_store::ACTOR_OWNER, "cancelled")
            .map_err(internal)?;
    if ended {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(not_found("wait"))
    }
}
