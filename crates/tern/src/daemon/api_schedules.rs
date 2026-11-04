//! Routines: schedules over the API (gui-spec §2.3). The same rules as the
//! `schedule_*` tools, which share `tern_tools::schedule::create_schedule`.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/schedules[?agent=]` | all schedules (or one agent's), with their last run's status |
//! | POST | `/v1/schedules` | create `{cron | at | watch (+ every, backfill, include_self), prompt, gate?, gate_connector?, initiative?, agent?, deliver_to?, channel?, autonomy?, quiet_hours?, urgent?, runs_per_day?, usd_per_day?, tokens_per_run?}` |
//! | POST | `/v1/schedules/preview` | `{cron}` → normalized form and next 3 runs |
//! | PATCH | `/v1/schedules/{id}` | change any of those, `enabled`, or `resume: true` (lift a budget pause); `null` returns a cap or `quiet_hours` to the default |
//!
//! `channel` (slug or id) is where the routine's results are posted
//! (docs/channels-design.md §2.5): by default the owning agent's DM, Home
//! for the default agent. `agent` (id or slug, default `tern`) owns it
//! (docs/agent-profiles-design.md §4): its runs act as that agent.
//! | DELETE | `/v1/schedules/{id}` | delete |
//! | POST | `/v1/schedules/{id}/run` | run now, without moving the next run |

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_core::cron::Cron;
use tern_store::ScheduleRow;
use tern_tools::schedule::{ScheduleChange, ScheduleLimits, TriggerArgs};

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
    /// What starts it (T1b): `cron`, `once`, `watch` or `webhook` (T1c).
    #[serde(default = "cron")]
    pub trigger: String,
    /// The cron expression; empty for other triggers.
    pub cron: String,
    /// Unix ms: when a `once` routine runs (or ran).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
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
    /// Its autonomy profile; absent = the configured `autonomy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<String>,
    /// The agent whose job it is.
    #[serde(default = "default_agent")]
    pub agent_id: String,
    /// `<connector>.<watch>` for a watch trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<String>,
    /// How often a watch is polled, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_secs: Option<i64>,
    /// A watch's first poll starts runs instead of only recording.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub backfill: bool,
    /// Keeps events the watch marks as caused by tern.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_self: bool,
    /// What the last poll or gate found ("2 new events", "gate: skipped").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_at: Option<i64>,
    /// Luau run before each cron or once slot; false skips the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_connector: Option<String>,
    /// `silent`, `digest`, `notify` or `interrupt`; absent = the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiative: Option<String>,
}

fn cron() -> String {
    "cron".into()
}

fn default_agent() -> String {
    tern_store::DEFAULT_AGENT.into()
}

fn home() -> String {
    tern_store::HOME.into()
}

pub(super) fn view(m: &Shared, s: ScheduleRow) -> Schedule {
    let last =
        m.rt.store
            .runs_for_session(&s.session_id)
            .ok()
            .and_then(|runs| runs.into_iter().last());
    let paused_until = s.paused_until.filter(|u| *u > tern_store::now_ms());
    let at = (s.trigger() == "once").then(|| {
        if s.next_run_at == i64::MAX {
            s.last_run_at.unwrap_or(s.next_run_at)
        } else {
            s.next_run_at
        }
    });
    Schedule {
        next_run_at: (s.enabled && s.next_run_at != i64::MAX).then_some(s.next_run_at),
        trigger: s.trigger().to_owned(),
        at,
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
        agent_id: s.agent_id.unwrap_or_else(default_agent),
        channel_id: s.channel_id.unwrap_or_else(home),
        autonomy: s.autonomy,
        watch: s.watch,
        every_secs: s.every_secs,
        backfill: s.backfill,
        include_self: s.include_self,
        last_check: s.last_check,
        last_check_at: s.last_check_at,
        gate: s.gate,
        gate_connector: s.gate_connector,
        initiative: s.initiative,
    }
}

#[derive(Deserialize)]
struct ListQuery {
    agent: Option<String>,
}

async fn list(State(m): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Vec<Schedule>> {
    let agent = q
        .agent
        .as_deref()
        .map(|a| super::api_agents::find(&m, a).map(|a| a.id))
        .transpose()?;
    let rows = m.rt.store.schedules().map_err(internal)?;
    Ok(Json(
        rows.into_iter()
            .filter(|s| agent.as_deref().is_none_or(|a| s.agent() == a))
            .map(|s| view(&m, s))
            .collect(),
    ))
}

/// An agent to own a schedule: any but an archived one.
fn owner(m: &Shared, a: &str) -> Result<String, ApiError> {
    let a = super::api_agents::find(m, a)?;
    if a.archived_at.is_some() {
        return Err(bad(format!("{} is archived", a.name)));
    }
    Ok(a.id)
}

#[derive(Deserialize)]
pub(super) struct CreateBody {
    #[serde(flatten)]
    pub(super) trigger: TriggerArgs,
    prompt: String,
    /// `none` (default) or `telegram:<chat_id>`.
    deliver_to: Option<String>,
    channel: Option<String>,
    agent: Option<String>,
    /// `readonly`, `supervised`, `autonomous`, `yolo`, or `default`.
    autonomy: Option<String>,
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

/// A routine's autonomy from the API: `default` (or empty) clears it.
fn autonomy(a: &str) -> Result<Option<String>, ApiError> {
    match a.trim() {
        "" | "default" => Ok(None),
        a => tern_core::approval::Autonomy::parse(a)
            .map(|a| Some(a.as_str().to_owned()))
            .ok_or_else(|| {
                bad(format!(
                    "autonomy {a:?} must be readonly, supervised, autonomous, yolo or default"
                ))
            }),
    }
}

fn set_autonomy(m: &Shared, s: &mut ScheduleRow, a: Option<&str>) -> Result<(), ApiError> {
    if let Some(a) = a {
        let a = autonomy(a)?;
        m.rt.store
            .set_schedule_autonomy(&s.id, a.as_deref())
            .map_err(internal)?;
        s.autonomy = a;
    }
    Ok(())
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

/// Check a trigger's watch against the active connectors.
async fn check_trigger(
    m: &Shared,
    t: &mut TriggerArgs,
    limits: &ScheduleLimits,
    old: Option<&ScheduleRow>,
) -> Result<(), ApiError> {
    let checks = m.rt.routine_checks().map_err(internal)?;
    if t.watch.is_some() {
        tern_tools::schedule::check_trigger(t, Some(&checks))
            .await
            .map_err(bad)?;
    }
    limits.check(old, Some(&checks)).await.map_err(bad)
}

async fn create(
    State(m): State<Shared>,
    Json(b): Json<CreateBody>,
) -> Result<(StatusCode, Json<Schedule>), ApiError> {
    let row = create_row(&m, b).await?;
    Ok((StatusCode::CREATED, Json(view(&m, row))))
}

/// Create the routine `b` describes (`POST /v1/webhooks` sets a webhook
/// trigger first).
pub(super) async fn create_row(m: &Shared, mut b: CreateBody) -> Result<ScheduleRow, ApiError> {
    let m = m.clone();
    check_trigger(&m, &mut b.trigger, &b.limits, None).await?;
    let deliver_to = target(b.deliver_to.as_deref())?;
    if let Some(a) = &b.autonomy {
        autonomy(a)?;
    }
    let channel = b
        .channel
        .as_deref()
        .map(|c| channel_id(&m, c))
        .transpose()?;
    let agent = match b.agent.as_deref() {
        Some(a) => owner(&m, a)?,
        None => tern_store::DEFAULT_AGENT.into(),
    };
    let mut row = tern_tools::schedule::create_routine(
        &m.rt.store,
        &b.trigger,
        &b.prompt,
        deliver_to,
        &b.limits,
        &agent,
    )
    .map_err(bad)?;
    if let Some(c) = channel {
        m.rt.store
            .set_schedule_channel(&row.id, &c)
            .map_err(internal)?;
        row.channel_id = Some(c);
    }
    join_channel(&m, &row)?;
    set_autonomy(&m, &mut row, b.autonomy.as_deref())?;
    Ok(row)
}

/// A routine posting to a topic channel makes its agent a member there
/// (agent profiles §5.1).
fn join_channel(m: &Shared, s: &tern_store::ScheduleRow) -> Result<(), ApiError> {
    let Some(c) = s.channel_id.as_deref() else {
        return Ok(());
    };
    let channel = m.rt.store.channel(c).map_err(internal)?;
    let agent =
        m.rt.store
            .agent(s.agent_id.as_deref().unwrap_or(tern_store::DEFAULT_AGENT))
            .map_err(internal)?;
    if let (Some(c), Some(a)) = (channel, agent) {
        m.join(&c, &a).map_err(internal)?;
    }
    Ok(())
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
    #[serde(flatten)]
    trigger: TriggerArgs,
    prompt: Option<String>,
    enabled: Option<bool>,
    deliver_to: Option<String>,
    channel: Option<String>,
    /// Hand the job to another agent; its results then go to that agent's
    /// DM unless `channel` says otherwise.
    agent: Option<String>,
    autonomy: Option<String>,
    #[serde(default)]
    resume: bool,
    #[serde(flatten)]
    limits: ScheduleLimits,
}

async fn update(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(mut b): Json<UpdateBody>,
) -> ApiResult<Schedule> {
    let old = m.rt.store.schedule(&id).map_err(internal)?;
    check_trigger(&m, &mut b.trigger, &b.limits, old.as_ref()).await?;
    let deliver_to = match &b.deliver_to {
        Some(t) => Some(target(Some(t))?),
        None => None,
    };
    if let Some(a) = &b.autonomy {
        autonomy(a)?;
    }
    let mut channel = b
        .channel
        .as_deref()
        .map(|c| channel_id(&m, c))
        .transpose()?;
    let agent = b.agent.as_deref().map(|a| owner(&m, a)).transpose()?;
    let ch = ScheduleChange {
        trigger: b.trigger,
        prompt: b.prompt,
        enabled: b.enabled,
        deliver_to,
        limits: b.limits,
        resume: b.resume,
    };
    let mut s = tern_tools::schedule::update_schedule(&m.rt.store, &id, &ch)
        .map_err(bad)?
        .ok_or_else(|| not_found("schedule"))?;
    if let Some(a) = agent.filter(|a| a != s.agent()) {
        m.rt.store.set_schedule_agent(&s.id, &a).map_err(internal)?;
        if channel.is_none() {
            channel =
                m.rt.store
                    .agent_channel(&a)
                    .map_err(internal)?
                    .map(|c| c.id);
        }
        s.agent_id = Some(a).filter(|a| a != tern_store::DEFAULT_AGENT);
    }
    if let Some(c) = channel {
        m.rt.store
            .set_schedule_channel(&s.id, &c)
            .map_err(internal)?;
        s.channel_id = Some(c);
    }
    join_channel(&m, &s)?;
    set_autonomy(&m, &mut s, b.autonomy.as_deref())?;
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
