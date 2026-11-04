//! `tern daemon` (spec §12.3): one process that owns the runtime, the
//! scheduler, gateways, and the local API on `~/.tern/tern.sock`.

pub mod api;
pub mod api_agents;
mod api_bundles;
pub mod api_channels;
mod api_config;
mod api_connections;
mod api_ledger;
pub mod api_lifecycle;
mod api_mcp;
pub mod api_mcp_serve;
mod api_outbox;
mod api_providers;
mod api_runs;
mod api_schedules;
mod api_secrets;
mod api_subagents;
pub mod api_tasks;
mod api_webhooks;
pub mod auth;
pub mod channels;
pub mod delegations;
pub mod hooks;
pub mod listen;
pub mod manager;
pub mod outbox;
pub mod scheduler;
pub mod suggestions;
pub mod tls;
pub mod uploads;
pub mod waiting;
pub mod wakes;

use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use tern_core::TernHome;
use tern_core::agent::{RESUME_MAX_AGE, triage_running_runs};
use tern_gateway::Gateway;
use tern_gateway::lua::LuaGateway;
use tern_gateway::telegram::{TelegramGateway, api::DEFAULT_API_URL};
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
    listen::check(&rt.config.api)?;
    hooks::check(&rt.config)?;
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
    let agent_bots = agent_bots(&rt, home);
    for g in &agent_bots {
        if gateways.contains_key(g.name()) {
            continue;
        }
        gateways.insert(g.name().to_owned(), g.clone() as Arc<dyn Gateway>);
    }
    let lua = lua_gateways(&rt).await;
    for g in &lua {
        gateways.insert(g.name().to_owned(), g.clone());
    }
    let _ = routes.gateways.set(gateways);

    let manager = Manager::new(rt.clone(), routes.clone());
    let _ = routes.manager.set(Arc::downgrade(&manager));
    if let Some(h) = &rt.subagents {
        h.set_driver(Arc::new(manager::ManagerDriver(Arc::downgrade(&manager))));
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

    let running = telegram
        .map(|g| g as Arc<dyn Gateway>)
        .into_iter()
        .chain(agent_bots.into_iter().map(|g| g as Arc<dyn Gateway>))
        .chain(lua.into_iter().map(|g| g as Arc<dyn Gateway>));
    for g in running {
        let host: Arc<dyn tern_gateway::ChatHost> = Arc::new(manager::Chats(manager.clone()));
        let name = g.name().to_owned();
        let task = tokio::spawn(g.run(host, shutdown.clone()));
        if let Ok(mut t) = routes.lifecycle.gateway_tasks.lock() {
            t.push((name, task));
        }
    }
    tokio::spawn(scheduler::run(manager.clone(), shutdown.clone()));
    manager.watch_mentions(shutdown.clone());

    let socket = home.socket_path();
    let bind = home.socket_bind_path();
    let api = api::serve(manager.clone(), &socket, &bind, shutdown.clone()).await?;
    let tcp = listen::serve(manager.clone(), home, &rt.config.api, shutdown.clone()).await?;
    let webhooks = hooks::serve(manager.clone(), home, &rt.config, shutdown.clone()).await?;
    eprintln!(
        "tern daemon running (pid {}), API on {}",
        std::process::id(),
        socket.display()
    );
    for (addr, _) in &tcp {
        eprintln!("  and on tcp://{addr} (device tokens required; pair with `tern devices pair`)");
    }
    for (addr, _) in &webhooks {
        eprintln!("  webhooks on tcp://{addr} (each routine's own secret; never device tokens)");
    }
    if !tcp.is_empty() && rt.config.sandbox.backend == tern_core::config::SandboxBackendKind::Host {
        tracing::warn!(
            "the API is reachable over TCP and the sandbox is `host`: approved shell commands run on this machine"
        );
    }
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
    for (_, task) in tcp {
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    }
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&bind);
    Ok(ExitCode::SUCCESS)
}

/// A Telegram bot for each active agent that has its own (agent profiles
/// §6.4, AP3): one poller per token, named `telegram-<slug>`. One whose
/// secret isn't set, or that shares a token with another bot, is skipped
/// with a warning (`tern doctor` says so too); the rest still start.
fn agent_bots(rt: &Runtime, home: &TernHome) -> Vec<Arc<TelegramGateway>> {
    let Ok(agents) = rt.store.agents(false) else {
        return Vec::new();
    };
    let secrets = tern_core::Secrets::load(&home.secrets_path()).ok();
    let main = rt
        .config
        .gateway
        .telegram
        .as_ref()
        .filter(|t| t.enabled)
        .map(|t| t.bot_token_secret.clone());
    let mut used: Vec<String> = main.into_iter().collect();
    let mut out = Vec::new();
    for a in agents {
        let Some(bot) = tern_core::agents::AgentTelegram::parse(a.telegram.as_deref()) else {
            continue;
        };
        if used.contains(&bot.bot_token_secret) {
            tracing::warn!(agent = %a.slug, secret = %bot.bot_token_secret, "telegram bot skipped: another bot uses the same token");
            continue;
        }
        let Some(token) = secrets.as_ref().and_then(|s| s.get(&bot.bot_token_secret)) else {
            tracing::warn!(agent = %a.slug, secret = %bot.bot_token_secret, "telegram bot skipped: the secret isn't set");
            continue;
        };
        match TelegramGateway::for_agent(
            &a.slug,
            &a.id,
            bot.api_url.as_deref().unwrap_or(DEFAULT_API_URL),
            &token,
            rt.store.clone(),
        ) {
            Ok(g) => {
                tracing::info!(agent = %a.slug, "telegram bot started for an agent");
                used.push(bot.bot_token_secret);
                out.push(g);
            }
            Err(e) => {
                tracing::warn!(agent = %a.slug, error = %e, "telegram bot not started");
            }
        }
    }
    out
}

/// The gateway connectors `[gateway.connectors]` enables (tooling spec
/// §6.4). One that won't load is left out with a warning, so the daemon
/// and its other gateways still start; `tern doctor` says why.
async fn lua_gateways(rt: &Runtime) -> Vec<Arc<LuaGateway>> {
    let mut out = Vec::new();
    for (name, cfg) in rt
        .config
        .gateway
        .connectors
        .iter()
        .filter(|(_, c)| c.enabled)
    {
        if name == tern_gateway::telegram::SOURCE {
            tracing::warn!("a gateway connector can't be called telegram; skipped");
            continue;
        }
        let opened = match rt.lua_host() {
            Ok(mut host) => {
                host.default_rate = rt.config.connectors.rate;
                LuaGateway::open(name, host, rt.home.connectors_dir(), cfg.clone()).await
            }
            Err(e) => Err(format!("{e:#}")),
        };
        match opened {
            Ok(g) => {
                tracing::info!(gateway = %name, kind = %g.info().kind, "gateway connector started");
                out.push(g);
            }
            Err(e) => tracing::warn!(gateway = %name, error = %e, "gateway connector not started"),
        }
    }
    out
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
