//! Waits (docs/wait-until-design.md): the daemon polls each one's check
//! with no model call, and when its condition matches, or it times out,
//! starts one run in the conversation that made it and deletes the wait.
//! The poll runs on the scheduler's tick.

use std::sync::Arc;
use std::time::Duration;

use chrono::Local;
use tern_core::quiet;
use tern_store::WaitRow;
use tern_tools::wait::{Check, Ended, Until, clip, continuation_text, parse_output};

use super::manager::Manager;

/// Most of a check's answer that is read, kept or handed to the model.
const MAX_BODY: usize = 256 * 1024;
const MAX_VALUE: usize = 8 * 1024;
const URL_TIMEOUT: Duration = Duration::from_secs(20);

/// Check every wait that is due; end the ones that are done.
pub async fn poll(manager: &Arc<Manager>) -> anyhow::Result<()> {
    let store = &manager.rt.store;
    let now = tern_store::now_ms();
    for w in store.due_waits(now)? {
        if let Err(e) = step(manager, w).await {
            tracing::warn!(error = %e, "checking a wait failed");
        }
    }
    Ok(())
}

/// One due wait: a check, then an end if it is over.
async fn step(manager: &Arc<Manager>, w: WaitRow) -> anyhow::Result<()> {
    let store = &manager.rt.store;
    let check = Check::from_row(&w.check_kind, &w.check_spec).map_err(anyhow::Error::msg)?;
    let now = tern_store::now_ms();
    let mut value: Option<String> = w.last_value.clone();
    let mut error: Option<String> = None;
    let ended = match &check {
        Check::Time => (now >= w.expires_at).then_some(Ended::Time),
        _ => {
            let until: Until = w
                .until_spec
                .as_deref()
                .and_then(|u| serde_json::from_str(u).ok())
                .ok_or_else(|| anyhow::anyhow!("wait {} has no condition", w.id))?;
            let mut matched = false;
            match run_check(manager, &check).await {
                Ok(text) => {
                    let parsed = parse_output(&text);
                    // What the condition looked at, short, for the list.
                    let seen = until.pick(&parsed).map_or_else(
                        || "field not found".to_owned(),
                        |v| clip(&v.to_string(), 80),
                    );
                    matched = until.met(&parsed);
                    value = Some(clip(&text, MAX_VALUE));
                    store.record_wait_check(
                        &w.id,
                        now,
                        now + w.every_secs * 1000,
                        &format!("{} = {seen}", until.path.as_deref().unwrap_or("result")),
                        value.as_deref(),
                    )?;
                }
                Err(e) => {
                    let e = clip(&e, 300);
                    store.record_wait_check(
                        &w.id,
                        now,
                        now + w.every_secs * 1000,
                        &format!("check failed: {e}"),
                        None,
                    )?;
                    error = Some(e);
                }
            }
            if matched {
                Some(Ended::Matched {
                    value: value.clone().unwrap_or_default(),
                })
            } else if now >= w.expires_at {
                Some(Ended::TimedOut {
                    value: value.clone(),
                    error: error.clone(),
                })
            } else {
                None
            }
        }
    };
    match ended {
        Some(ended) => finish(manager, &w, check, ended).await,
        None => Ok(()),
    }
}

/// Run a wait's check: a connector's read tool or a public URL. The text
/// it answers with, or why it couldn't.
async fn run_check(manager: &Manager, check: &Check) -> Result<String, String> {
    match check {
        Check::Time => Ok(String::new()),
        Check::Connector {
            connector,
            tool,
            args,
        } => {
            let host = manager.rt.lua_host().map_err(|e| e.to_string())?;
            tern_lua::watch::call_read(
                &host,
                &manager.rt.home.connectors_dir(),
                connector,
                tool,
                args,
            )
            .await
        }
        Check::Url(url) => {
            let f = tern_core::net::get_public(url, MAX_BODY, false, URL_TIMEOUT).await?;
            if !f.status.is_success() {
                return Err(format!("HTTP {}", f.status.as_u16()));
            }
            if f.truncated {
                return Err("the answer is too large".into());
            }
            String::from_utf8(f.body).map_err(|_| "the answer isn't text".to_owned())
        }
    }
}

/// End a wait: continue its conversation, unless a daily limit stops it or
/// the conversation is busy (then it is tried again on the next tick).
async fn finish(
    manager: &Arc<Manager>,
    w: &WaitRow,
    check: Check,
    ended: Ended,
) -> anyhow::Result<()> {
    let store = &manager.rt.store;
    // A run is working in the conversation: it would be cut off. Wait.
    if manager.active_run_in(&w.session_id).is_some() {
        return Ok(());
    }
    // The user may have cancelled it while it was checked.
    if store.wait(&w.id)?.is_none() {
        return Ok(());
    }
    let defaults = manager.rt.live().schedules;
    let now = Local::now();
    if let Some(cap) = defaults.runs_per_day {
        let since = quiet::start_of_day(now).timestamp_millis();
        let used = store.waits_fired_since(&w.agent_id, since)?;
        if used >= cap {
            store.end_wait(&w.id, "wait.cancel", tern_store::ACTOR_TERN, "daily limit")?;
            let text = format!(
                "⏸ Dropped the wait “{}”: this agent has continued {used} times today, its limit (runs_per_day = {cap}).",
                w.note
            );
            tell(manager, w, &text).await;
            return Ok(());
        }
    }
    let why = match &ended {
        Ended::Time => "time",
        Ended::Matched { .. } => "matched",
        Ended::TimedOut { .. } => "timed out",
    };
    let outside = !matches!(check, Check::Time)
        && match &ended {
            Ended::Matched { .. } => true,
            Ended::TimedOut { value, error } => value.is_some() || error.is_some(),
            Ended::Time => false,
        };
    // What came from outside is untrusted: the run starts tainted, so its
    // writes ask. So does a wait made by a run that was already tainted.
    let taint = if outside {
        Some(format!("started by a wait on {}", check.label()))
    } else if w.tainted {
        Some("the run that made this wait was tainted".to_owned())
    } else {
        None
    };
    let text = continuation_text(w, &ended, tern_store::now_ms());
    let started = manager
        .start_tainted(
            Some(&w.session_id),
            None,
            &text,
            "wait",
            "wait",
            w.deliver_to.as_deref(),
            taint.as_deref(),
        )
        .await;
    match started {
        Ok(run) => {
            if let Some(cap) = defaults.tokens_per_run {
                store.set_run_budget_tokens(&run.run_id, cap)?;
            }
            // A crash before this line fires it again: twice at worst,
            // never lost.
            store.end_wait(&w.id, "wait.fire", tern_store::ACTOR_TERN, why)?;
            tracing::info!(wait = %w.id, session = %w.session_id, run = %run.run_id, why, "wait ended; conversation continued");
        }
        Err(e) => {
            tracing::error!(wait = %w.id, error = %e, "a wait could not continue its conversation");
            store.end_wait(
                &w.id,
                "wait.cancel",
                tern_store::ACTOR_TERN,
                "couldn't continue",
            )?;
            tell(
                manager,
                w,
                &format!("The wait “{}” ended but I couldn't continue: {e}", w.note),
            )
            .await;
        }
    }
    Ok(())
}

/// Tell the chat the wait was made in, when there is one.
async fn tell(manager: &Manager, w: &WaitRow, text: &str) {
    if let Some(target) = &w.deliver_to
        && let Err(e) = manager.send_to(target, None, text).await
    {
        tracing::warn!(target, error = %e, "telling a chat about a wait failed");
    }
    tracing::info!(wait = %w.id, "{text}");
}
