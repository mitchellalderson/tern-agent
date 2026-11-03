//! Runs and their journal steps, for the run inspector (gui-spec §4.4).
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/sessions/{id}/runs` | the session's runs, oldest first |
//! | GET | `/v1/runs/{id}/steps` | the run's journal steps |

use axum::extract::{Path as UrlPath, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::api::{ApiResult, Shared, internal, not_found};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/sessions/{id}/runs", get(session_runs))
        .route("/v1/runs/{id}/steps", get(steps))
}

#[derive(Serialize, Deserialize)]
pub struct RunItem {
    pub id: String,
    pub status: String,
    pub trigger: String,
    pub model: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}

async fn session_runs(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Vec<RunItem>> {
    let runs = m.rt.store.runs_for_session(&id).map_err(internal)?;
    Ok(Json(
        runs.into_iter()
            .map(|r| RunItem {
                id: r.id,
                status: r.status,
                trigger: r.trigger,
                model: r.model,
                started_at: r.started_at,
                finished_at: r.finished_at,
                error: r.error,
            })
            .collect(),
    ))
}

#[derive(Serialize, Deserialize)]
pub struct StepItem {
    pub idx: u32,
    /// `llm`, `tool`, `approval`, `compaction`, …
    pub kind: String,
    /// `started`, `completed` or `failed`.
    pub status: String,
    pub input: Value,
    pub output: Option<Value>,
    pub usage: Option<Value>,
    pub error: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

async fn steps(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Vec<StepItem>> {
    m.rt.store
        .run(&id)
        .map_err(internal)?
        .ok_or_else(|| not_found("run"))?;
    let steps = m.rt.store.steps(&id).map_err(internal)?;
    Ok(Json(
        steps
            .into_iter()
            .map(|s| StepItem {
                idx: s.idx,
                kind: s.kind,
                status: s.status,
                input: json(&s.input_json),
                output: s.output_json.as_deref().map(json),
                usage: s.usage_json.as_deref().map(json),
                error: s.error,
                started_at: s.started_at,
                finished_at: s.finished_at,
            })
            .collect(),
    ))
}
