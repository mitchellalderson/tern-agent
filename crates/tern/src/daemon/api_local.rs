//! Models that run on this computer (docs/local-models.md).
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/local/status` | engine state: installed, running, version, who started it, context, disk, warnings, and the install plan and progress |
//! | GET | `/v1/local/models` | installed models: size, family, quantization, `tools`, `thinking`, `max_context`, `loaded` |
//! | DELETE | `/v1/local/models/{name}` | remove an installed model |
//! | GET | `/v1/local/pulls` | model downloads, running and recent: `{id, model, phase, completed, total, error}` |
//! | POST | `/v1/local/pull` | `{model}`: start a download (one at a time; needs Ollama running and 10 GB to spare); answers the download |
//! | POST | `/v1/local/pulls/{id}/cancel` | stop a download (a later pull resumes it) |
//! | GET | `/v1/local/catalog` | the recommended model per tier (agent, fast, decision) for this Mac: `{ram_bytes, tiers: [{tier, model, size_gb, blurb, note, installed, current}], setting_up, error}` |
//! | POST | `/v1/local/tiers` | `{tier, model}`: use an installed model for a tier (refused when it can't use tools and the tier needs them); answers the catalog |
//! | POST | `/v1/local/setup` | download what's missing from the recommended set, one at a time, then use each for its tier; progress is in `/pulls` and `setting_up` |
//! | POST | `/v1/local/start` | start the engine unless one already answers (a running one is reused as is) |
//! | POST | `/v1/local/stop` | stop an engine tern started; one the user runs is left alone |
//! | POST | `/v1/local/install` | `{consent: true}`: download, verify and install Ollama in the background; progress is in `status.installing` |
//!
//! Every change sends `local_changed` on `/v1/events`; a download also
//! sends `local_pull` as it goes, and when one ends the `ollama` provider's
//! model list is refreshed (`models_changed`). Reading is for any
//! device; the rest is owner-only like every write.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::api::{ApiError, ApiResult, Shared};
use crate::local::catalog::Tier;
use crate::local::pulls::Pull;
use crate::local::tiers;
use crate::local::{Error, LocalModel, Status};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/local/status", get(status))
        .route("/v1/local/models", get(models))
        .route("/v1/local/models/{*name}", axum::routing::delete(delete))
        .route("/v1/local/catalog", get(catalog))
        .route("/v1/local/tiers", post(use_for))
        .route("/v1/local/setup", post(set_up))
        .route("/v1/local/pulls", get(pulls))
        .route("/v1/local/pull", post(pull))
        .route("/v1/local/pulls/{id}/cancel", post(cancel))
        .route("/v1/local/start", post(start))
        .route("/v1/local/stop", post(stop))
        .route("/v1/local/install", post(install))
}

pub(super) fn fail(e: Error) -> ApiError {
    let code = match e {
        Error::NotInstalled(_) | Error::Unavailable(_) | Error::Cancelled => StatusCode::CONFLICT,
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

async fn pulls(State(m): State<Shared>) -> Json<Vec<Pull>> {
    Json(m.rt.local.pulls())
}

#[derive(Deserialize)]
struct PullBody {
    model: String,
}

async fn pull(State(m): State<Shared>, Json(b): Json<PullBody>) -> ApiResult<Pull> {
    // Subscribe first, so the end of a fast download isn't missed.
    let mut changes = m.rt.subscribe_changes();
    let job = m.rt.local.pull(&b.model).await.map_err(fail)?;
    let (m2, id) = (m.clone(), job.id.clone());
    tokio::spawn(async move {
        use tern_core::models::Change;
        while let Ok(c) = changes.recv().await {
            if let Change::LocalPull { id: i, phase, .. } = c
                && i == id
                && matches!(phase.as_str(), "done" | "failed" | "cancelled")
            {
                refresh_lists(&m2).await;
                return;
            }
        }
    });
    Ok(Json(job))
}

async fn cancel(State(m): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    m.rt.local.cancel_pull(&id).map_err(fail)?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete(State(m): State<Shared>, Path(name): Path<String>) -> ApiResult<Value> {
    m.rt.local.delete(&name).await.map_err(fail)?;
    refresh_lists(&m).await;
    Ok(Json(json!({ "ok": true })))
}

/// New or removed models change what Ollama providers list.
async fn refresh_lists(m: &Shared) {
    let names: Vec<String> =
        m.rt.live()
            .providers
            .iter()
            .filter(|(_, p)| {
                p.effective_base_url()
                    .is_some_and(tern_providers::ollama::is_ollama_url)
            })
            .map(|(n, _)| n.clone())
            .collect();
    for n in names {
        let _ = m.rt.models(Some(&n), true).await;
    }
}

async fn catalog(State(m): State<Shared>) -> Json<tiers::Catalog> {
    Json(tiers::catalog(&m.rt).await)
}

#[derive(Deserialize)]
struct UseBody {
    tier: Tier,
    model: String,
}

async fn use_for(State(m): State<Shared>, Json(b): Json<UseBody>) -> ApiResult<tiers::Catalog> {
    tiers::use_for(&m.rt, b.tier, &b.model)
        .await
        .map_err(fail)?;
    Ok(Json(tiers::catalog(&m.rt).await))
}

async fn set_up(State(m): State<Shared>) -> ApiResult<tiers::Catalog> {
    if m.rt.local.preset_state().0 {
        return Err(fail(Error::Unavailable("already setting up".into())));
    }
    let rt = m.rt.clone();
    tokio::spawn(async move { tiers::set_up_all(rt).await });
    // The flag is set by the task; show it as running at once.
    let mut c = tiers::catalog(&m.rt).await;
    c.setting_up = true;
    Ok(Json(c))
}
