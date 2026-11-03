//! Secrets over the API (gui-spec §7.1). Values are write-only: nothing
//! here ever returns one.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/secrets` | every known secret: `[{name, in_file, in_env, used_by}]` |
//! | PUT | `/v1/secrets/{name}` | store `{value}` in `secrets.toml` |
//! | DELETE | `/v1/secrets/{name}` | remove from `secrets.toml` |
//!
//! Changes apply to providers, connectors and newly started MCP servers
//! without a restart; the Telegram gateway reads its token at startup.

use axum::extract::{Path as UrlPath, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use tern_core::SecretStatus;
use tern_core::secrets::SecretError;

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/secrets", get(list))
        .route("/v1/secrets/{name}", axum::routing::put(set).delete(delete))
}

fn error(e: SecretError) -> ApiError {
    match e {
        SecretError::InvalidName(_) | SecretError::EmptyValue(_) => bad(e),
        _ => internal(e),
    }
}

fn status(m: &Shared, name: &str) -> Option<SecretStatus> {
    m.rt.secret_statuses().into_iter().find(|s| s.name == name)
}

async fn list(State(m): State<Shared>) -> ApiResult<Vec<SecretStatus>> {
    Ok(Json(m.rt.secret_statuses()))
}

#[derive(Deserialize)]
struct SetBody {
    value: String,
}

async fn set(
    State(m): State<Shared>,
    UrlPath(name): UrlPath<String>,
    Json(b): Json<SetBody>,
) -> ApiResult<SecretStatus> {
    m.rt.secrets.set(&name, &b.value).map_err(error)?;
    tracing::info!(secret = %name, "secret set");
    status(&m, &name)
        .map(Json)
        .ok_or_else(|| internal("secret vanished after setting it"))
}

async fn delete(
    State(m): State<Shared>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<Option<SecretStatus>> {
    if !m.rt.secrets.remove(&name).map_err(error)? {
        return Err(not_found("secret"));
    }
    tracing::info!(secret = %name, "secret removed");
    // Still listed if something uses it or the environment sets it.
    Ok(Json(status(&m, &name)))
}
