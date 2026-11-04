//! MCP servers over the API (docs/mcp-management-spec.md §7). Secret
//! values travel only in request bodies and are never returned.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/mcp` | `[McpStatus]` |
//! | GET | `/v1/mcp/{name}` | one `McpStatus`, with its stderr tail |
//! | POST | `/v1/mcp/verify` | test `{name, config, secrets?}`: `{ok, verified}` or `{ok: false, error}`; saves nothing |
//! | POST | `/v1/mcp/import` | `{text}` → `[ImportedServer]`; saves nothing |
//! | POST | `/v1/mcp` | `{name, config, secrets?}`: store secrets, write config, start it. 409 if the name exists |
//! | PUT | `/v1/mcp/{name}` | replace `{name?, config, secrets?}`; a different `name` renames |
//! | PATCH | `/v1/mcp/{name}` | `{enabled?, disabled_tools?, requires_approval?, idempotent?}` |
//! | DELETE | `/v1/mcp/{name}?remove_secrets=` | remove a server |
//! | POST | `/v1/mcp/{name}/restart` | reconnect now; returns the new `McpStatus` |
//!
//! Reads need Read scope, everything else Owner (`/v1/mcp` isn't in
//! `CHAT_WRITES`). A `stdio` server runs a command on this host outside the
//! sandbox, so paired devices may add, change or test one only with
//! `api.remote_mcp_stdio = true`. Every state change sends `mcp_changed`
//! on `/v1/events`.

use std::collections::BTreeMap;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::config::{McpServerConfig, McpTransport};
use tern_core::mcp::{ImportedServer, McpPatch, McpStatus};

use super::api::{ApiError, ApiResult, Shared, bad, not_found};
use super::api_providers::limited;
use super::auth::Auth;
use crate::mcp::McpError;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/mcp", get(list).post(add))
        .route("/v1/mcp/verify", post(verify))
        .route("/v1/mcp/import", post(import))
        .route(
            "/v1/mcp/{name}",
            get(show).put(replace).patch(patch).delete(remove),
        )
        .route("/v1/mcp/{name}/restart", post(restart))
}

/// Refuse `stdio` configs from paired devices unless the host allows it.
fn stdio_allowed(
    m: &Shared,
    auth: Option<&Extension<Auth>>,
    cfg: &McpServerConfig,
) -> Result<(), ApiError> {
    if cfg.transport != McpTransport::Stdio || auth.is_none() || m.rt.live().api.remote_mcp_stdio {
        return Ok(());
    }
    Err(ApiError(
        StatusCode::FORBIDDEN,
        "adding local commands from another device is off: a stdio MCP server runs on the \
         host outside the sandbox. Set `api.remote_mcp_stdio = true` on the host to allow it."
            .into(),
    ))
}

fn write_err(e: anyhow::Error) -> ApiError {
    match e.downcast_ref::<McpError>() {
        Some(McpError::NotFound(_)) => ApiError(StatusCode::NOT_FOUND, e.to_string()),
        Some(McpError::Exists(_)) => ApiError(StatusCode::CONFLICT, e.to_string()),
        _ => bad(e),
    }
}

async fn list(State(m): State<Shared>) -> ApiResult<Vec<McpStatus>> {
    Ok(Json(m.rt.mcp_statuses()))
}

async fn show(State(m): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult<McpStatus> {
    m.rt.mcp_status(&name)
        .map(Json)
        .ok_or_else(|| not_found("MCP server"))
}

#[derive(Deserialize)]
struct Draft {
    name: String,
    config: McpServerConfig,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

async fn verify(
    State(m): State<Shared>,
    auth: Option<Extension<Auth>>,
    Json(d): Json<Draft>,
) -> ApiResult<Value> {
    stdio_allowed(&m, auth.as_ref(), &d.config)?;
    limited(auth.as_ref())?;
    Ok(Json(
        match m.rt.verify_mcp(&d.name, &d.config, d.secrets).await {
            Ok(v) => json!({ "ok": true, "verified": v }),
            Err(e) => json!({ "ok": false, "error": e }),
        },
    ))
}

#[derive(Deserialize)]
struct ImportBody {
    text: String,
}

async fn import(
    State(m): State<Shared>,
    Json(b): Json<ImportBody>,
) -> ApiResult<Vec<ImportedServer>> {
    m.rt.import_mcp(&b.text).map(Json).map_err(bad)
}

async fn add(
    State(m): State<Shared>,
    auth: Option<Extension<Auth>>,
    Json(d): Json<Draft>,
) -> Result<(StatusCode, Json<McpStatus>), ApiError> {
    stdio_allowed(&m, auth.as_ref(), &d.config)?;
    let s =
        m.rt.save_mcp(None, &d.name, &d.config, &d.secrets)
            .map_err(write_err)?;
    Ok((StatusCode::CREATED, Json(s)))
}

#[derive(Deserialize)]
struct ReplaceBody {
    name: Option<String>,
    config: McpServerConfig,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

async fn replace(
    State(m): State<Shared>,
    auth: Option<Extension<Auth>>,
    UrlPath(old): UrlPath<String>,
    Json(b): Json<ReplaceBody>,
) -> ApiResult<McpStatus> {
    stdio_allowed(&m, auth.as_ref(), &b.config)?;
    let name = b.name.unwrap_or_else(|| old.clone());
    m.rt.save_mcp(Some(&old), &name, &b.config, &b.secrets)
        .map(Json)
        .map_err(write_err)
}

async fn patch(
    State(m): State<Shared>,
    UrlPath(name): UrlPath<String>,
    Json(p): Json<McpPatch>,
) -> ApiResult<McpStatus> {
    m.rt.patch_mcp(&name, &p).map(Json).map_err(write_err)
}

#[derive(Deserialize)]
struct RemoveQuery {
    #[serde(default)]
    remove_secrets: bool,
}

async fn remove(
    State(m): State<Shared>,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<Value> {
    m.rt.remove_mcp(&name, q.remove_secrets)
        .map_err(write_err)?;
    Ok(Json(json!({ "ok": true })))
}

async fn restart(State(m): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult<McpStatus> {
    m.rt.restart_mcp(&name).await.map(Json).map_err(write_err)
}
