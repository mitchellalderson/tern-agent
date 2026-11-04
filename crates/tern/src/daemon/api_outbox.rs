//! The outbox over the API (docs/personal-agent-tooling-spec.md §5.2).
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/outbox[?state=pending\|all\|<state>]` | items, newest first (default `pending`) |
//! | GET | `/v1/outbox/{id}` | one item |
//! | POST | `/v1/outbox/{id}/approve` | `{input?}`: run it once, with `input` replacing its arguments |
//! | POST | `/v1/outbox/{id}/reject` | `{note?}`: the run that queued it is told, if still working |
//!
//! `/v1/events` carries `{"outbox_changed": {"id", "state"}}` when an item
//! is queued or changes state. Local-only items (host commands) can only be
//! decided over the Unix socket.

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use tern_core::outbox::OutboxItem;

use super::api::{ApiError, ApiResult, Shared, internal, not_found};
use super::outbox::{OutboxError, approve_input};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/outbox", get(list))
        .route("/v1/outbox/{id}", get(one))
        .route("/v1/outbox/{id}/approve", post(approve))
        .route("/v1/outbox/{id}/reject", post(reject))
}

#[derive(Deserialize)]
struct ListQuery {
    state: Option<String>,
}

async fn list(State(m): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Vec<OutboxItem>> {
    let state = match q.state.as_deref() {
        None => Some("pending"),
        Some("all") => None,
        Some(s) => Some(s),
    };
    let rows = m.rt.store.outbox(state, 200).map_err(internal)?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

async fn one(State(m): State<Shared>, UrlPath(id): UrlPath<i64>) -> ApiResult<OutboxItem> {
    match m.rt.store.outbox_item(id).map_err(internal)? {
        Some(row) => Ok(Json(row.into())),
        None => Err(not_found("outbox item")),
    }
}

fn status(e: OutboxError) -> ApiError {
    let code = match &e {
        OutboxError::NotFound => StatusCode::NOT_FOUND,
        OutboxError::LocalOnly => StatusCode::FORBIDDEN,
        OutboxError::NotPending(_) => StatusCode::CONFLICT,
        OutboxError::Bad(_) => StatusCode::BAD_REQUEST,
        OutboxError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ApiError(code, e.to_string())
}

async fn approve(
    State(m): State<Shared>,
    auth: Option<axum::Extension<super::auth::Auth>>,
    UrlPath(id): UrlPath<i64>,
    body: Option<Json<Value>>,
) -> ApiResult<OutboxItem> {
    let input = body.and_then(|Json(b)| approve_input(&b));
    m.approve_outbox(id, input, auth.is_some())
        .map(Json)
        .map_err(status)
}

async fn reject(
    State(m): State<Shared>,
    auth: Option<axum::Extension<super::auth::Auth>>,
    UrlPath(id): UrlPath<i64>,
    body: Option<Json<Value>>,
) -> ApiResult<OutboxItem> {
    let note = body.and_then(|Json(b)| b.get("note").and_then(Value::as_str).map(str::to_owned));
    m.reject_outbox(id, note.as_deref(), auth.is_some())
        .map(Json)
        .map_err(status)
}
