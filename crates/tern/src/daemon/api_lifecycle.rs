//! Status and stopping (docs/personal-agent-tooling-spec.md §11.7, T8a).
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/status` | one cheap summary: version, uptime, counts, gateway health |
//! | POST | `/v1/shutdown[?dry_run=1]` | stop gracefully (exit 0); runs in progress stay `running` and resume on the next start. With `dry_run`, only list them |
//!
//! Shutdown is owner-only over TCP (`auth::OWNER_ONLY`); the Unix socket is
//! the owner.

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::api::{ApiResult, Shared, internal};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/shutdown", post(shutdown))
}

#[derive(Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub pid: u32,
    pub uptime_secs: u64,
    /// `launchd` or `systemd` when the service started this daemon.
    pub managed_by: Option<String>,
    /// Top-level runs in progress (sub-agents count with their parent).
    pub running_runs: usize,
    /// Approvals and secret requests waiting for an API client.
    pub pending_approvals: usize,
    /// Enabled schedules that will fire again.
    pub active_schedules: usize,
    pub gateways: Vec<GatewayStatus>,
}

#[derive(Serialize, Deserialize)]
pub struct GatewayStatus {
    pub name: String,
    /// Its polling task is still running.
    pub ok: bool,
}

async fn status(State(m): State<Shared>) -> ApiResult<Status> {
    let schedules = m.rt.store.schedules().map_err(internal)?;
    let gateways = m
        .routes
        .lifecycle
        .gateway_tasks
        .lock()
        .map(|t| {
            t.iter()
                .map(|(name, task)| GatewayStatus {
                    name: name.clone(),
                    ok: !task.is_finished(),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(Status {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        pid: std::process::id(),
        uptime_secs: m.routes.lifecycle.started.elapsed().as_secs(),
        managed_by: std::env::var(crate::service::SERVICE_ENV)
            .ok()
            .filter(|v| !v.is_empty()),
        running_runs: in_progress(&m).len(),
        pending_approvals: m.routes.api.pending().len() + m.routes.secrets.pending().len(),
        active_schedules: schedules
            .iter()
            .filter(|s| s.enabled && s.next_run_at != i64::MAX)
            .count(),
        gateways,
    }))
}

/// A run that stopping would pause, named the way the user would know it.
#[derive(Serialize, Deserialize)]
pub struct PausedRun {
    pub run_id: String,
    pub session_id: String,
    /// The session's title, else the start of the message that began the
    /// run.
    pub title: String,
}

#[derive(Serialize, Deserialize)]
pub struct ShutdownReply {
    /// False for a dry run.
    pub stopping: bool,
    pub runs: Vec<PausedRun>,
}

#[derive(Deserialize)]
struct ShutdownQuery {
    dry_run: Option<String>,
}

async fn shutdown(
    State(m): State<Shared>,
    Query(q): Query<ShutdownQuery>,
) -> ApiResult<ShutdownReply> {
    let dry_run = q
        .dry_run
        .as_deref()
        .is_some_and(|v| !matches!(v, "" | "0" | "false"));
    let runs = in_progress(&m);
    if !dry_run {
        tracing::info!(runs = runs.len(), "shutdown requested over the API");
        if std::env::var_os(super::TEST_IGNORE_SHUTDOWN_ENV).is_none() {
            m.routes.lifecycle.shutdown.cancel();
        }
    }
    Ok(Json(ShutdownReply {
        stopping: !dry_run,
        runs,
    }))
}

fn in_progress(m: &Shared) -> Vec<PausedRun> {
    let store = &m.rt.store;
    let mut runs: Vec<_> = m
        .active_runs()
        .into_iter()
        .filter_map(|(session_id, run_id)| {
            let run = store.run(&run_id).ok().flatten()?;
            if run.parent_run_id.is_some() {
                return None;
            }
            let title = store
                .session(&session_id)
                .ok()
                .flatten()
                .and_then(|s| s.title)
                .filter(|t| !t.trim().is_empty())
                .or_else(|| {
                    let id = run.input_message_id?;
                    store
                        .session_messages(&session_id)
                        .ok()?
                        .into_iter()
                        .find(|msg| msg.id == id)
                        .map(|msg| msg.text)
                })
                .map_or_else(|| "a task".to_owned(), |t| short(&t));
            Some((
                run.started_at,
                PausedRun {
                    run_id,
                    session_id,
                    title,
                },
            ))
        })
        .collect();
    runs.sort_by_key(|(started, _)| *started);
    runs.into_iter().map(|(_, r)| r).collect()
}

/// One line, at most 60 characters.
fn short(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() <= 60 {
        line.to_owned()
    } else {
        format!("{}…", line.chars().take(59).collect::<String>().trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::short;

    #[test]
    fn titles_are_one_short_line() {
        assert_eq!(
            short("\n  Summarize the Q3 report\nmore"),
            "Summarize the Q3 report"
        );
        let long = "x".repeat(80);
        assert_eq!(short(&long).chars().count(), 60);
        assert!(short(&long).ends_with('…'));
    }
}
