//! Routines: schedules over the API (gui-spec §2.3). The same rules as the
//! `schedule_*` tools, which share `tern_tools::schedule::create_schedule`.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/schedules` | all schedules, with their last run's status |
//! | POST | `/v1/schedules` | create `{cron, prompt, deliver_to?}` |
//! | POST | `/v1/schedules/preview` | `{cron}` → normalized form and next 3 runs |
//! | PATCH | `/v1/schedules/{id}` | change `{cron?, prompt?, enabled?, deliver_to?}` |
//! | DELETE | `/v1/schedules/{id}` | delete |
//! | POST | `/v1/schedules/{id}/run` | run now, without moving the next run |

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_core::cron::Cron;
use tern_store::ScheduleRow;

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/schedules", get(list).post(create))
        .route("/v1/schedules/preview", post(preview))
        .route("/v1/schedules/{id}", axum::routing::patch(update).delete(delete))
        .route("/v1/schedules/{id}/run", post(run_now))
}

#[derive(Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub cron: String,
    pub prompt: String,
    pub session_id: String,
    pub deliver_to: Option<String>,
    pub enabled: bool,
    /// Unix ms; absent when disabled or the schedule never fires again.
    pub next_run_at: Option<i64>,
    pub last_run_at: Option<i64>,
    /// Status of the latest run in the schedule's session.
    pub last_status: Option<String>,
}

fn view(m: &Shared, s: ScheduleRow) -> Schedule {
    let last_status = m
        .rt
        .store
        .runs_for_session(&s.session_id)
        .ok()
        .and_then(|runs| runs.into_iter().last())
        .map(|r| r.status);
    Schedule {
        next_run_at: (s.enabled && s.next_run_at != i64::MAX).then_some(s.next_run_at),
        id: s.id,
        cron: s.cron,
        prompt: s.prompt,
        session_id: s.session_id,
        deliver_to: s.deliver_to,
        enabled: s.enabled,
        last_run_at: s.last_run_at,
        last_status,
    }
}

async fn list(State(m): State<Shared>) -> ApiResult<Vec<Schedule>> {
    let rows = m.rt.store.schedules().map_err(internal)?;
    Ok(Json(rows.into_iter().map(|s| view(&m, s)).collect()))
}

#[derive(Deserialize)]
struct CreateBody {
    cron: String,
    prompt: String,
    /// `none` (default) or `telegram:<chat_id>`.
    deliver_to: Option<String>,
}

fn target(t: Option<&str>) -> Result<Option<String>, ApiError> {
    match t.map(str::trim) {
        None | Some("") | Some("none") => Ok(None),
        Some(t) => {
            tern_tools::schedule::check_deliver_to(t).map_err(bad)?;
            Ok(Some(t.to_owned()))
        }
    }
}

async fn create(
    State(m): State<Shared>,
    Json(b): Json<CreateBody>,
) -> Result<(StatusCode, Json<Schedule>), ApiError> {
    let deliver_to = target(b.deliver_to.as_deref())?;
    let row = tern_tools::schedule::create_schedule(&m.rt.store, &b.cron, &b.prompt, deliver_to)
        .map_err(bad)?;
    Ok((StatusCode::CREATED, Json(view(&m, row))))
}

fn parse(cron: &str) -> Result<Cron, ApiError> {
    cron.parse::<Cron>().map_err(bad)
}

async fn preview(Json(b): Json<Value>) -> ApiResult<Value> {
    let cron = parse(b.get("cron").and_then(Value::as_str).unwrap_or_default())?;
    let mut next = Vec::new();
    let mut at = chrono::Local::now();
    while next.len() < 3 {
        let Some(t) = cron.next_after(at) else { break };
        next.push(t.timestamp_millis());
        at = t;
    }
    Ok(Json(json!({ "cron": cron.to_string(), "next": next })))
}

#[derive(Deserialize)]
struct UpdateBody {
    cron: Option<String>,
    prompt: Option<String>,
    enabled: Option<bool>,
    deliver_to: Option<String>,
}

async fn update(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Schedule> {
    let store = &m.rt.store;
    let mut s = store
        .schedule(&id)
        .map_err(internal)?
        .ok_or_else(|| not_found("schedule"))?;
    let reschedule = b.cron.is_some() || b.enabled == Some(true) && !s.enabled;
    if let Some(c) = b.cron {
        s.cron = parse(&c)?.to_string();
    }
    if let Some(p) = b.prompt {
        if p.trim().is_empty() {
            return Err(bad("prompt is empty"));
        }
        p.trim().clone_into(&mut s.prompt);
    }
    if let Some(e) = b.enabled {
        s.enabled = e;
    }
    if b.deliver_to.is_some() {
        s.deliver_to = target(b.deliver_to.as_deref())?;
    }
    if reschedule {
        // Re-enabling starts from now rather than catching up missed slots.
        s.next_run_at = parse(&s.cron)?
            .next_after(chrono::Local::now())
            .map_or(i64::MAX, |t| t.timestamp_millis());
    }
    store.update_schedule(&s).map_err(internal)?;
    Ok(Json(view(&m, s)))
}

async fn delete(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Value> {
    if m.rt.store.delete_schedule(&id).map_err(internal)? {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(not_found("schedule"))
    }
}

async fn run_now(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Value> {
    let s = m
        .rt
        .store
        .schedule(&id)
        .map_err(internal)?
        .ok_or_else(|| not_found("schedule"))?;
    let started = m
        .start(
            Some(&s.session_id),
            None,
            &s.prompt,
            "cron",
            "cron",
            s.deliver_to.as_deref(),
        )
        .await
        .map_err(bad)?;
    Ok(Json(
        json!({ "run_id": started.run_id, "session_id": started.session_id }),
    ))
}
