//! `tern daemon` (spec §12.3): one process that owns the runtime, the
//! scheduler, and the local API on `~/.tern/tern.sock`.

pub mod api;
pub mod api_agents;
mod api_bundles;
pub mod api_channels;
mod api_config;
mod api_connections;
mod api_documents;
mod api_ledger;
pub mod api_lifecycle;
mod api_local;
mod api_mcp;
pub mod api_mcp_serve;
mod api_outbox;
mod api_providers;
mod api_runs;
pub mod api_schedules;
mod api_secrets;
pub mod api_skills;
mod api_subagents;
pub mod api_tasks;
mod api_waits;
pub mod api_workspaces;
pub mod channels;
pub mod delegations;
pub mod health;
pub mod manager;
pub mod outbox;
pub mod scheduler;
pub mod suggestions;
pub mod uploads;
pub mod waiting;
pub mod waits;
pub mod wakes;

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use tern_core::TernHome;
use tern_core::agent::{RESUME_MAX_AGE, triage_running_runs};
use tokio_util::sync::CancellationToken;

use crate::runtime::Runtime;
use manager::{Manager, Router, Routes};

/// Test switch: answer `/v1/shutdown` but keep running, and ignore SIGTERM,
/// like a wedged daemon, so `tern stop`'s escalation can be tested.
pub const TEST_IGNORE_SHUTDOWN_ENV: &str = "TERN_TEST_IGNORE_SHUTDOWN";

/// Holds the PID file lock for the daemon's lifetime.
struct PidLock {
    _file: std::fs::File,
}

fn lock_pid(home: &TernHome) -> anyhow::Result<PidLock> {
    let path = home.pid_path();
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    if file.try_lock().is_err() {
        let pid = std::fs::read_to_string(&path).unwrap_or_default();
        bail!(
            "another tern daemon is already running (pid {}); only one may use {}",
            pid.trim(),
            home.db_path().display()
        );
    }
    file.set_len(0)?;
    write!(file, "{}", std::process::id())?;
    file.flush()?;
    Ok(PidLock { _file: file })
}

pub fn run(home: &TernHome) -> anyhow::Result<ExitCode> {
    home.ensure_layout()?;
    let _pid = match lock_pid(home) {
        Ok(lock) => lock,
        // Under launchd/systemd a failure would be restarted every few
        // seconds; another daemon running is fine, so exit cleanly.
        Err(e) if std::env::var_os(crate::service::SERVICE_ENV).is_some() => {
            eprintln!("{e:#}; leaving it running");
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => return Err(e),
    };
    let routes = Arc::new(Routes::default());
    let rt = Arc::new(Runtime::open(home, Arc::new(Router(routes.clone())))?);
    let _ = routes.store.set(rt.store.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(home, rt, routes))
}

async fn serve(home: &TernHome, rt: Arc<Runtime>, routes: Arc<Routes>) -> anyhow::Result<ExitCode> {
    let shutdown = CancellationToken::new();

    let manager = Manager::new(rt.clone(), routes.clone());
    let _ = routes.manager.set(Arc::downgrade(&manager));
    if let Some(h) = &rt.subagents {
        h.set_driver(Arc::new(manager::ManagerDriver(Arc::downgrade(&manager))));
    }

    // An Ollama provider is configured: have Ollama running before the first
    // run (reuse one that answers, else start it; loopback only).
    if rt.live().providers.values().any(|p| {
        p.effective_base_url()
            .is_some_and(tern_providers::ollama::is_ollama_url)
    }) {
        let rt = rt.clone();
        tokio::spawn(async move {
            if let Err(e) = rt.local.ensure_running().await {
                tracing::info!(error = %e, "Ollama isn't running and couldn't be started");
            }
        });
    }

    // Channel posts a crash skipped (docs/channels-design.md §2.3).
    manager.reconcile_posts();
    // Spec §5.3: resume recent runs a crash cut off; mark older ones interrupted.
    // Sub-agents are ordinary runs here; a parent that gets to waiting on
    // one first starts it itself, and `spawn` won't drive a run twice.
    for run in triage_running_runs(&rt.store, RESUME_MAX_AGE)? {
        // An MCP client's call: the client is gone and may retry it, so it
        // isn't run again with nobody to return the result to.
        if run.trigger == tern_core::agent::mcp::TRIGGER {
            rt.store.set_run_status(
                &run.id,
                tern_core::agent::RunStatus::Interrupted.as_str(),
                Some("tern restarted during this MCP client's call"),
            )?;
            continue;
        }
        tracing::info!(run = %run.id, "resuming run after restart");
        if run.trigger == tern_core::outbox::TRIGGER {
            manager.drive_outbox_run(run.id);
        } else {
            manager.spawn(&run.session_id, &run.id, true);
        }
    }
    // Tooling spec §5.2: items approved just before a crash, then expiry.
    manager.reconcile_outbox();
    manager.spawn_outbox_expiry(shutdown.clone());
    if let Some(h) = &rt.subagents {
        h.kick();
    }

    tokio::spawn(scheduler::run(manager.clone(), shutdown.clone()));
    manager.watch_mentions(shutdown.clone());

    let socket = home.socket_path();
    let bind = home.socket_bind_path();
    let api = api::serve(manager.clone(), &socket, &bind, shutdown.clone()).await?;
    eprintln!(
        "tern daemon running (pid {}), API on {}",
        std::process::id(),
        socket.display()
    );
    tracing::info!(socket = %socket.display(), "daemon started");

    tokio::select! {
        () = wait_for_signal() => {}
        () = routes.lifecycle.shutdown.cancelled() => {}
    }
    tracing::info!("daemon stopping");
    // Runs in progress are left `running`: the next start resumes them,
    // exactly as after a crash.
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), api).await;
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&bind);
    Ok(ExitCode::SUCCESS)
}

async fn wait_for_signal() {
    if std::env::var_os(TEST_IGNORE_SHUTDOWN_ENV).is_some() {
        // Hold SIGTERM and SIGINT so they don't kill us; only SIGKILL will.
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let _int = signal(SignalKind::interrupt());
            let _term = signal(SignalKind::terminate());
            std::future::pending::<()>().await;
        }
    }
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) {
            (Ok(mut int), Ok(mut term)) => {
                tokio::select! {
                    _ = int.recv() => {}
                    _ = term.recv() => {}
                }
            }
            _ => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
