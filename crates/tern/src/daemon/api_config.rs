//! `config.toml` as settings, for the config editor. Owner only (`/v1/config`
//! is in `OWNER_ONLY`). Providers, MCP servers and the Telegram gateway
//! have their own routes, and no secret value passes through here.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/config/settings` | `{path, error?, restart_required, sections: [{id, title, description?, settings: [Setting]}]}` |
//! | PUT | `/v1/config/settings/{key}` | set `{value}` (`null` unsets an optional one) → the `Setting` |
//! | DELETE | `/v1/config/settings/{key}` | back to the default (removed from the file) → the `Setting` |
//!
//! A `Setting` is `{key, kind, choices?, min?, max?, optional, value,
//! default, set, description, applies: "live" | "restart",
//! restart_pending, restart_required, security?, risky, editable,
//! group, advanced}`. `group` places it in the platform Settings window
//! (`general`, `models`, `permissions`, `limits`, `servers`, `advanced`);
//! `advanced` marks tuning knobs. Secrets are never returned: `*_secret`
//! keys hold only a secret's name, are not editable here and refuse writes
//! (400); values go through `/v1/secrets`, write-only. Writes keep the file's
//! comments and layout, and are refused (400) when the value has the wrong
//! type or range, or the config wouldn't load with it; unknown keys are
//! 404.

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use tern_core::config::edit::EditError;
use tern_core::config::settings::{ConfigView, Setting, SettingError};

use super::api::{ApiError, ApiResult, Shared, bad, internal};

pub fn routes() -> Router<Shared> {
    Router::new().route("/v1/config/settings", get(list)).route(
        "/v1/config/settings/{key}",
        axum::routing::put(set).delete(reset),
    )
}

fn error(e: SettingError) -> ApiError {
    match e {
        SettingError::Unknown(_) => ApiError(StatusCode::NOT_FOUND, e.to_string()),
        SettingError::Edit(EditError::Write { .. }) => internal(e),
        _ => bad(e),
    }
}

async fn list(State(m): State<Shared>) -> Json<ConfigView> {
    Json(m.rt.config_view())
}

#[derive(Deserialize)]
struct SetBody {
    value: Value,
}

async fn set(
    State(m): State<Shared>,
    UrlPath(key): UrlPath<String>,
    Json(b): Json<SetBody>,
) -> ApiResult<Setting> {
    m.rt.change_setting(&key, Some(&b.value))
        .map(Json)
        .map_err(error)
}

async fn reset(State(m): State<Shared>, UrlPath(key): UrlPath<String>) -> ApiResult<Setting> {
    m.rt.change_setting(&key, None).map(Json).map_err(error)
}
