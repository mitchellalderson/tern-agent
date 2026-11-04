//! `schedule_create`, `schedule_update`, `schedule_list`, `schedule_delete`
//! (spec §12.5). The daemon runs due schedules.

use async_trait::async_trait;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use tern_core::cron::Cron;
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;
use tern_store::{ScheduleRow, Store};

use crate::{args, spec};

/// Schedules are tern's own state: a write the host grants.
const POLICY: ToolPolicy = ToolPolicy::granted_write(false);

fn fmt_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%a %Y-%m-%d %H:%M %Z")
                .to_string()
        })
        .unwrap_or_else(|| "?".into())
}

/// Where results go when the caller says "here": the chat this session is
/// attached to, if any.
fn here(store: &Store, session_id: &str) -> Option<String> {
    let s = store.session(session_id).ok().flatten()?;
    match (s.source.as_str(), s.external_ref) {
        ("telegram", Some(chat)) => Some(format!("telegram:{chat}")),
        _ => None,
    }
}

/// `null` → `Some(None)` (back to the default), absent → `None` (unchanged).
fn some<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// Quiet hours, urgency and caps for one schedule (T1a). Absent fields are
/// left as they are; `null` returns a field to the `[schedules]` default.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ScheduleLimits {
    /// `HH:MM-HH:MM`, `none`, or `default`.
    #[serde(default, deserialize_with = "some")]
    pub quiet_hours: Option<Option<String>>,
    pub urgent: Option<bool>,
    #[serde(default, deserialize_with = "some")]
    pub runs_per_day: Option<Option<u32>>,
    #[serde(default, deserialize_with = "some")]
    pub usd_per_day: Option<Option<f64>>,
    #[serde(default, deserialize_with = "some")]
    pub tokens_per_run: Option<Option<u64>>,
}

impl ScheduleLimits {
    /// Check and copy the given fields onto `row`.
    pub fn apply(&self, row: &mut ScheduleRow) -> Result<(), String> {
        if let Some(q) = &self.quiet_hours {
            row.quiet_hours = match q.as_deref().map(str::trim) {
                None | Some("default") | Some("") => None,
                Some(q) => Some(
                    q.parse::<tern_core::quiet::QuietHours>()
                        .map_err(|e| e.to_string())?
                        .to_string(),
                ),
            };
        }
        if let Some(u) = self.urgent {
            row.urgent = u;
        }
        if let Some(n) = self.runs_per_day {
            if n == Some(0) {
                return Err("runs_per_day must be at least 1 (or null for the default)".into());
            }
            row.runs_per_day = n;
        }
        if let Some(usd) = self.usd_per_day {
            if usd.is_some_and(|u| !u.is_finite() || u <= 0.0) {
                return Err("usd_per_day must be more than 0 (or null for the default)".into());
            }
            row.usd_per_day = usd;
        }
        if let Some(t) = self.tokens_per_run {
            if t == Some(0) {
                return Err("tokens_per_run must be at least 1 (or null for the default)".into());
            }
            row.tokens_per_run = t;
        }
        Ok(())
    }
}

/// The JSON schema properties for [`ScheduleLimits`], for tool specs.
fn limit_properties() -> serde_json::Map<String, Value> {
    let props = json!({
        "quiet_hours": { "type": "string", "description": "local \"HH:MM-HH:MM\" when results wait until the window ends, \"none\", or \"default\" (the user's setting, normally 22:00-07:30). Only set it when the user asks." },
        "urgent": { "type": "boolean", "description": "deliver even during quiet hours. Only when the user says it's urgent." },
        "runs_per_day": { "type": ["integer", "null"], "description": "cap; when reached the schedule pauses until tomorrow" },
        "usd_per_day": { "type": ["number", "null"], "description": "spend cap per day, in USD" },
        "tokens_per_run": { "type": ["integer", "null"], "description": "a run that uses more tokens is stopped" }
    });
    match props {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

/// Validate and save a schedule with its own `cron` session. `deliver_to`
/// is already resolved (`None` = keep results in the session).
pub fn create_schedule(
    store: &Store,
    cron: &str,
    prompt: &str,
    deliver_to: Option<String>,
    limits: &ScheduleLimits,
) -> Result<ScheduleRow, String> {
    let cron: Cron = cron
        .parse()
        .map_err(|e: tern_core::cron::CronError| e.to_string())?;
    if prompt.trim().is_empty() {
        return Err("prompt is empty".into());
    }
    if let Some(t) = &deliver_to {
        check_deliver_to(t)?;
    }
    let next = cron
        .next_after(chrono::Local::now())
        .ok_or("that schedule never fires")?;
    let mut row = ScheduleRow {
        id: format!("sch_{}", &tern_core::agent::new_id()[20..]),
        cron: cron.to_string(),
        prompt: prompt.trim().to_owned(),
        session_id: tern_core::agent::new_id(),
        deliver_to,
        enabled: true,
        next_run_at: next.timestamp_millis(),
        ..ScheduleRow::default()
    };
    limits.apply(&mut row)?;
    store
        .create_session(&row.session_id, "cron", None)
        .and_then(|_| store.create_schedule(&row))
        .map_err(|e| format!("saving schedule: {e}"))?;
    Ok(row)
}

/// Changes to a schedule; `None` leaves a field as it is.
#[derive(Debug, Default, Clone)]
pub struct ScheduleChange {
    pub cron: Option<String>,
    pub prompt: Option<String>,
    pub enabled: Option<bool>,
    /// Already resolved: `Some(None)` stops delivering.
    pub deliver_to: Option<Option<String>>,
    pub limits: ScheduleLimits,
    /// Lift a budget pause now (re-enabling does too).
    pub resume: bool,
}

/// Apply `ch` to schedule `id`. `Ok(None)` if there is no such schedule.
pub fn update_schedule(
    store: &Store,
    id: &str,
    ch: &ScheduleChange,
) -> Result<Option<ScheduleRow>, String> {
    let Some(mut s) = store.schedule(id).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let enabling = ch.enabled == Some(true) && !s.enabled;
    let reschedule = ch.cron.is_some() || enabling;
    if let Some(c) = &ch.cron {
        s.cron = c.parse::<Cron>().map_err(|e| e.to_string())?.to_string();
    }
    if let Some(p) = &ch.prompt {
        if p.trim().is_empty() {
            return Err("prompt is empty".into());
        }
        p.trim().clone_into(&mut s.prompt);
    }
    if let Some(e) = ch.enabled {
        s.enabled = e;
    }
    if let Some(t) = &ch.deliver_to {
        if let Some(t) = t {
            check_deliver_to(t)?;
        }
        s.deliver_to.clone_from(t);
    }
    ch.limits.apply(&mut s)?;
    if reschedule {
        // Re-enabling starts from now rather than catching up missed slots.
        s.next_run_at = s
            .cron
            .parse::<Cron>()
            .map_err(|e| e.to_string())?
            .next_after(chrono::Local::now())
            .map_or(i64::MAX, |t| t.timestamp_millis());
    }
    store.update_schedule(&s).map_err(|e| e.to_string())?;
    if ch.resume || enabling {
        store
            .unpause_schedule(id, true)
            .map_err(|e| e.to_string())?;
    }
    store.schedule(id).map_err(|e| e.to_string())
}

/// Quiet hours, urgency, caps and pause state, for listings.
pub fn describe_limits(s: &ScheduleRow) -> String {
    let mut parts = Vec::new();
    if let Some(until) = s.paused_until.filter(|u| *u > tern_store::now_ms()) {
        parts.push(format!(
            "paused: budget ({}) until {}",
            s.paused_reason.as_deref().unwrap_or("daily cap"),
            fmt_time(until)
        ));
    }
    if let Some(q) = &s.quiet_hours {
        parts.push(format!("quiet hours {q}"));
    }
    if s.urgent {
        parts.push("urgent".into());
    }
    if let Some(n) = s.runs_per_day {
        parts.push(format!("{n} runs/day"));
    }
    if let Some(u) = s.usd_per_day {
        parts.push(format!("${u}/day"));
    }
    if let Some(t) = s.tokens_per_run {
        parts.push(format!("{t} tokens/run"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// A concrete delivery target: `telegram:<chat_id>`.
pub fn check_deliver_to(t: &str) -> Result<(), String> {
    match t.strip_prefix("telegram:") {
        Some(chat) if chat.parse::<i64>().is_ok() => Ok(()),
        _ => Err(format!(
            "deliver_to {t:?} must be \"here\", \"none\", or \"telegram:<chat_id>\""
        )),
    }
}

pub struct ScheduleCreate {
    pub store: Store,
}

#[derive(Deserialize)]
struct CreateArgs {
    cron: String,
    prompt: String,
    deliver_to: Option<String>,
    #[serde(flatten)]
    limits: ScheduleLimits,
}

/// "here" (or nothing) is the chat this session is attached to.
fn resolve_target(store: &Store, session_id: &str, t: Option<&str>) -> Option<String> {
    match t.map(str::trim) {
        None | Some("here") | Some("") => here(store, session_id),
        Some("none") => None,
        Some(t) => Some(t.to_owned()),
    }
}

#[async_trait]
impl Tool for ScheduleCreate {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "schedule_create",
            "Schedule a prompt to run on a cron schedule (5 fields: minute hour day-of-month month day-of-week, in the user's local time; e.g. \"0 8 * * 1-5\" is weekdays at 08:00). Each run happens in its own session and its final answer is delivered to `deliver_to`: \"here\" (default: this chat, if it's a messaging chat), \"none\", or \"telegram:<chat_id>\". A run with nothing to report answers NO_REPLY and sends nothing. Results finishing in quiet hours wait until they end, unless `urgent`. Scheduled runs only happen while `tern daemon` is running.",
            {
                let mut props = limit_properties();
                props.insert("cron".into(), json!({ "type": "string" }));
                props.insert("prompt".into(), json!({ "type": "string", "description": "what to do each time, written as a standalone instruction" }));
                props.insert("deliver_to".into(), json!({ "type": "string" }));
                json!({ "type": "object", "properties": props, "required": ["cron", "prompt"] })
            },
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        POLICY
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: CreateArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let deliver_to = resolve_target(&self.store, &ctx.session_id, a.deliver_to.as_deref());
        let row = match create_schedule(
            &self.store,
            &a.cron,
            &a.prompt,
            deliver_to.clone(),
            &a.limits,
        ) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(e),
        };
        ToolResult::ok(format!(
            "created schedule {} ({}); next run {}; results go to {}{}",
            row.id,
            row.cron,
            fmt_time(row.next_run_at),
            deliver_to
                .as_deref()
                .unwrap_or("nowhere (check with `tern sessions`)"),
            describe_limits(&row)
        ))
    }
}

pub struct ScheduleUpdate {
    pub store: Store,
}

#[derive(Deserialize)]
struct UpdateArgs {
    id: String,
    cron: Option<String>,
    prompt: Option<String>,
    enabled: Option<bool>,
    deliver_to: Option<String>,
    #[serde(default)]
    resume: bool,
    #[serde(flatten)]
    limits: ScheduleLimits,
}

#[async_trait]
impl Tool for ScheduleUpdate {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "schedule_update",
            "Change a schedule by id: its cron, prompt, `deliver_to`, `enabled`, quiet hours, urgency or caps. Only the fields you pass change; pass null for a cap to return it to the default. `resume: true` lifts a pause caused by a daily cap.",
            {
                let mut props = limit_properties();
                props.insert("id".into(), json!({ "type": "string" }));
                props.insert("cron".into(), json!({ "type": "string" }));
                props.insert("prompt".into(), json!({ "type": "string" }));
                props.insert("enabled".into(), json!({ "type": "boolean" }));
                props.insert("deliver_to".into(), json!({ "type": "string" }));
                props.insert("resume".into(), json!({ "type": "boolean" }));
                json!({ "type": "object", "properties": props, "required": ["id"] })
            },
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        POLICY
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: UpdateArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let ch = ScheduleChange {
            cron: a.cron,
            prompt: a.prompt,
            enabled: a.enabled,
            deliver_to: a
                .deliver_to
                .as_deref()
                .map(|t| resolve_target(&self.store, &ctx.session_id, Some(t))),
            limits: a.limits,
            resume: a.resume,
        };
        match update_schedule(&self.store, &a.id, &ch) {
            Ok(Some(s)) => ToolResult::ok(format!(
                "updated {} [{}] {}; next run {} → {}{}",
                s.id,
                s.cron,
                if s.enabled { "enabled" } else { "disabled" },
                fmt_time(s.next_run_at),
                s.deliver_to.as_deref().unwrap_or("none"),
                describe_limits(&s)
            )),
            Ok(None) => ToolResult::error(format!("no schedule {:?}", a.id)),
            Err(e) => ToolResult::error(e),
        }
    }
}

pub struct ScheduleList {
    pub store: Store,
}

#[async_trait]
impl Tool for ScheduleList {
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "schedule_list",
            "List scheduled prompts.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    async fn call(&self, _: &ToolCtx, _: Value) -> ToolResult {
        match self.store.schedules() {
            Err(e) => ToolResult::error(e.to_string()),
            Ok(list) if list.is_empty() => ToolResult::ok("no schedules"),
            Ok(list) => ToolResult::ok(
                list.iter()
                    .map(|s| {
                        format!(
                            "{} [{}] {} → {}{}: {}",
                            s.id,
                            s.cron,
                            if s.enabled {
                                format!("next {}", fmt_time(s.next_run_at))
                            } else {
                                "disabled".into()
                            },
                            s.deliver_to.as_deref().unwrap_or("none"),
                            describe_limits(s),
                            s.prompt
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }
}

pub struct ScheduleDelete {
    pub store: Store,
}

#[async_trait]
impl Tool for ScheduleDelete {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "schedule_delete",
            "Delete a schedule by id.",
            json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        POLICY
    }

    async fn call(&self, _: &ToolCtx, raw: Value) -> ToolResult {
        let id = raw.get("id").and_then(Value::as_str).unwrap_or_default();
        match self.store.delete_schedule(id) {
            Ok(true) => ToolResult::ok(format!("deleted {id}")),
            Ok(false) => ToolResult::error(format!("no schedule {id:?}")),
            Err(e) => ToolResult::error(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn create_list_delete_and_deliver_here() {
        let store = Store::open_in_memory().unwrap();
        store
            .create_session_with_ref("chat", "telegram", "555", None)
            .unwrap();
        let ctx = ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: "r".into(),
            session_id: "chat".into(),
            cancel: CancellationToken::new(),
        };
        let create = ScheduleCreate {
            store: store.clone(),
        };
        let bad = create
            .call(&ctx, json!({"cron": "0 8 * *", "prompt": "x"}))
            .await;
        assert!(bad.is_error && bad.content.contains("5 fields"));
        let r = create
            .call(&ctx, json!({"cron": "0 8 * * 1-5", "prompt": "Summarize my unread GitHub notifications"}))
            .await;
        assert!(
            !r.is_error && r.content.contains("telegram:555"),
            "{}",
            r.content
        );
        let s = &store.schedules().unwrap()[0];
        assert_eq!(s.deliver_to.as_deref(), Some("telegram:555"));
        assert_eq!(
            store.session(&s.session_id).unwrap().unwrap().source,
            "cron"
        );
        assert!(s.next_run_at > tern_store::now_ms());

        let listed = ScheduleList {
            store: store.clone(),
        }
        .call(&ctx, json!({}))
        .await
        .content;
        assert!(
            listed.contains("0 8 * * 1-5") && listed.contains("GitHub"),
            "{listed}"
        );
        let update = ScheduleUpdate {
            store: store.clone(),
        };
        let r = update
            .call(
                &ctx,
                json!({"id": s.id, "quiet_hours": "23:00-06:00", "urgent": true, "runs_per_day": 3, "usd_per_day": 1, "tokens_per_run": 5000}),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            r.content
                .contains("quiet hours 23:00-06:00, urgent, 3 runs/day, $1/day, 5000 tokens/run"),
            "{}",
            r.content
        );
        let r = update
            .call(
                &ctx,
                json!({"id": s.id, "runs_per_day": null, "quiet_hours": "default"}),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        let row = store.schedule(&s.id).unwrap().unwrap();
        assert_eq!((row.runs_per_day, row.quiet_hours), (None, None));
        assert_eq!(row.usd_per_day, Some(1.0));
        for bad in [
            json!({"id": s.id, "quiet_hours": "late"}),
            json!({"id": s.id, "runs_per_day": 0}),
        ] {
            assert!(update.call(&ctx, bad).await.is_error);
        }
        assert!(update.call(&ctx, json!({"id": "nope"})).await.is_error);

        store
            .pause_schedule(&s.id, tern_store::now_ms() + 60_000, "runs_per_day")
            .unwrap();
        let listed = ScheduleList {
            store: store.clone(),
        }
        .call(&ctx, json!({}))
        .await
        .content;
        assert!(listed.contains("paused: budget (runs_per_day)"), "{listed}");
        assert!(
            !update
                .call(&ctx, json!({"id": s.id, "resume": true}))
                .await
                .is_error
        );
        assert_eq!(store.schedule(&s.id).unwrap().unwrap().paused_until, None);

        let del = ScheduleDelete {
            store: store.clone(),
        };
        assert!(!del.call(&ctx, json!({"id": s.id})).await.is_error);
        assert!(del.call(&ctx, json!({"id": s.id})).await.is_error);
    }
}
