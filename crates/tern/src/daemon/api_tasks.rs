//! The Tasks view over the API (tooling spec §8.2): everything running,
//! queued, scheduled, waiting on you, or failed recently, across agents and
//! channels. A query over what exists, not a new model: the same pending
//! approvals, outbox, secret requests, runs and routines the other
//! endpoints serve, gathered in one list.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/tasks` | `{items, counts}`; each item has a `section` (`needs_you`, `running`, `queued`, `scheduled`, `failed`), a `kind`, and where it lives (`session_id`, `channel_id`, `routine_id`) |
//!
//! "Needs you" is the count Home's badge shows. Goals (§10) join it when
//! they exist.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::api::{ApiResult, Shared, internal};
use super::manager::Manager;

pub fn routes() -> Router<Shared> {
    Router::new().route("/v1/tasks", get(list))
}

/// The sections, in the order they are shown.
pub const SECTIONS: [&str; 5] = ["needs_you", "running", "queued", "scheduled", "failed"];

/// How far back "failed recently" looks.
const FAILED_WINDOW_MS: i64 = 24 * 3_600_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub section: String,
    /// `approval`, `outbox`, `secret_request`, `sign_in`, `skill_offer` (needs you); `run`
    /// (running, queued, failed); `events`, `delivery` (queued);
    /// `routine` (scheduled); `routine_paused` (failed).
    pub kind: String,
    /// The approval, outbox item, run or routine id.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// When it started, is due, or failed (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Sub-agents nest under their parent run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// The conversation it lives in (the top of a sub-agent's tree).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tainted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tasks {
    pub items: Vec<Task>,
    /// Items per section.
    pub counts: BTreeMap<String, u32>,
}

async fn list(State(m): State<Shared>) -> ApiResult<Tasks> {
    collect(&m).map(Json).map_err(internal)
}

/// A routine's name: its prompt's first line.
fn routine_title(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or("").trim();
    match line.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}

/// Gather every task. Each source is read as the endpoint that owns it
/// reads it.
pub fn collect(m: &Manager) -> anyhow::Result<Tasks> {
    let store = &m.rt.store;
    let now = tern_store::now_ms();
    let routines = store.schedules()?;
    let routine_of = |session: &str| routines.iter().find(|r| r.session_id == session);
    // Where a session lives: its channel and agent.
    let place = |session: &str| -> (Option<String>, Option<String>) {
        match store.session(session).ok().flatten() {
            Some(s) => (s.channel_id.clone(), Some(s.agent().to_owned())),
            None => (None, None),
        }
    };
    let run_task = |section: &str, r: &tern_store::RunRow| -> Task {
        let routine = routine_of(&r.session_id);
        let session = store.session(&r.session_id).ok().flatten();
        let title = match (&r.label, routine, &session) {
            (Some(l), _, _) if r.parent_run_id.is_some() => format!("sub-agent {l}"),
            (_, Some(s), _) => routine_title(&s.prompt),
            (_, _, Some(s)) => s.title.clone().unwrap_or_else(|| "Conversation".into()),
            _ => "Run".into(),
        };
        let (channel_id, agent_id) = place(&r.session_id);
        Task {
            section: section.into(),
            kind: "run".into(),
            id: r.id.clone(),
            title,
            detail: r.error.clone(),
            at: Some(if section == "failed" {
                r.finished_at.unwrap_or(r.updated_at)
            } else {
                r.started_at
            }),
            run_id: Some(r.id.clone()),
            parent_run_id: r.parent_run_id.clone(),
            session_id: Some(r.session_id.clone()),
            channel_id,
            agent_id,
            routine_id: routine.map(|s| s.id.clone()),
            tainted: r.taint.is_some(),
        }
    };
    let mut items = Vec::new();

    // Needs you.
    for a in m.routes.api.pending() {
        let session = a.root_session_id.clone().unwrap_or(a.session_id.clone());
        items.push(Task {
            section: "needs_you".into(),
            kind: "approval".into(),
            id: a.id.clone(),
            title: format!("{} · {}", a.effect, a.summary),
            detail: a.taint.as_deref().map(tern_core::approval::taint_note),
            run_id: Some(a.run_id.clone()),
            parent_run_id: a.parent_run_id.clone(),
            routine_id: routine_of(&session).map(|s| s.id.clone()),
            session_id: Some(session),
            channel_id: a.channel_id.clone(),
            agent_id: a.agent_id.clone(),
            tainted: a.taint.is_some(),
            ..Task::default()
        });
    }
    for o in store.outbox(Some("pending"), 200)? {
        let item = m.outbox_view(o);
        let session = store
            .root_run(&item.run_id)
            .ok()
            .and_then(|r| store.run(&r).ok().flatten())
            .map_or(item.session_id.clone(), |r| r.session_id);
        items.push(Task {
            section: "needs_you".into(),
            kind: "outbox".into(),
            id: item.id.to_string(),
            title: item.headline(),
            detail: Some(format!(
                "outbox #{}, expires in {}h",
                item.id,
                (item.expires_at - now).max(0) / 3_600_000
            )),
            at: Some(item.created_at),
            run_id: Some(item.run_id.clone()),
            routine_id: routine_of(&session).map(|s| s.id.clone()),
            session_id: Some(session),
            channel_id: item.channel_id.clone(),
            agent_id: item.agent_id.clone(),
            tainted: item.taint.is_some(),
            ..Task::default()
        });
    }
    for o in m.offers(Some("pending"))? {
        let (channel_id, agent_id) = place(&o.session_id);
        items.push(Task {
            section: "needs_you".into(),
            kind: "skill_offer".into(),
            id: o.id.to_string(),
            title: format!(
                "Save as a skill? {}",
                o.title.as_deref().unwrap_or("a recent conversation")
            ),
            detail: Some(format!(
                "{} tool calls; `tern skills save {}` writes a draft to review",
                o.tool_calls, o.id
            )),
            at: Some(o.created_at),
            run_id: Some(o.run_id.clone()),
            session_id: Some(o.session_id.clone()),
            channel_id,
            agent_id,
            ..Task::default()
        });
    }
    for r in m.routes.secrets.pending() {
        let session = r.root_session_id.clone().unwrap_or(r.session_id.clone());
        let names: Vec<&str> = r.secrets.iter().map(|s| s.name.as_str()).collect();
        items.push(Task {
            section: "needs_you".into(),
            kind: "secret_request".into(),
            id: r.id.clone(),
            title: format!("secrets: {}", names.join(", ")),
            detail: Some(r.reason.clone()),
            run_id: Some(r.run_id.clone()),
            session_id: Some(session),
            channel_id: r.channel_id.clone(),
            agent_id: r.agent_id.clone(),
            ..Task::default()
        });
    }
    for p in m.routes.auth.pending() {
        let session = p
            .root_session_id
            .clone()
            .unwrap_or(p.prompt.session_id.clone());
        items.push(Task {
            section: "needs_you".into(),
            kind: "sign_in".into(),
            id: p.prompt.id.clone(),
            title: "sign in".into(),
            run_id: Some(p.prompt.run_id.clone()),
            session_id: Some(session),
            channel_id: p.channel_id.clone(),
            agent_id: p.agent_id.clone(),
            ..Task::default()
        });
    }

    // Running: what the manager is driving, sub-agents after their parent.
    let mut running: Vec<tern_store::RunRow> = m
        .active_runs()
        .into_iter()
        .filter_map(|(_, run)| store.run(&run).ok().flatten())
        .collect();
    running.sort_by_key(|r| (r.depth, r.started_at));
    for r in &running {
        items.push(run_task("running", r));
    }

    // Queued: runs waiting for their session, watch events waiting for a
    // run, results held for quiet hours or a digest.
    for r in store.runs_with_status("queued")? {
        items.push(run_task("queued", &r));
    }
    for (routine, n) in store.waiting_routine_events()? {
        let r = routines.iter().find(|s| s.id == routine);
        let (channel_id, agent_id) = r.map(|r| place(&r.session_id)).unwrap_or_default();
        items.push(Task {
            section: "queued".into(),
            kind: "events".into(),
            id: routine.clone(),
            title: r.map_or(routine.clone(), |r| routine_title(&r.prompt)),
            detail: Some(format!(
                "{n} event{} waiting",
                if n == 1 { "" } else { "s" }
            )),
            session_id: r.map(|r| r.session_id.clone()),
            channel_id,
            agent_id,
            routine_id: Some(routine),
            ..Task::default()
        });
    }
    for h in store.due_held_deliveries(i64::MAX)? {
        let run = h
            .run_id
            .as_deref()
            .and_then(|r| store.run(r).ok().flatten());
        let session = run.as_ref().map(|r| r.session_id.clone());
        let (channel_id, agent_id) = session.as_deref().map(place).unwrap_or_default();
        items.push(Task {
            section: "queued".into(),
            kind: "delivery".into(),
            id: h.id.to_string(),
            title: h.label.clone().unwrap_or_else(|| "A result".into()),
            detail: Some(match h.kind.as_str() {
                "digest" => format!("in the next digest to {}", h.target),
                _ => format!("held for quiet hours, to {}", h.target),
            }),
            at: Some(h.release_at),
            run_id: h.run_id.clone(),
            routine_id: session
                .as_deref()
                .and_then(routine_of)
                .map(|s| s.id.clone()),
            session_id: session,
            channel_id,
            agent_id,
            ..Task::default()
        });
    }

    // Scheduled, and routines in error.
    for r in &routines {
        let (channel_id, agent_id) = place(&r.session_id);
        let base = Task {
            id: r.id.clone(),
            title: routine_title(&r.prompt),
            session_id: Some(r.session_id.clone()),
            channel_id: r.channel_id.clone().or(channel_id),
            agent_id: r.agent_id.clone().or(agent_id),
            routine_id: Some(r.id.clone()),
            ..Task::default()
        };
        if let Some(until) = r.paused_until.filter(|&u| u > now) {
            items.push(Task {
                section: "failed".into(),
                kind: "routine_paused".into(),
                detail: Some(r.paused_reason.clone().unwrap_or_else(|| "paused".into())),
                at: Some(until),
                ..base
            });
            continue;
        }
        if !r.enabled {
            continue;
        }
        let (at, detail) = match r.trigger.as_str() {
            "watch" => (
                None,
                Some(format!("checks every {}s", r.every_secs.unwrap_or(300))),
            ),
            "webhook" => (None, Some("on each delivery".into())),
            _ => (Some(r.next_run_at), Some(r.cron.clone())),
        };
        items.push(Task {
            section: "scheduled".into(),
            kind: "routine".into(),
            at,
            detail,
            ..base
        });
    }
    for r in store.failed_runs_since(now - FAILED_WINDOW_MS, 50)? {
        items.push(run_task("failed", &r));
    }

    let mut counts: BTreeMap<String, u32> = SECTIONS.iter().map(|s| (s.to_string(), 0)).collect();
    for t in &items {
        *counts.entry(t.section.clone()).or_default() += 1;
    }
    Ok(Tasks { items, counts })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routines_are_named_by_their_first_line() {
        assert_eq!(routine_title("Morning brief\nthen more"), "Morning brief");
        let long = "x".repeat(100);
        assert_eq!(routine_title(&long).chars().count(), 81);
    }
}
