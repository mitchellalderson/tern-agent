//! `tern daemon` (spec §12.3): one process that owns the runtime, the
//! scheduler, gateways, and the local API on `~/.tern/tern.sock`.

pub mod api;
mod api_providers;
mod api_runs;
mod api_schedules;
mod api_secrets;
mod api_subagents;
pub mod auth;
pub mod listen;
pub mod manager;
pub mod scheduler;
pub mod tls;
pub mod uploads;

use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use tern_core::TernHome;
use tern_core::agent::{RESUME_MAX_AGE, triage_running_runs};
use tern_gateway::Gateway;
use tern_gateway::telegram::{TelegramGateway, api::DEFAULT_API_URL};
use tokio_util::sync::CancellationToken;

use crate::runtime::Runtime;
use manager::{Manager, Router, Routes};

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
    let _pid = lock_pid(home)?;
    let routes = Arc::new(Routes::default());
    let rt = Arc::new(Runtime::open(home, Arc::new(Router(routes.clone())))?);
    let _ = routes.store.set(rt.store.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(home, rt, routes))
}

async fn serve(home: &TernHome, rt: Arc<Runtime>, routes: Arc<Routes>) -> anyhow::Result<ExitCode> {
    listen::check(&rt.config.api)?;
    let shutdown = CancellationToken::new();

    let mut gateways: HashMap<String, Arc<dyn Gateway>> = HashMap::new();
    let mut telegram = None;
    if let Some(t) = rt.config.gateway.telegram.as_ref().filter(|t| t.enabled) {
        let secrets = tern_core::Secrets::load(&home.secrets_path())?;
        let token = secrets.get(&t.bot_token_secret).with_context(|| {
            format!(
                "telegram is enabled but secret {} is not set",
                t.bot_token_secret
            )
        })?;
        let g = TelegramGateway::new(
            t.api_url.as_deref().unwrap_or(DEFAULT_API_URL),
            &token,
            rt.store.clone(),
        )?;
        gateways.insert(g.name().to_owned(), g.clone());
        telegram = Some(g);
    }
    let _ = routes.gateways.set(gateways);

    let manager = Manager::new(rt.clone(), routes.clone());
    if let Some(h) = &rt.subagents {
        h.set_driver(Arc::new(manager::ManagerDriver(Arc::downgrade(&manager))));
    }

    // Spec §5.3: resume recent runs a crash cut off; mark older ones interrupted.
    // Sub-agents are ordinary runs here; a parent that gets to waiting on
    // one first starts it itself, and `spawn` won't drive a run twice.
    for run in triage_running_runs(&rt.store, RESUME_MAX_AGE)? {
        tracing::info!(run = %run.id, "resuming run after restart");
        manager.spawn(&run.session_id, &run.id, true);
    }
    if let Some(h) = &rt.subagents {
        h.kick();
    }

    if let Some(g) = telegram {
        let host: Arc<dyn tern_gateway::ChatHost> = Arc::new(manager::Chats(manager.clone()));
        tokio::spawn(g.run(host, shutdown.clone()));
    }
    tokio::spawn(scheduler::run(manager.clone(), shutdown.clone()));

    let socket = home.socket_path();
    let bind = home.socket_bind_path();
    let api = api::serve(manager.clone(), &socket, &bind, shutdown.clone()).await?;
    let tcp = listen::serve(manager.clone(), home, &rt.config.api, shutdown.clone()).await?;
    eprintln!(
        "tern daemon running (pid {}), API on {}",
        std::process::id(),
        socket.display()
    );
    for (addr, _) in &tcp {
        eprintln!("  and on tcp://{addr} (device tokens required; pair with `tern devices pair`)");
    }
    if !tcp.is_empty() && rt.config.sandbox.backend == tern_core::config::SandboxBackendKind::Host {
        tracing::warn!(
            "the API is reachable over TCP and the sandbox is `host`: approved shell commands run on this machine"
        );
    }
    tracing::info!(socket = %socket.display(), "daemon started");

    wait_for_signal().await;
    tracing::info!("daemon stopping");
    // Runs in progress are left `running`: the next start resumes them,
    // exactly as after a crash.
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), api).await;
    for (_, task) in tcp {
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    }
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&bind);
    Ok(ExitCode::SUCCESS)
}

async fn wait_for_signal() {
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
