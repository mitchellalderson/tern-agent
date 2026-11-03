//! Providers and model lists over the API (docs/provider-setup-spec.md).
//! Keys travel only in request bodies and are never returned.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/providers` | configured providers with key source and last list status |
//! | POST | `/v1/providers/verify` | check a draft `{kind, name?, base_url?, key?, use_env_key?}`; saves nothing |
//! | POST | `/v1/providers` | verify, store the key, write config, reload; `default_model?` |
//! | PATCH | `/v1/providers/{name}` | change key or base URL (verifies first) |
//! | POST | `/v1/providers/{name}/verify` | check a change to an existing provider |
//! | DELETE | `/v1/providers/{name}?remove_secret=` | remove a provider |
//! | GET | `/v1/models?provider=&refresh=` | model lists: `[{provider, error?, message?, fetched_at, models}]` |
//! | PUT | `/v1/config/default_model` | set `{model}`, resolved like `/v1/models/check` |
//! | POST | `/v1/config/reload` | re-read providers and `default_model` from `config.toml` |
//!
//! Every write sends `providers_changed` on `/v1/events`; a fresh model list
//! sends `models_changed`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::api::{ApiError, ApiResult, Shared, bad, internal};
use super::auth::Auth;
use crate::providers::{ProviderDraft, ProviderModels, ProviderStatus, Resolved, Verified};

/// Verifications allowed per device (or local client) per minute, so a
/// paired device can't be used to test keys in bulk.
const VERIFY_PER_MINUTE: usize = 10;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/providers", get(list).post(add))
        .route("/v1/providers/verify", post(verify_new))
        .route(
            "/v1/providers/{name}",
            axum::routing::patch(change).delete(remove),
        )
        .route("/v1/providers/{name}/verify", post(verify_existing))
        .route("/v1/models", get(models))
        .route("/v1/config/default_model", put(set_default))
        .route("/v1/config/reload", post(reload))
}

fn limited(auth: Option<&Extension<Auth>>) -> Result<(), ApiError> {
    static HITS: Mutex<Option<HashMap<String, VecDeque<Instant>>>> = Mutex::new(None);
    let who = match auth.map(|a| &a.0) {
        Some(Auth::Device { id, .. }) => id.clone(),
        None => "local".into(),
    };
    let now = Instant::now();
    let mut hits = HITS.lock().unwrap_or_else(|p| p.into_inner());
    let q = hits
        .get_or_insert_with(HashMap::new)
        .entry(who)
        .or_default();
    while q
        .front()
        .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
    {
        q.pop_front();
    }
    if q.len() >= VERIFY_PER_MINUTE {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "too many key checks; wait a minute".into(),
        ));
    }
    q.push_back(now);
    Ok(())
}

async fn list(State(m): State<Shared>) -> ApiResult<Vec<ProviderStatus>> {
    Ok(Json(m.rt.provider_statuses()))
}

async fn verify_new(
    State(m): State<Shared>,
    auth: Option<Extension<Auth>>,
    Json(d): Json<ProviderDraft>,
) -> ApiResult<Verified> {
    limited(auth.as_ref())?;
    m.rt.verify_provider(&d, None).await.map(Json).map_err(bad)
}

async fn verify_existing(
    State(m): State<Shared>,
    auth: Option<Extension<Auth>>,
    UrlPath(name): UrlPath<String>,
    Json(d): Json<ProviderDraft>,
) -> ApiResult<Verified> {
    limited(auth.as_ref())?;
    m.rt.verify_provider(&d, Some(&name))
        .await
        .map(Json)
        .map_err(bad)
}

#[derive(Deserialize)]
struct SaveBody {
    #[serde(flatten)]
    draft: ProviderDraft,
    default_model: Option<String>,
}

async fn add(
    State(m): State<Shared>,
    auth: Option<Extension<Auth>>,
    Json(b): Json<SaveBody>,
) -> ApiResult<Value> {
    limited(auth.as_ref())?;
    let (provider, verified) =
        m.rt.save_provider(&b.draft, None, b.default_model.as_deref())
            .await
            .map_err(bad)?;
    Ok(Json(json!({ "provider": provider, "verified": verified })))
}

async fn change(
    State(m): State<Shared>,
    auth: Option<Extension<Auth>>,
    UrlPath(name): UrlPath<String>,
    Json(b): Json<SaveBody>,
) -> ApiResult<Value> {
    limited(auth.as_ref())?;
    let (provider, verified) =
        m.rt.save_provider(&b.draft, Some(&name), b.default_model.as_deref())
            .await
            .map_err(bad)?;
    Ok(Json(json!({ "provider": provider, "verified": verified })))
}

#[derive(Deserialize)]
struct RemoveQuery {
    #[serde(default)]
    remove_secret: bool,
}

async fn remove(
    State(m): State<Shared>,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<Value> {
    m.rt.remove_provider(&name, q.remove_secret).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ModelsQuery {
    provider: Option<String>,
    #[serde(default)]
    refresh: bool,
}

async fn models(
    State(m): State<Shared>,
    Query(q): Query<ModelsQuery>,
) -> ApiResult<Vec<ProviderModels>> {
    m.rt.models(q.provider.as_deref(), q.refresh)
        .await
        .map(Json)
        .map_err(bad)
}

#[derive(Deserialize)]
struct DefaultBody {
    model: String,
}

async fn set_default(State(m): State<Shared>, Json(b): Json<DefaultBody>) -> ApiResult<Resolved> {
    m.rt.set_default_model(&b.model).map(Json).map_err(bad)
}

async fn reload(State(m): State<Shared>) -> ApiResult<Value> {
    m.rt.reload_providers().map_err(internal)?;
    Ok(Json(json!({
        "ok": true,
        "reloaded": "providers and default_model; restart the daemon for other settings",
        "providers": m.rt.live().providers.len(),
    })))
}
