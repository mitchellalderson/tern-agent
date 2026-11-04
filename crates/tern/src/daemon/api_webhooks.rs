//! Webhook routines over the API (T1c). Owner-only (`auth::OWNER_ONLY`):
//! a webhook secret lets the internet start runs. The deliveries
//! themselves arrive on the webhook listener (`hooks`), never here.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/webhooks` | every webhook routine's hook: URL, auth, state; never secrets |
//! | POST | `/v1/webhooks` | create `{prompt, auth?: hmac|bearer, …the fields of POST /v1/schedules but a trigger}` → `{schedule, webhook}` with the secret, shown this once |
//! | POST | `/v1/webhooks/{id}/rotate` | `{auth?}` → a new secret at once (the old one stops working); also turns a revoked hook back on |
//! | DELETE | `/v1/webhooks/{id}` | revoke: the URL answers 404 until rotated |
//! | POST | `/v1/webhooks/{id}/fire` | deliver the request body locally (`tern hooks fire`), through the gate and dedup like a real delivery |
//!
//! Changing the routine's prompt, gate, limits or `enabled` is
//! `PATCH /v1/schedules/{id}`; giving it another trigger there revokes its
//! secret.

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_store::{ScheduleRow, WebhookRow};
use tern_tools::schedule::TriggerArgs;

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};
use super::api_schedules::{CreateBody, Schedule, create_row, view};
use super::hooks;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/webhooks", get(list).post(create))
        .route("/v1/webhooks/{id}", delete(revoke))
        .route("/v1/webhooks/{id}/rotate", post(rotate))
        .route("/v1/webhooks/{id}/fire", post(fire))
}

/// A routine's hook. `secret` only in the answer that made it.
#[derive(Debug, Serialize, Deserialize)]
pub struct Hook {
    pub routine_id: String,
    /// `hmac` or `bearer`.
    pub auth: String,
    /// `/v1/hooks/<id>` on the webhook listener.
    pub path: String,
    /// The full URL, when `[webhooks]` has a listener or public URL.
    pub url: Option<String>,
    /// False once revoked.
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    pub created_at: i64,
    pub rotated_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub last_delivery_at: Option<i64>,
}

fn hook(m: &Shared, w: WebhookRow, secret: Option<String>) -> Hook {
    Hook {
        path: format!("{}{}", hooks::PATH_PREFIX, w.routine_id),
        url: hooks::url(&m.rt.config, &w.routine_id),
        active: w.secret.is_some(),
        routine_id: w.routine_id,
        auth: w.auth,
        secret,
        created_at: w.created_at,
        rotated_at: w.rotated_at,
        revoked_at: w.revoked_at,
        last_delivery_at: w.last_delivery_at,
    }
}

async fn list(State(m): State<Shared>) -> ApiResult<Vec<Hook>> {
    let rows = m.rt.store.webhooks().map_err(internal)?;
    Ok(Json(rows.into_iter().map(|w| hook(&m, w, None)).collect()))
}

#[derive(Deserialize)]
struct CreateHook {
    auth: Option<String>,
    #[serde(flatten)]
    routine: CreateBody,
}

#[derive(Serialize, Deserialize)]
pub struct Created {
    pub schedule: Schedule,
    pub webhook: Hook,
}

/// Give routine `id` a fresh secret of kind `auth` and answer with it.
fn new_secret(m: &Shared, id: &str, auth: &str) -> Result<Hook, ApiError> {
    let secret = hooks::new_secret().map_err(internal)?;
    m.rt.store
        .set_webhook_secret(id, auth, &secret)
        .map_err(internal)?;
    let w =
        m.rt.store
            .webhook(id)
            .map_err(internal)?
            .ok_or_else(|| not_found("webhook"))?;
    tracing::info!(target: "audit", routine = %id, auth, "webhook secret issued");
    Ok(hook(m, w, Some(secret)))
}

async fn create(
    State(m): State<Shared>,
    Json(mut b): Json<CreateHook>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    let t = &b.routine.trigger;
    if t.cron.is_some() || t.at.is_some() || t.watch.is_some() || t.every.is_some() {
        return Err(bad("a webhook routine has no cron, at or watch"));
    }
    let auth = hooks::check_auth(b.auth.as_deref().unwrap_or_default()).map_err(bad)?;
    b.routine.trigger = TriggerArgs::webhook();
    let row = create_row(&m, b.routine).await?;
    let webhook = new_secret(&m, &row.id, auth)?;
    Ok((
        StatusCode::CREATED,
        Json(Created {
            schedule: view(&m, row),
            webhook,
        }),
    ))
}

fn webhook_routine(m: &Shared, id: &str) -> Result<ScheduleRow, ApiError> {
    m.rt.store
        .schedule(id)
        .map_err(internal)?
        .filter(|s| s.trigger() == "webhook")
        .ok_or_else(|| not_found("webhook routine"))
}

#[derive(Deserialize, Default)]
struct RotateBody {
    auth: Option<String>,
}

async fn rotate(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    body: Option<Json<RotateBody>>,
) -> ApiResult<Hook> {
    webhook_routine(&m, &id)?;
    let old = m.rt.store.webhook(&id).map_err(internal)?;
    let auth = match body.and_then(|Json(b)| b.auth) {
        Some(a) => hooks::check_auth(&a).map_err(bad)?,
        None => hooks::check_auth(old.as_ref().map_or("", |w| w.auth.as_str())).map_err(bad)?,
    };
    Ok(Json(new_secret(&m, &id, auth)?))
}

async fn revoke(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Value> {
    webhook_routine(&m, &id)?;
    if m.rt.store.revoke_webhook(&id).map_err(internal)? {
        tracing::info!(target: "audit", routine = %id, "webhook secret revoked");
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(not_found("active webhook"))
    }
}

/// A local delivery: the owner is the sender, so no secret, but the same
/// gate, dedup, taint and run as one from the internet.
async fn fire(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Value> {
    let s = webhook_routine(&m, &id)?;
    if !s.enabled {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "this routine is turned off".into(),
        ));
    }
    // Firing the same body twice is two deliveries; the time keeps them apart.
    let mut signed = format!("local.{}.", tern_store::now_ms()).into_bytes();
    signed.extend_from_slice(&body);
    let outcome = hooks::accept(&m, &s, hooks::event(&headers, &body), &signed)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "status": outcome.as_str() })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture the Dart client decodes is exactly this API's shape.
    #[test]
    fn dart_fixture_matches_the_api() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/desktop/packages/tern_client/test/fixtures/webhook.json"
        );
        let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let parsed: Hook = serde_json::from_value(fixture.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), fixture);
    }
}
