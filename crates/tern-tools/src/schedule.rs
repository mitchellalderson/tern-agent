//! `schedule_create`, `schedule_update`, `schedule_list`, `schedule_delete`
//! (spec §12.5). The daemon runs due schedules.
//!
//! A schedule is a routine (tooling spec §3, T1b): a prompt with a trigger,
//! `cron` or `once` (`at`). The tools keep their `schedule_*` names, so
//! tool scopes and grants from before routines still apply.

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
    // Telegram's and gateway connectors' chats; `<chat>|<agent>` is an
    // agent's stream in the same chat.
    match (s.source.as_str(), s.external_ref) {
        (tern_core::subagent::SESSION_SOURCE, _) | (_, None) => None,
        (source, Some(chat)) => {
            let chat = chat.split('|').next().unwrap_or(&chat);
            Some(format!("{source}:{chat}"))
        }
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
    /// Luau run before each cron or once slot; false skips the run (T1b).
    /// `null` or empty removes it.
    #[serde(default, deserialize_with = "some")]
    pub gate: Option<Option<String>>,
    /// The active connector whose hosts, secrets and tools the gate gets.
    #[serde(default, deserialize_with = "some")]
    pub gate_connector: Option<Option<String>>,
    /// `silent`, `digest`, `notify`, `interrupt`, or `default`/`null`.
    #[serde(default, deserialize_with = "some")]
    pub initiative: Option<Option<String>>,
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
        let blank = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        if let Some(i) = &self.initiative {
            row.initiative = match i.as_deref().map(str::trim) {
                None | Some("default") | Some("") => None,
                Some(i) => Some(
                    tern_core::initiative::Initiative::parse(i)
                        .ok_or_else(|| {
                            format!("initiative {i:?} must be silent, digest, notify, interrupt or default")
                        })?
                        .as_str()
                        .to_owned(),
                ),
            };
        }
        if let Some(g) = &self.gate {
            row.gate = blank(g);
        }
        if let Some(c) = &self.gate_connector {
            row.gate_connector = blank(c);
        }
        if row.gate.is_some() && row.trigger() == "watch" {
            return Err(
                "a gate is for cron and once routines; a watch already decides when to run".into(),
            );
        }
        Ok(())
    }

    /// Check a gate given here (with its connector) compiles. A gate left
    /// as it is was checked when it was set.
    pub async fn check(
        &self,
        row: Option<&ScheduleRow>,
        checks: Option<&dyn RoutineChecks>,
    ) -> Result<(), String> {
        if self.gate.is_none() && self.gate_connector.is_none() {
            return Ok(());
        }
        let pick = |given: &Option<Option<String>>, old: Option<&String>| match given {
            Some(v) => v.clone().filter(|v| !v.trim().is_empty()),
            None => old.cloned(),
        };
        let gate = pick(&self.gate, row.and_then(|r| r.gate.as_ref()));
        let connector = pick(
            &self.gate_connector,
            row.and_then(|r| r.gate_connector.as_ref()),
        );
        match gate {
            None => Ok(()),
            Some(code) => {
                let checks = checks.ok_or("gates aren't available here")?;
                checks.gate(connector.as_deref(), &code).await
            }
        }
    }
}

/// What starts a routine (T1b). At most one of `cron`, `at` and `watch`;
/// on create, one of them is required.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct TriggerArgs {
    /// 5-field cron, local time.
    pub cron: Option<String>,
    /// Once, at a local `YYYY-MM-DD HH:MM` (or RFC 3339), then off.
    pub at: Option<String>,
    /// `<connector>.<watch>`: poll it, and run once per new event.
    pub watch: Option<String>,
    /// How often to poll a watch: `90s`, `5m`, `1h`, `1d` (at least 1m).
    /// [`check_trigger`] fills in the watch's own default.
    pub every: Option<String>,
    /// Run for what the first poll finds, instead of only recording it.
    pub backfill: Option<bool>,
    /// Keep events the watch marks as caused by tern itself.
    pub include_self: Option<bool>,
}

/// The shortest watch interval (tooling spec §3.2).
pub const MIN_EVERY_SECS: i64 = 60;

/// Checks a routine's watch (and gate) against the active connectors.
/// tern-tools runs no Luau; the runtime provides this.
#[async_trait]
pub trait RoutineChecks: Send + Sync {
    /// Watch `watch` of active connector `connector`.
    async fn watch(&self, connector: &str, watch: &str) -> Result<WatchSummary, String>;
    /// Whether gate `code` compiles, with `connector`'s capabilities.
    async fn gate(&self, connector: Option<&str>, code: &str) -> Result<(), String>;
}

/// What the user should know about a watch before it runs: how often, and
/// where it reaches (tooling spec §3.2, "activating a watch shows its
/// interval and hosts").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchSummary {
    pub default_every: Option<String>,
    pub hosts: Vec<String>,
}

/// Check a trigger's watch (if any) and fill in its interval. `checks` is
/// `None` where no connectors exist, and then a watch is refused.
pub async fn check_trigger(
    t: &mut TriggerArgs,
    checks: Option<&dyn RoutineChecks>,
) -> Result<Option<WatchSummary>, String> {
    let Some(w) = &t.watch else {
        return Ok(None);
    };
    let (connector, watch) = split_watch(w)?;
    let checks = checks.ok_or("watches aren't available here")?;
    let summary = checks.watch(connector, watch).await?;
    if t.every.is_none() {
        t.every = Some(summary.default_every.clone().unwrap_or_else(|| "5m".into()));
    }
    parse_every(t.every.as_deref().unwrap_or_default())?;
    Ok(Some(summary))
}

/// `github.review_requests` → (`github`, `review_requests`).
pub fn split_watch(w: &str) -> Result<(&str, &str), String> {
    w.trim()
        .split_once('.')
        .filter(|(c, w)| !c.is_empty() && !w.is_empty())
        .ok_or_else(|| format!("watch {w:?} must be \"<connector>.<watch>\""))
}

/// `90s`, `5m`, `1h`, `1d` or plain seconds, at least a minute.
pub fn parse_every(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let (n, unit) = match s.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => (&s[..i], s[i..].trim()),
        None => (s, "s"),
    };
    let mult = match unit {
        "s" | "sec" | "secs" => 1,
        "m" | "min" | "mins" => 60,
        "h" | "hr" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86_400,
        _ => 0,
    };
    let secs = n.parse::<i64>().ok().filter(|_| mult > 0).map(|n| n * mult);
    match secs {
        Some(secs) if secs >= MIN_EVERY_SECS => Ok(secs),
        Some(_) => Err(format!("`every` {s:?} is too often: at least 1m")),
        None => Err(format!(
            "`every` {s:?} must be like \"5m\", \"1h\" or \"1d\""
        )),
    }
}

/// `300` → `5m`.
pub fn fmt_every(secs: i64) -> String {
    match secs {
        s if s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

impl TriggerArgs {
    pub fn cron(cron: &str) -> Self {
        Self {
            cron: Some(cron.to_owned()),
            ..Self::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.cron.is_none() && self.at.is_none() && self.watch.is_none()
    }

    /// Check the trigger and set it on `row`, with its next run. A watch
    /// must have been through [`check_trigger`].
    fn apply(&self, row: &mut ScheduleRow) -> Result<(), String> {
        if let Some(b) = self.backfill {
            row.backfill = b;
        }
        if let Some(i) = self.include_self {
            row.include_self = i;
        }
        let given = [self.cron.is_some(), self.at.is_some(), self.watch.is_some()];
        if given.iter().filter(|g| **g).count() > 1 {
            return Err("give one of `cron`, `at` or `watch`".into());
        }
        if self.every.is_some() && self.watch.is_none() {
            if row.trigger() != "watch" {
                return Err("`every` is for watches".into());
            }
            row.every_secs = Some(parse_every(self.every.as_deref().unwrap_or_default())?);
            return Ok(());
        }
        if let Some(c) = &self.cron {
            let cron: Cron = c
                .parse()
                .map_err(|e: tern_core::cron::CronError| e.to_string())?;
            let next = cron
                .next_after(chrono::Local::now())
                .ok_or("that schedule never fires")?;
            row.trigger = "cron".into();
            row.cron = cron.to_string();
            row.watch = None;
            row.every_secs = None;
            row.next_run_at = next.timestamp_millis();
        } else if let Some(at) = &self.at {
            let at = parse_at(at)?;
            if at <= chrono::Local::now() {
                return Err(format!(
                    "{} has already passed",
                    at.format("%Y-%m-%d %H:%M")
                ));
            }
            row.trigger = "once".into();
            row.cron = String::new();
            row.watch = None;
            row.every_secs = None;
            row.next_run_at = at.timestamp_millis();
        } else if let Some(w) = &self.watch {
            let (connector, watch) = split_watch(w)?;
            row.trigger = "watch".into();
            row.cron = String::new();
            row.watch = Some(format!("{connector}.{watch}"));
            row.every_secs = Some(parse_every(self.every.as_deref().unwrap_or("5m"))?);
            // The first poll (a baseline) happens on the next tick.
            row.next_run_at = tern_store::now_ms();
        } else if row.trigger.is_empty() {
            return Err("give `cron` (repeating), `at` (once) or `watch` (on events)".into());
        }
        Ok(())
    }
}

/// A local `YYYY-MM-DD HH:MM` (or with a `T`, or seconds), or RFC 3339.
pub fn parse_at(s: &str) -> Result<chrono::DateTime<chrono::Local>, String> {
    let s = s.trim();
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&chrono::Local));
    }
    for fmt in [
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return t
                .and_local_timezone(chrono::Local)
                .earliest()
                .ok_or_else(|| format!("{s} doesn't exist in local time"));
        }
    }
    Err(format!(
        "`at` {s:?} must be a local \"YYYY-MM-DD HH:MM\" (or RFC 3339)"
    ))
}

/// The trigger in words, for listings: `0 8 * * 1-5` or `once at …`.
pub fn describe_trigger(s: &ScheduleRow) -> String {
    match s.trigger() {
        "once" => {
            let at = if s.next_run_at == i64::MAX {
                s.last_run_at.unwrap_or(s.next_run_at)
            } else {
                s.next_run_at
            };
            format!("once at {}", fmt_time(at))
        }
        "watch" => format!(
            "watch {} every {}",
            s.watch.as_deref().unwrap_or("?"),
            fmt_every(s.every_secs.unwrap_or(300))
        ),
        _ => s.cron.clone(),
    }
}

/// The JSON schema properties for [`TriggerArgs`], for tool specs.
fn trigger_properties() -> serde_json::Map<String, Value> {
    let props = json!({
        "cron": { "type": "string", "description": "repeat on a 5-field cron (minute hour day-of-month month day-of-week, local time), e.g. \"0 8 * * 1-5\" is weekdays at 08:00" },
        "at": { "type": "string", "description": "run once at this local time, \"YYYY-MM-DD HH:MM\" (reminders: \"tomorrow at 9\"), instead of `cron`" },
        "watch": { "type": "string", "description": "\"<connector>.<watch>\": run on events from an active connector's watch instead of a clock. The watch is polled (no model call when it finds nothing); each new event starts a run with the event attached as untrusted input. The first poll only records what is already there." },
        "every": { "type": "string", "description": "how often to poll the watch, e.g. \"5m\" (at least 1m; default: the watch's own)" },
        "backfill": { "type": "boolean", "description": "also run for what the first poll finds. Rarely wanted." }
    });
    match props {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

/// The JSON schema properties for [`ScheduleLimits`], for tool specs.
fn limit_properties() -> serde_json::Map<String, Value> {
    let props = json!({
        "quiet_hours": { "type": "string", "description": "local \"HH:MM-HH:MM\" when results wait until the window ends, \"none\", or \"default\" (the user's setting, normally 22:00-07:30). Only set it when the user asks." },
        "urgent": { "type": "boolean", "description": "deliver even during quiet hours. Only when the user says it's urgent." },
        "initiative": { "type": ["string", "null"], "description": "how the result reaches the chat (it is always posted to the channel): \"interrupt\" sends it at once, for reminders and anything the user asked to receive at a time; \"notify\" sends it only when a quick check finds it worth interrupting for, else in the digest (monitoring); \"digest\" (the default) batches it into the next digest (08:30 and 17:30); \"silent\" never sends." },
        "runs_per_day": { "type": ["integer", "null"], "description": "cap; when reached the schedule pauses until tomorrow" },
        "usd_per_day": { "type": ["number", "null"], "description": "spend cap per day, in USD" },
        "tokens_per_run": { "type": ["integer", "null"], "description": "a run that uses more tokens is stopped" },
        "gate": { "type": ["string", "null"], "description": "Luau checked before each cron/once run, at no token cost; a false result skips that run. An expression (`os.date(\"*t\").wday ~= 1`) or statements with `return`. Gets `ctx` and `routine`; with `gate_connector`, also `connector` (its tools, e.g. `#connector.tools.unread.run(ctx, {}) > 0`) and that connector's hosts (GET only)." },
        "gate_connector": { "type": ["string", "null"], "description": "an active connector the gate may use" }
    });
    match props {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

/// The agent a tool call acts for: its session's (agent profiles §4).
fn caller(store: &Store, ctx: &ToolCtx) -> String {
    store
        .session_agent(&ctx.session_id)
        .unwrap_or_else(|_| tern_store::DEFAULT_AGENT.into())
}

/// Schedule `id` if `agent` owns it: one agent can't see or change
/// another's jobs.
fn owned(store: &Store, id: &str, agent: &str) -> Option<ScheduleRow> {
    store
        .schedule(id)
        .ok()
        .flatten()
        .filter(|s| s.agent() == agent)
}

/// Validate and save a cron schedule owned by `agent` (see
/// [`create_routine`]).
pub fn create_schedule(
    store: &Store,
    cron: &str,
    prompt: &str,
    deliver_to: Option<String>,
    limits: &ScheduleLimits,
    agent: &str,
) -> Result<ScheduleRow, String> {
    create_routine(
        store,
        &TriggerArgs::cron(cron),
        prompt,
        deliver_to,
        limits,
        agent,
    )
}

/// Validate and save a routine owned by `agent`, with its own `cron`
/// session. `deliver_to` is already resolved (`None` = keep results in the
/// session). An agent's results are posted to its DM; the default agent's
/// to Home.
pub fn create_routine(
    store: &Store,
    trigger: &TriggerArgs,
    prompt: &str,
    deliver_to: Option<String>,
    limits: &ScheduleLimits,
    agent: &str,
) -> Result<ScheduleRow, String> {
    if prompt.trim().is_empty() {
        return Err("prompt is empty".into());
    }
    if let Some(t) = &deliver_to {
        check_deliver_to(t)?;
    }
    let mut row = ScheduleRow {
        id: format!("sch_{}", &tern_core::agent::new_id()[20..]),
        prompt: prompt.trim().to_owned(),
        session_id: tern_core::agent::new_id(),
        deliver_to,
        enabled: true,
        agent_id: (agent != tern_store::DEFAULT_AGENT).then(|| agent.to_owned()),
        ..ScheduleRow::default()
    };
    trigger.apply(&mut row)?;
    if row.agent_id.is_some() {
        let dm = store
            .agent_channel(agent)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("agent {agent} has no DM"))?;
        row.channel_id = Some(dm.id);
    }
    limits.apply(&mut row)?;
    store
        .create_session(&row.session_id, "cron", None)
        .and_then(|_| store.set_session_agent(&row.session_id, agent))
        .and_then(|_| store.create_schedule(&row))
        .map_err(|e| format!("saving schedule: {e}"))?;
    if row.channel_id.is_none() {
        row.channel_id = Some(tern_store::HOME.into());
    }
    Ok(row)
}

/// Changes to a schedule; `None` leaves a field as it is.
#[derive(Debug, Default, Clone)]
pub struct ScheduleChange {
    /// A new trigger replaces the old one (`cron` alone stays a cron).
    pub trigger: TriggerArgs,
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
    let t = &ch.trigger;
    let retrigger = !t.is_empty();
    if retrigger || t.every.is_some() || t.backfill.is_some() || t.include_self.is_some() {
        t.apply(&mut s)?;
    }
    if retrigger {
        // A new time for a reminder that already ran turns it back on.
        if s.trigger() == "once" && ch.enabled.is_none() {
            s.enabled = true;
        }
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
    if enabling && !retrigger {
        // Re-enabling starts from now rather than catching up missed slots.
        match s.trigger() {
            "once" if s.next_run_at <= tern_store::now_ms() || s.next_run_at == i64::MAX => {
                return Err("its time has passed; give a new `at` to run it again".into());
            }
            "once" => {}
            _ => {
                s.next_run_at = s
                    .cron
                    .parse::<Cron>()
                    .map_err(|e| e.to_string())?
                    .next_after(chrono::Local::now())
                    .map_or(i64::MAX, |t| t.timestamp_millis());
            }
        }
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
    if let Some(i) = &s.initiative {
        parts.push(i.clone());
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
    if s.gate.is_some() {
        parts.push(match &s.gate_connector {
            Some(c) => format!("gated (with {c})"),
            None => "gated".into(),
        });
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// A concrete delivery target: `telegram:<chat_id>`, or `<gateway>:<chat>`
/// for a gateway connector (`ntfy:<topic>`, `slack:<channel id>`).
pub fn check_deliver_to(t: &str) -> Result<(), String> {
    let ok = match t.split_once(':') {
        Some(("telegram", chat)) => chat.parse::<i64>().is_ok(),
        Some((gateway, chat)) => {
            let mut g = gateway.chars();
            gateway.len() <= 32
                && g.next().is_some_and(|c| c.is_ascii_lowercase())
                && g.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && !chat.is_empty()
                && !chat.chars().any(char::is_whitespace)
        }
        None => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "deliver_to {t:?} must be \"here\", \"none\", \"telegram:<chat_id>\" or \"<gateway>:<chat>\" (e.g. \"ntfy:<topic>\")"
        ))
    }
}

pub struct ScheduleCreate {
    pub store: Store,
    /// Checks watches against the active connectors; `None` refuses them.
    pub checks: Option<std::sync::Arc<dyn RoutineChecks>>,
}

#[derive(Deserialize)]
struct CreateArgs {
    #[serde(flatten)]
    trigger: TriggerArgs,
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
            "Schedule a prompt (a routine) to run repeatedly on a `cron`, or once `at` a time (reminders). Each run happens in its own session and its final answer is delivered to `deliver_to`: \"here\" (default: this chat, if it's a messaging chat), \"none\", \"telegram:<chat_id>\", or \"<gateway>:<chat>\" for a gateway connector such as ntfy or slack. A run with nothing to report answers NO_REPLY and sends nothing. Results finishing in quiet hours wait until they end, unless `urgent`. Scheduled runs only happen while `tern daemon` is running.",
            {
                let mut props = limit_properties();
                props.extend(trigger_properties());
                props.insert("prompt".into(), json!({ "type": "string", "description": "what to do each time, written as a standalone instruction" }));
                props.insert("deliver_to".into(), json!({ "type": "string" }));
                json!({ "type": "object", "properties": props, "required": ["prompt"] })
            },
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        POLICY
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let mut a: CreateArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let watch = match check_trigger(&mut a.trigger, self.checks.as_deref()).await {
            Ok(w) => w,
            Err(e) => return ToolResult::error(e),
        };
        if let Err(e) = a.limits.check(None, self.checks.as_deref()).await {
            return ToolResult::error(e);
        }
        let deliver_to = resolve_target(&self.store, &ctx.session_id, a.deliver_to.as_deref());
        let row = match create_routine(
            &self.store,
            &a.trigger,
            &a.prompt,
            deliver_to.clone(),
            &a.limits,
            &caller(&self.store, ctx),
        ) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(e),
        };
        let when = match &watch {
            Some(w) => format!(
                "it reaches {}; the first poll records what is there now without running",
                if w.hosts.is_empty() {
                    "no hosts".to_owned()
                } else {
                    w.hosts.join(", ")
                }
            ),
            None => format!("next run {}", fmt_time(row.next_run_at)),
        };
        ToolResult::ok(format!(
            "created schedule {} ({}); {when}; results go to {}{}",
            row.id,
            describe_trigger(&row),
            deliver_to
                .as_deref()
                .unwrap_or("nowhere (check with `tern sessions`)"),
            describe_limits(&row)
        ))
    }
}

pub struct ScheduleUpdate {
    pub store: Store,
    pub checks: Option<std::sync::Arc<dyn RoutineChecks>>,
}

#[derive(Deserialize)]
struct UpdateArgs {
    id: String,
    #[serde(flatten)]
    trigger: TriggerArgs,
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
            "Change a schedule by id: its trigger (`cron` or `at`), prompt, `deliver_to`, `enabled`, quiet hours, urgency or caps. Only the fields you pass change; pass null for a cap to return it to the default. `resume: true` lifts a pause caused by a daily cap.",
            {
                let mut props = limit_properties();
                props.extend(trigger_properties());
                props.insert("id".into(), json!({ "type": "string" }));
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
        let mut a: UpdateArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        if let Err(e) = check_trigger(&mut a.trigger, self.checks.as_deref()).await {
            return ToolResult::error(e);
        }
        let old = self.store.schedule(&a.id).ok().flatten();
        if let Err(e) = a.limits.check(old.as_ref(), self.checks.as_deref()).await {
            return ToolResult::error(e);
        }
        if owned(&self.store, &a.id, &caller(&self.store, ctx)).is_none() {
            return ToolResult::error(format!("no schedule {:?} of yours", a.id));
        }
        let ch = ScheduleChange {
            trigger: a.trigger,
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
                describe_trigger(&s),
                if s.enabled { "enabled" } else { "disabled" },
                fmt_time(s.next_run_at),
                s.deliver_to.as_deref().unwrap_or("none"),
                describe_limits(&s)
            )),
            Ok(None) => ToolResult::error(format!("no schedule {:?} of yours", a.id)),
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
            "List your scheduled prompts.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    async fn call(&self, ctx: &ToolCtx, _: Value) -> ToolResult {
        let me = caller(&self.store, ctx);
        let mine = self.store.schedules().map(|l| {
            l.into_iter()
                .filter(|s| s.agent() == me)
                .collect::<Vec<_>>()
        });
        match mine {
            Err(e) => ToolResult::error(e.to_string()),
            Ok(list) if list.is_empty() => ToolResult::ok("no schedules"),
            Ok(list) => ToolResult::ok(
                list.iter()
                    .map(|s| {
                        format!(
                            "{} [{}] {} → {}{}: {}",
                            s.id,
                            describe_trigger(s),
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

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let id = raw.get("id").and_then(Value::as_str).unwrap_or_default();
        if owned(&self.store, id, &caller(&self.store, ctx)).is_none() {
            return ToolResult::error(format!("no schedule {id:?} of yours"));
        }
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
            checks: None,
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
            checks: None,
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

    #[tokio::test]
    async fn a_once_routine_runs_at_a_time_and_can_be_set_again() {
        let store = Store::open_in_memory().unwrap();
        store.create_session("s", "api", None).unwrap();
        let ctx = ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let create = ScheduleCreate {
            store: store.clone(),
            checks: None,
        };
        let tomorrow = (chrono::Local::now() + chrono::Duration::days(1))
            .format("%Y-%m-%d 09:00")
            .to_string();
        for bad in [
            json!({"prompt": "x"}),
            json!({"prompt": "x", "at": "2001-01-01 09:00"}),
            json!({"prompt": "x", "at": "tomorrow"}),
            json!({"prompt": "x", "at": tomorrow, "cron": "0 9 * * *"}),
        ] {
            let r = create.call(&ctx, bad.clone()).await;
            assert!(r.is_error, "{bad}: {}", r.content);
        }
        let r = create
            .call(&ctx, json!({"prompt": "Call the dentist", "at": tomorrow}))
            .await;
        assert!(
            !r.is_error && r.content.contains("once at"),
            "{}",
            r.content
        );
        let s = store.schedules().unwrap().remove(0);
        assert_eq!((s.trigger(), s.cron.as_str()), ("once", ""));
        assert_eq!(
            s.next_run_at,
            parse_at(&tomorrow).unwrap().timestamp_millis()
        );
        // After it ran, it is off; a new `at` turns it back on.
        store.finish_once(&s.id, tern_store::now_ms()).unwrap();
        let update = ScheduleUpdate {
            store: store.clone(),
            checks: None,
        };
        let r = update
            .call(&ctx, json!({"id": s.id, "enabled": true}))
            .await;
        assert!(r.is_error && r.content.contains("passed"), "{}", r.content);
        let r = update.call(&ctx, json!({"id": s.id, "at": tomorrow})).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(store.schedule(&s.id).unwrap().unwrap().enabled);
        // And a cron turns it into a repeating routine.
        update
            .call(&ctx, json!({"id": s.id, "cron": "0 9 * * *"}))
            .await;
        let s = store.schedule(&s.id).unwrap().unwrap();
        assert_eq!((s.trigger(), s.cron.as_str()), ("cron", "0 9 * * *"));
    }

    #[tokio::test]
    async fn an_agent_sees_and_changes_only_its_own_schedules() {
        let store = Store::open_in_memory().unwrap();
        let (agent, dm) = store
            .create_agent(&tern_store::AgentRow {
                id: "tern:inbox".into(),
                slug: "inbox".into(),
                name: "Inbox".into(),
                memory: "private".into(),
                ..Default::default()
            })
            .unwrap();
        store
            .create_thread(&tern_store::NewThread {
                id: "inbox-dm",
                source: "api",
                channel_id: &dm,
                ..Default::default()
            })
            .unwrap();
        store.create_session("home", "api", None).unwrap();
        let ctx = |session: &str| ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: "r".into(),
            session_id: session.into(),
            cancel: CancellationToken::new(),
        };
        let (inbox, home) = (ctx("inbox-dm"), ctx("home"));
        let create = ScheduleCreate {
            store: store.clone(),
            checks: None,
        };
        let r = create
            .call(
                &inbox,
                json!({"cron": "0 8 * * 1-5", "prompt": "sweep my inbox"}),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        let s = store.schedules().unwrap().remove(0);
        // Owned by Inbox, posting to its DM, its session acting as Inbox.
        assert_eq!(s.agent(), agent.id);
        assert_eq!(s.channel_id.as_deref(), Some(dm.as_str()));
        assert_eq!(store.session_agent(&s.session_id).unwrap(), agent.id);
        // tern doesn't see it and can't change or delete it.
        let list = ScheduleList {
            store: store.clone(),
        };
        assert_eq!(list.call(&home, json!({})).await.content, "no schedules");
        assert!(list.call(&inbox, json!({})).await.content.contains("sweep"));
        let update = ScheduleUpdate {
            store: store.clone(),
            checks: None,
        };
        let r = update
            .call(&home, json!({"id": s.id, "enabled": false}))
            .await;
        assert!(
            r.is_error && r.content.contains("of yours"),
            "{}",
            r.content
        );
        let del = ScheduleDelete {
            store: store.clone(),
        };
        assert!(del.call(&home, json!({"id": s.id})).await.is_error);
        assert!(store.schedule(&s.id).unwrap().unwrap().enabled);
        assert!(
            !update
                .call(&inbox, json!({"id": s.id, "enabled": false}))
                .await
                .is_error
        );
        assert!(!del.call(&inbox, json!({"id": s.id})).await.is_error);
        // tern's own still go to Home.
        create
            .call(&home, json!({"cron": "0 9 * * *", "prompt": "brief"}))
            .await;
        let s = store.schedules().unwrap().remove(0);
        assert_eq!((s.agent(), s.channel_id.as_deref()), ("tern", Some("home")));
    }

    struct FakeChecks;

    #[async_trait]
    impl RoutineChecks for FakeChecks {
        async fn watch(&self, connector: &str, watch: &str) -> Result<WatchSummary, String> {
            match (connector, watch) {
                ("github", "reviews") => Ok(WatchSummary {
                    default_every: Some("10m".into()),
                    hosts: vec!["api.github.com".into()],
                }),
                _ => Err(format!("connector {connector} has no watch {watch:?}")),
            }
        }

        async fn gate(&self, connector: Option<&str>, code: &str) -> Result<(), String> {
            match (connector, code) {
                (Some(c), _) if c != "github" => Err(format!("connector {c} is not active")),
                (_, "true and") => Err("syntax error".into()),
                _ => Ok(()),
            }
        }
    }

    #[tokio::test]
    async fn gates_are_checked_and_only_for_clock_routines() {
        let store = Store::open_in_memory().unwrap();
        store.create_session("s", "api", None).unwrap();
        let ctx = ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let checks: Option<std::sync::Arc<dyn RoutineChecks>> =
            Some(std::sync::Arc::new(FakeChecks));
        let create = ScheduleCreate {
            store: store.clone(),
            checks: checks.clone(),
        };
        for bad in [
            json!({"prompt": "x", "cron": "0 8 * * *", "gate": "true and"}),
            json!({"prompt": "x", "cron": "0 8 * * *", "gate": "true", "gate_connector": "nope"}),
            json!({"prompt": "x", "watch": "github.reviews", "gate": "true"}),
        ] {
            let r = create.call(&ctx, bad.clone()).await;
            assert!(r.is_error, "{bad}: {}", r.content);
        }
        let r = create
            .call(&ctx, json!({"prompt": "brief", "cron": "0 8 * * *", "gate": " #connector.tools.search.run(ctx, {}) > 0 ", "gate_connector": "github"}))
            .await;
        assert!(
            !r.is_error && r.content.contains("gated (with github)"),
            "{}",
            r.content
        );
        let s = store.schedules().unwrap().remove(0);
        assert_eq!(
            s.gate.as_deref(),
            Some("#connector.tools.search.run(ctx, {}) > 0")
        );
        let update = ScheduleUpdate {
            store: store.clone(),
            checks,
        };
        assert!(
            update
                .call(&ctx, json!({"id": s.id, "gate_connector": "nope"}))
                .await
                .is_error
        );
        assert!(
            update
                .call(&ctx, json!({"id": s.id, "initiative": "loud"}))
                .await
                .is_error
        );
        let r = update
            .call(&ctx, json!({"id": s.id, "initiative": "Notify"}))
            .await;
        assert!(!r.is_error && r.content.contains("notify"), "{}", r.content);
        let r = update
            .call(
                &ctx,
                json!({"id": s.id, "gate": null, "initiative": "default"}),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        let row = store.schedule(&s.id).unwrap().unwrap();
        assert_eq!((row.gate, row.initiative), (None, None));
    }

    #[tokio::test]
    async fn a_watch_routine_names_its_interval_and_hosts() {
        let store = Store::open_in_memory().unwrap();
        store.create_session("s", "api", None).unwrap();
        let ctx = ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let checks: Option<std::sync::Arc<dyn RoutineChecks>> =
            Some(std::sync::Arc::new(FakeChecks));
        let create = ScheduleCreate {
            store: store.clone(),
            checks: checks.clone(),
        };
        let no_lua = ScheduleCreate {
            store: store.clone(),
            checks: None,
        };
        let pr = json!({"prompt": "Summarize the PR", "watch": "github.reviews"});
        assert!(no_lua.call(&ctx, pr.clone()).await.is_error);
        for bad in [
            json!({"prompt": "x", "watch": "github.nope"}),
            json!({"prompt": "x", "watch": "github"}),
            json!({"prompt": "x", "watch": "github.reviews", "every": "30s"}),
            json!({"prompt": "x", "watch": "github.reviews", "cron": "* * * * *"}),
            json!({"prompt": "x", "cron": "* * * * *", "every": "5m"}),
        ] {
            let r = create.call(&ctx, bad.clone()).await;
            assert!(r.is_error, "{bad}: {}", r.content);
        }
        let r = create.call(&ctx, pr).await;
        assert!(
            !r.is_error
                && r.content.contains("watch github.reviews every 10m")
                && r.content.contains("reaches api.github.com"),
            "{}",
            r.content
        );
        let s = store.schedules().unwrap().remove(0);
        assert_eq!(
            (s.trigger(), s.every_secs, s.backfill),
            ("watch", Some(600), false)
        );
        assert!(
            s.next_run_at <= tern_store::now_ms(),
            "polled on the next tick"
        );
        let update = ScheduleUpdate {
            store: store.clone(),
            checks,
        };
        let r = update
            .call(&ctx, json!({"id": s.id, "every": "1h", "backfill": true}))
            .await;
        assert!(!r.is_error, "{}", r.content);
        let s = store.schedule(&s.id).unwrap().unwrap();
        assert_eq!((s.every_secs, s.backfill), (Some(3600), true));
        assert_eq!(parse_every("2d"), Ok(172_800));
        assert_eq!(fmt_every(5400), "90m");
    }
}
