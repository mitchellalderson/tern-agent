//! Sub-agent runs (docs/subagents-design.md §3.2).
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/runs/{id}/children[?recursive=1]` | the run's sub-agents: `[ChildRun]` |
//! | GET | `/v1/sessions/{id}/subagents` | every sub-agent under the session's runs, at any depth |
//! | POST | `/v1/runs/{id}/message` | `{text}` from the user to a running sub-agent → `{ok: true}` |

use axum::extract::{Path as UrlPath, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::subagent::ChildRun;

use super::api::{ApiResult, Shared, bad, internal, not_found};
use crate::subagents::children_of;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/runs/{id}/children", get(children))
        .route("/v1/runs/{id}/message", post(message))
        .route("/v1/sessions/{id}/subagents", get(session_subagents))
}

#[derive(Deserialize)]
struct ChildrenQuery {
    recursive: Option<String>,
}

async fn children(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<ChildrenQuery>,
) -> ApiResult<Vec<ChildRun>> {
    m.rt.store
        .run(&id)
        .map_err(internal)?
        .ok_or_else(|| not_found("run"))?;
    let recursive = matches!(q.recursive.as_deref(), Some("1" | "true"));
    Ok(Json(
        children_of(&m.rt.store, &id, recursive).map_err(internal)?,
    ))
}

async fn session_subagents(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Vec<ChildRun>> {
    m.rt.store
        .session(&id)
        .map_err(internal)?
        .ok_or_else(|| not_found("session"))?;
    let mut out = Vec::new();
    for run in m.rt.store.runs_for_session(&id).map_err(internal)? {
        out.extend(children_of(&m.rt.store, &run.id, true).map_err(internal)?);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct MessageBody {
    text: String,
}

async fn message(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<MessageBody>,
) -> ApiResult<Value> {
    m.rt.store
        .run(&id)
        .map_err(internal)?
        .ok_or_else(|| not_found("run"))?;
    let Some(host) = &m.rt.subagents else {
        return Err(bad("sub-agents are disabled ([subagents] enabled = false)"));
    };
    host.message_run(&id, &b.text).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}
