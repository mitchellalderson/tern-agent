//! The scheduler (spec §12.5): runs due schedules, enforces their daily
//! caps, and sends deliveries held during quiet hours (T1a).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Local};
use tern_core::config::SchedulesConfig;
use tern_core::cron::Cron;
use tern_core::quiet;
use tern_store::{HeldDelivery, NewEvent, ScheduleRow};
use tokio_util::sync::CancellationToken;

use super::manager::Manager;

/// Env var overriding the scheduler tick (milliseconds), for tests.
pub const TICK_ENV: &str = "TERN_SCHEDULER_TICK_MS";
const DEFAULT_TICK: Duration = Duration::from_secs(30);

/// Run due schedules every tick. A schedule missed while the daemon was
/// down runs once, then continues from now: no backfill.
pub async fn run(manager: Arc<Manager>, shutdown: CancellationToken) {
    let tick = std::env::var(TICK_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TICK);
    let mut interval = tokio::time::interval(tick);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            () = shutdown.cancelled() => return,
        }
        if let Err(e) = release_held(&manager).await {
            tracing::error!(error = %e, "sending held deliveries failed");
        }
        if let Err(e) = run_due(&manager).await {
            tracing::error!(error = %e, "scheduler tick failed");
        }
    }
}

async fn run_due(manager: &Arc<Manager>) -> anyhow::Result<()> {
    let store = &manager.rt.store;
    let now = Local::now();
    let defaults = manager.rt.live().schedules;
    for s in store.due_schedules(now.timestamp_millis())? {
        let next = match next_slot(&s, now) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(schedule = %s.id, error = %e, "invalid trigger; parking routine");
                store.mark_schedule_ran(&s.id, now.timestamp_millis(), i64::MAX)?;
                continue;
            }
        };
        if still_paused(manager, &s, now)? {
            store.advance_schedule(&s.id, next)?;
            continue;
        }
        if s.trigger() == "watch" {
            // Polling costs no tokens; caps apply to the runs events start.
            store.advance_schedule(&s.id, next)?;
            poll(manager, &s, &defaults).await;
            continue;
        }
        if !gate_passes(manager, &s, now).await {
            if s.trigger() == "once" {
                store.finish_once(&s.id, now.timestamp_millis())?;
            } else {
                store.advance_schedule(&s.id, next)?;
            }
            continue;
        }
        if pause_for_cap(manager, &s, &defaults, now).await? {
            store.advance_schedule(&s.id, next)?;
            continue;
        }
        // Advance first, so a crash mid-run doesn't start it twice.
        if s.trigger() == "once" {
            store.finish_once(&s.id, now.timestamp_millis())?;
        } else {
            store.mark_schedule_ran(&s.id, now.timestamp_millis(), next)?;
        }
        tracing::info!(schedule = %s.id, "running scheduled prompt");
        if let Err(e) = manager.start_scheduled(&s).await {
            tracing::error!(schedule = %s.id, error = %e, "scheduled run failed to start");
        }
    }
    start_waiting(manager, &defaults, now).await
}

/// Whether a budget pause still holds `s`; lifts one that has ended.
fn still_paused(manager: &Manager, s: &ScheduleRow, now: DateTime<Local>) -> anyhow::Result<bool> {
    if let Some(until) = s.paused_until {
        if until > now.timestamp_millis() {
            return Ok(true);
        }
        manager.rt.store.unpause_schedule(&s.id, false)?;
    }
    Ok(false)
}

/// Run `s`'s gate, if it has one (tooling spec §3.3): false skips this
/// slot without a model call. A gate that fails lets the run go ahead, so a
/// broken gate costs a run rather than a missed brief; `last_check` says so.
async fn gate_passes(manager: &Manager, s: &ScheduleRow, now: DateTime<Local>) -> bool {
    let Some(code) = &s.gate else {
        return true;
    };
    let routine = serde_json::json!({
        "id": s.id,
        "last_run_at": s.last_run_at,
        "now": now.timestamp_millis(),
    });
    let result = match manager.rt.lua_host() {
        Ok(host) => {
            tern_lua::watch::gate(
                &host,
                &manager.rt.home.connectors_dir(),
                s.gate_connector.as_deref(),
                code,
                &routine,
            )
            .await
        }
        Err(e) => Err(e.to_string()),
    };
    let (pass, check) = match result {
        Ok(true) => (true, "gate: passed".to_owned()),
        Ok(false) => (false, "gate: skipped".to_owned()),
        Err(e) => {
            tracing::warn!(schedule = %s.id, error = %e, "gate failed; running anyway");
            (true, format!("gate failed, ran anyway: {e}"))
        }
    };
    if !pass {
        tracing::info!(schedule = %s.id, "gate said no; skipping this run");
    }
    if let Err(e) = manager.rt.store.set_routine_check(&s.id, &check) {
        tracing::warn!(schedule = %s.id, error = %e, "recording a gate failed");
    }
    pass
}

/// Pause `s` until tomorrow if a daily cap stops it, notifying once.
async fn pause_for_cap(
    manager: &Manager,
    s: &ScheduleRow,
    defaults: &SchedulesConfig,
    now: DateTime<Local>,
) -> anyhow::Result<bool> {
    let Some(why) = cap_reached(manager, s, defaults, now)? else {
        return Ok(false);
    };
    let until = quiet::next_day(now).timestamp_millis();
    // Only the first slot after the cap notifies.
    if manager.rt.store.pause_schedule(&s.id, until, why.key)? {
        tracing::info!(schedule = %s.id, cap = why.key, "schedule paused until tomorrow");
        let text = format!(
            "⏸ Paused “{}” until tomorrow: {}. It resumes on its own then; to resume it now, use the app or ask me.",
            title(s),
            why.detail
        );
        manager.deliver_for_schedule(s, None, &text).await;
    }
    Ok(true)
}

/// Poll a watch routine (tooling spec §3.2) and record what it found. No
/// model is called here: new events wait for [`start_waiting`].
async fn poll(manager: &Manager, s: &ScheduleRow, defaults: &SchedulesConfig) {
    let store = &manager.rt.store;
    let watch = s.watch.as_deref().unwrap_or_default();
    let check = match poll_once(manager, s, defaults).await {
        Ok(check) => check,
        Err(e) => {
            tracing::warn!(schedule = %s.id, watch, error = %e, "watch poll failed");
            format!("watch failed: {e}")
        }
    };
    tracing::debug!(schedule = %s.id, watch, check, "polled");
    if let Err(e) = store.set_routine_check(&s.id, &check) {
        tracing::warn!(schedule = %s.id, error = %e, "recording a poll failed");
    }
}

async fn poll_once(
    manager: &Manager,
    s: &ScheduleRow,
    defaults: &SchedulesConfig,
) -> anyhow::Result<String> {
    let store = &manager.rt.store;
    let (connector, watch) =
        tern_tools::schedule::split_watch(s.watch.as_deref().unwrap_or_default())
            .map_err(anyhow::Error::msg)?;
    let cursor = store.watch_cursor(&s.id)?;
    // The first poll is a baseline: what is already there doesn't run.
    let baseline = cursor.is_none() && !s.backfill;
    let cursor = cursor
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or(serde_json::Value::Null);
    let host = manager.rt.lua_host()?;
    let found = tern_lua::watch::poll(
        &host,
        &manager.rt.home.connectors_dir(),
        connector,
        watch,
        &cursor,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    let events: Vec<NewEvent> = found
        .events
        .into_iter()
        .map(|e| NewEvent {
            state: if baseline {
                "baseline"
            } else if e.from_self && !s.include_self {
                "self"
            } else {
                "pending"
            },
            key: e.key,
            payload: e.payload,
        })
        .collect();
    let dropped = events.iter().filter(|e| e.state == "self").count();
    let rec = store.record_poll(
        &s.id,
        &events,
        &found.cursor.to_string(),
        "polled",
        defaults.max_events_per_poll.max(1),
    )?;
    if rec.to_run > 0 {
        tracing::info!(schedule = %s.id, events = rec.to_run, "watch found new events");
    }
    Ok(match (baseline, rec.new, dropped) {
        (true, n, _) => format!("first poll: {n} existing events recorded, not run"),
        (false, 0, _) => "no new events".into(),
        (false, n, 0) => format!("{n} new events"),
        (false, n, d) => format!("{n} new events ({d} from tern itself, skipped)"),
    })
}

/// Start runs for waiting events now rather than at the next tick: a
/// webhook delivery calls this once its event is recorded.
pub async fn start_waiting_now(manager: &Arc<Manager>) -> anyhow::Result<()> {
    let defaults = manager.rt.live().schedules;
    start_waiting(manager, &defaults, Local::now()).await
}

/// Start a run for routines with events waiting: one at a time per
/// routine, since its runs share a session. Caps apply as for cron runs.
async fn start_waiting(
    manager: &Arc<Manager>,
    defaults: &SchedulesConfig,
    now: DateTime<Local>,
) -> anyhow::Result<()> {
    // The tick and webhook deliveries both start events; one at a time, so
    // an event can't be handed to two runs.
    static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = STARTING.lock().await;
    let store = &manager.rt.store;
    let busy: HashSet<String> = manager
        .active_runs()
        .into_iter()
        .map(|(session, _)| session)
        .collect();
    for id in store.routines_with_waiting_events()? {
        let Some(s) = store.schedule(&id)? else {
            continue;
        };
        if !s.enabled
            || busy.contains(&s.session_id)
            || still_paused(manager, &s, now)?
            || pause_for_cap(manager, &s, defaults, now).await?
        {
            continue;
        }
        let events = store.next_routine_events(&id)?;
        if events.is_empty() {
            continue;
        }
        match manager.start_watch_run(&s, &events).await {
            Ok(started) => {
                // A crash before this line starts these events again: twice
                // at worst, never lost.
                let keys: Vec<String> = events.into_iter().map(|e| e.key).collect();
                store.mark_events_started(&id, &keys, &started.run_id)?;
                tracing::info!(schedule = %id, events = keys.len(), run = %started.run_id, trigger = s.trigger(), "event run started");
            }
            Err(e) => {
                tracing::error!(schedule = %id, error = %e, "watch run failed to start");
            }
        }
    }
    Ok(())
}

/// When a due routine is due next: its cron's next slot after `now`, a
/// watch's next poll, never for a `once` routine.
fn next_slot(s: &ScheduleRow, now: DateTime<Local>) -> Result<i64, String> {
    match s.trigger() {
        "once" | "webhook" => Ok(i64::MAX),
        "watch" => Ok(now.timestamp_millis()
            + 1000
                * s.every_secs
                    .unwrap_or(300)
                    .max(tern_tools::schedule::MIN_EVERY_SECS)),
        "cron" => Ok(s
            .cron
            .parse::<Cron>()
            .map_err(|e| e.to_string())?
            .next_after(now)
            .map_or(i64::MAX, |t| t.timestamp_millis())),
        other => Err(format!("unknown trigger {other:?}")),
    }
}

/// The first line of a schedule's prompt, for notices.
pub fn title(s: &ScheduleRow) -> String {
    let line = s.prompt.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(60) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}

struct CapReached {
    /// The setting that was reached: `runs_per_day` or `usd_per_day`.
    key: &'static str,
    detail: String,
}

/// Which daily cap, if any, stops `s` from running again today. Counts
/// from the start of the local day, or from when the user last resumed it.
fn cap_reached(
    manager: &Manager,
    s: &ScheduleRow,
    defaults: &SchedulesConfig,
    now: DateTime<Local>,
) -> anyhow::Result<Option<CapReached>> {
    let store = &manager.rt.store;
    let since = quiet::start_of_day(now)
        .timestamp_millis()
        .max(s.budget_since.unwrap_or(0));
    if let Some(cap) = s.runs_per_day.or(defaults.runs_per_day) {
        let runs = store.scheduled_runs_since(&s.session_id, since)?;
        if runs >= cap {
            return Ok(Some(CapReached {
                key: "runs_per_day",
                detail: format!("it has run {runs} times today, its limit (runs_per_day = {cap})"),
            }));
        }
    }
    if let Some(cap) = s.usd_per_day.or(defaults.usd_per_day)
        && let Some(spent) = spend_since(manager, s, since)?
        && spent >= cap
    {
        return Ok(Some(CapReached {
            key: "usd_per_day",
            detail: format!("it has spent ${spent:.2} today, its limit (usd_per_day = ${cap})"),
        }));
    }
    Ok(None)
}

/// What `s` has spent today against its daily USD cap (its own, else the
/// `[schedules]` default): `(spent, cap)`. `None` without a cap or a
/// known price, so a client shows nothing rather than a guess.
pub fn spend_today(manager: &Manager, s: &ScheduleRow) -> Option<(f64, f64)> {
    let cap = s
        .usd_per_day
        .or(manager.rt.live().schedules.usd_per_day)?;
    let since = quiet::start_of_day(Local::now())
        .timestamp_millis()
        .max(s.budget_since.unwrap_or(0));
    let spent = spend_since(manager, s, since).ok()??;
    Some((spent, cap))
}

/// Models whose missing price has been logged, so it's said once.
fn noted() -> &'static Mutex<HashSet<String>> {
    static NOTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    NOTED.get_or_init(Default::default)
}

/// USD `s` has spent since `since`, sub-agents included. `None` when a
/// model it used, or will use, has no known price: the USD cap is then
/// ignored, and that is logged once per schedule and model.
fn spend_since(manager: &Manager, s: &ScheduleRow, since: i64) -> anyhow::Result<Option<f64>> {
    let rt = &manager.rt;
    let usage = rt.store.usage_by_model_since(&s.session_id, since)?;
    let next_model = rt
        .store
        .session(&s.session_id)?
        .and_then(|sess| sess.model)
        .or_else(|| rt.live().default_model.map(|c| c.primary().to_owned()));
    let mut spent = 0.0;
    let models = usage
        .iter()
        .map(|(m, u)| (m.clone(), *u))
        .chain(next_model.map(|m| (Some(m), (0, 0, 0, 0))));
    for (model, used) in models {
        let model = model.unwrap_or_default();
        match rt.price_of(&model) {
            Some(p) => spent += p.cost(used),
            None => {
                let key = format!("{}|{model}", s.id);
                if noted().lock().is_ok_and(|mut n| n.insert(key)) {
                    tracing::warn!(
                        schedule = %s.id,
                        model,
                        "usd_per_day is ignored: no price is known for this model (set [providers.<name>.prices] in config.toml)"
                    );
                }
                return Ok(None);
            }
        }
    }
    Ok(Some(spent))
}

/// Send held deliveries whose quiet hours are over: one message per target.
async fn release_held(manager: &Manager) -> anyhow::Result<()> {
    let store = &manager.rt.store;
    let due = store.due_held_deliveries(tern_store::now_ms())?;
    let mut targets: Vec<&str> = due.iter().map(|h| h.target.as_str()).collect();
    targets.dedup();
    let mut seen = HashSet::new();
    for target in targets.into_iter().filter(|t| seen.insert(*t)) {
        let items: Vec<&HeldDelivery> = due.iter().filter(|h| h.target == target).collect();
        match manager.send_to(target, None, &combine(&items)).await {
            Ok(()) => {
                let ids: Vec<i64> = items.iter().map(|h| h.id).collect();
                store.delete_held_deliveries(&ids)?;
                tracing::info!(
                    target,
                    count = ids.len(),
                    "sent deliveries held during quiet hours"
                );
            }
            Err(e) => tracing::warn!(target, error = %e, "held delivery failed; will retry"),
        }
    }
    Ok(())
}

/// One message for everything held for a target: a digest if any of it
/// was held for one, else what quiet hours held.
fn combine(items: &[&HeldDelivery]) -> String {
    let digest = items.iter().any(|h| h.kind == "digest");
    if let [one] = items {
        return match (&one.label, digest) {
            (Some(label), true) => format!("Digest · ▸ {label}\n{}", one.text.trim_end()),
            _ => one.text.clone(),
        };
    }
    let mut out = if digest {
        format!("Digest · {} updates:", items.len())
    } else {
        format!("{} updates from quiet hours:", items.len())
    };
    for h in items {
        out.push_str("\n\n");
        if let Some(label) = &h.label {
            out.push_str(&format!("▸ {label}\n"));
        }
        out.push_str(h.text.trim_end());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(id: i64, label: Option<&str>, text: &str) -> HeldDelivery {
        HeldDelivery {
            id,
            target: "telegram:1".into(),
            run_id: None,
            label: label.map(Into::into),
            text: text.into(),
            release_at: 0,
            kind: "quiet".into(),
        }
    }

    #[test]
    fn held_deliveries_combine_into_one_message() {
        let (a, b) = (held(1, Some("Inbox"), "2 new\n"), held(2, None, "done"));
        assert_eq!(combine(&[&a]), "2 new\n");
        assert_eq!(
            combine(&[&a, &b]),
            "2 updates from quiet hours:\n\n▸ Inbox\n2 new\n\ndone"
        );
        let mut d = held(3, Some("Build"), "green");
        d.kind = "digest".into();
        assert_eq!(combine(&[&d]), "Digest · ▸ Build\ngreen");
        assert_eq!(
            combine(&[&a, &d]),
            "Digest · 2 updates:\n\n▸ Inbox\n2 new\n\n▸ Build\ngreen"
        );
    }

    #[test]
    fn titles_are_the_first_line_shortened() {
        let s = ScheduleRow {
            prompt: format!("{}\nmore", "x".repeat(70)),
            ..ScheduleRow::default()
        };
        assert_eq!(title(&s), format!("{}…", "x".repeat(60)));
    }
}
