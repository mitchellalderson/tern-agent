//! Models that run on this computer (docs/local-models.md).
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/local/status` | engine state: installed, running, version, who started it, context, disk, warnings, and the install plan and progress |
//! | GET | `/v1/local/models` | installed models: size, family, quantization, `tools`, `thinking`, `max_context`, `loaded` |
//! | POST | `/v1/local/start` | start the engine unless one already answers (a running one is reused as is) |
//! | POST | `/v1/local/stop` | stop an engine tern started; one the user runs is left alone |
//! | POST | `/v1/local/install` | `{consent: true}`: download, verify and install Ollama in the background; progress is in `status.installing` |
//!
//! Every change sends `local_changed` on `/v1/events`. Reading is for any
//! device; the rest is owner-only like every write.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::api::{ApiError, ApiResult, Shared};
use crate::local::{Error, LocalModel, Status};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/local/status", get(status))
        .route("/v1/local/models", get(models))
        .route("/v1/local/start", post(start))
        .route("/v1/local/stop", post(stop))
        .route("/v1/local/install", post(install))
}

pub(super) fn fail(e: Error) -> ApiError {
    let code = match e {
        Error::NotInstalled(_) | Error::Unavailable(_) => StatusCode::CONFLICT,
        Error::Failed(_) => StatusCode::BAD_REQUEST,
    };
    ApiError(code, e.to_string())
}

async fn status(State(m): State<Shared>) -> Json<Status> {
    Json(m.rt.local.status().await)
}

async fn models(State(m): State<Shared>) -> ApiResult<Vec<LocalModel>> {
    m.rt.local.models().await.map(Json).map_err(fail)
}

async fn start(State(m): State<Shared>) -> ApiResult<Status> {
    m.rt.local.start().await.map_err(fail)?;
    Ok(Json(m.rt.local.status().await))
}

async fn stop(State(m): State<Shared>) -> ApiResult<Status> {
    m.rt.local.stop().await.map_err(fail)?;
    Ok(Json(m.rt.local.status().await))
}

#[derive(Deserialize)]
struct InstallBody {
    #[serde(default)]
    consent: bool,
}

async fn install(State(m): State<Shared>, Json(b): Json<InstallBody>) -> ApiResult<Status> {
    m.rt.local.install(b.consent).await.map_err(fail)?;
    Ok(Json(m.rt.local.status().await))
}
