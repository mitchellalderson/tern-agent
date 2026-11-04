//! Routines: schedules over the API (gui-spec §2.3). The same rules as the
//! `schedule_*` tools, which share `tern_tools::schedule::create_schedule`.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/schedules` | all schedules, with their last run's status |
//! | POST | `/v1/schedules` | create `{cron, prompt, deliver_to?, channel?, quiet_hours?, urgent?, runs_per_day?, usd_per_day?, tokens_per_run?}` |
//! | POST | `/v1/schedules/preview` | `{cron}` → normalized form and next 3 runs |
//! | PATCH | `/v1/schedules/{id}` | change any of those, `enabled`, or `resume: true` (lift a budget pause); `null` returns a cap or `quiet_hours` to the default |
//!
//! `channel` (slug or id) is where the routine's results are posted
//! (docs/channels-design.md §2.5); Home by default.
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
use tern_tools::schedule::{ScheduleChange, ScheduleLimits};

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/schedules", get(list).post(create))
        .route("/v1/schedules/preview", post(preview))
        .route(
            "/v1/schedules/{id}",
            axum::routing::patch(update).delete(delete),
        )
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
    /// The latest run answered `NO_REPLY` and delivered nothing.
    #[serde(default)]
    pub last_quiet: bool,
    /// `HH:MM-HH:MM` or `none`; absent = the `[schedules]` default.
    #[serde(default)]
    pub quiet_hours: Option<String>,
    /// Delivers even during quiet hours.
    #[serde(default)]
    pub urgent: bool,
    /// Caps; absent = the `[schedules]` default.
    #[serde(default)]
    pub runs_per_day: Option<u32>,
    #[serde(default)]
    pub usd_per_day: Option<f64>,
    #[serde(default)]
    pub tokens_per_run: Option<u64>,
    /// `budget` while a daily cap has paused it.
    #[serde(default)]
    pub paused: Option<String>,
    /// Unix ms: when a budget pause ends on its own.
    #[serde(default)]
    pub paused_until: Option<i64>,
    /// Which cap paused it: `runs_per_day` or `usd_per_day`.
    #[serde(default)]
    pub paused_reason: Option<String>,
    /// The channel its results are posted to.
    #[serde(default = "home")]
    pub channel_id: String,
}

fn home() -> String {
    tern_store::HOME.into()
}

fn view(m: &Shared, s: ScheduleRow) -> Schedule {
    let last =
        m.rt.store
            .runs_for_session(&s.session_id)
            .ok()
            .and_then(|runs| runs.into_iter().last());
    let paused_until = s.paused_until.filter(|u| *u > tern_store::now_ms());
    Schedule {
        next_run_at: (s.enabled && s.next_run_at != i64::MAX).then_some(s.next_run_at),
        id: s.id,
        cron: s.cron,
        prompt: s.prompt,
        session_id: s.session_id,
        deliver_to: s.deliver_to,
        enabled: s.enabled,
        last_run_at: s.last_run_at,
        last_quiet: last.as_ref().is_some_and(|r| r.quiet),
        last_status: last.map(|r| r.status),
        quiet_hours: s.quiet_hours,
        urgent: s.urgent,
        runs_per_day: s.runs_per_day,
        usd_per_day: s.usd_per_day,
        tokens_per_run: s.tokens_per_run,
        paused: paused_until.map(|_| "budget".to_owned()),
        paused_reason: paused_until.and(s.paused_reason),
        paused_until,
        channel_id: s.channel_id.unwrap_or_else(home),
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
    channel: Option<String>,
    #[serde(flatten)]
    limits: ScheduleLimits,
}

/// A channel for routine results: any but an archived one.
fn channel_id(m: &Shared, c: &str) -> Result<String, ApiError> {
    let c = super::api_channels::find(m, c)?;
    if c.archived_at.is_some() {
        return Err(bad(format!("#{} is archived", c.slug)));
    }
    Ok(c.id)
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
    let channel = b
        .channel
        .as_deref()
        .map(|c| channel_id(&m, c))
        .transpose()?;
    let mut row = tern_tools::schedule::create_schedule(
        &m.rt.store,
        &b.cron,
        &b.prompt,
        deliver_to,
        &b.limits,
    )
    .map_err(bad)?;
    if let Some(c) = channel {
        m.rt.store
            .set_schedule_channel(&row.id, &c)
            .map_err(internal)?;
        row.channel_id = Some(c);
    }
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
    channel: Option<String>,
    #[serde(default)]
    resume: bool,
    #[serde(flatten)]
    limits: ScheduleLimits,
}

async fn update(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Schedule> {
    let deliver_to = match &b.deliver_to {
        Some(t) => Some(target(Some(t))?),
        None => None,
    };
    let channel = b
        .channel
        .as_deref()
        .map(|c| channel_id(&m, c))
        .transpose()?;
    let ch = ScheduleChange {
        cron: b.cron,
        prompt: b.prompt,
        enabled: b.enabled,
        deliver_to,
        limits: b.limits,
        resume: b.resume,
    };
    let mut s = tern_tools::schedule::update_schedule(&m.rt.store, &id, &ch)
        .map_err(bad)?
        .ok_or_else(|| not_found("schedule"))?;
    if let Some(c) = channel {
        m.rt.store
            .set_schedule_channel(&s.id, &c)
            .map_err(internal)?;
        s.channel_id = Some(c);
    }
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
    let s =
        m.rt.store
            .schedule(&id)
            .map_err(internal)?
            .ok_or_else(|| not_found("schedule"))?;
    let started = m.start_scheduled(&s).await.map_err(bad)?;
    Ok(Json(
        json!({ "run_id": started.run_id, "session_id": started.session_id }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture the Dart client decodes is exactly this API's shape.
    #[test]
    fn dart_fixture_matches_the_api() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/desktop/packages/tern_client/test/fixtures/schedule.json"
        );
        let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let parsed: Schedule = serde_json::from_value(fixture.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), fixture);
    }
}
