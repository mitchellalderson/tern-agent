//! `tern doctor`: explain what is configured, what is missing, and how to fix it.
//!
//! Read-only: never creates the data directory or the database.

use std::path::Path;
use std::process::ExitCode;

use tern_core::config::{
    ConfineMode, McpTransport, ProviderConfig, ProviderKind, SandboxBackendKind,
};
use tern_core::secrets::SecretSource;
use tern_core::{Config, Secrets, TernHome};
use tern_providers::ListError;
use tern_store::Store;

use crate::cli::DoctorArgs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Info,
    Warn,
    Fail,
}

struct Report {
    fails: usize,
    warns: usize,
}

impl Report {
    fn section(&self, title: &str) {
        println!("\n{title}");
    }

    fn check(&mut self, status: Status, msg: impl AsRef<str>, hint: Option<&str>) {
        let mark = match status {
            Status::Ok => "ok  ",
            Status::Info => "--  ",
            Status::Warn => {
                self.warns += 1;
                "warn"
            }
            Status::Fail => {
                self.fails += 1;
                "FAIL"
            }
        };
        println!("  [{mark}] {}", msg.as_ref());
        if let Some(hint) = hint {
            for line in hint.lines() {
                println!("         {line}");
            }
        }
    }
}

const PROVIDER_HINT: &str = "Run `tern setup` to connect one.";

pub fn run(home: &TernHome, args: &DoctorArgs) -> anyhow::Result<ExitCode> {
    let mut r = Report { fails: 0, warns: 0 };
    println!("tern {}", env!("CARGO_PKG_VERSION"));

    r.section("Data directory");
    let root = home.root();
    if root.is_dir() {
        r.check(Status::Ok, format!("{}", root.display()), None);
    } else {
        r.check(
            Status::Warn,
            format!("{} does not exist", root.display()),
            Some("Run `tern config init` to create it (or set TERN_HOME to use another location)."),
        );
    }

    r.section("Configuration");
    let config = match Config::load(&home.config_path()) {
        Ok((c, true)) => {
            r.check(
                Status::Ok,
                format!("{} parsed", home.config_path().display()),
                None,
            );
            c
        }
        Ok((c, false)) => {
            r.check(
                Status::Warn,
                "config.toml not found; using defaults",
                Some("Run `tern config init` to write a commented example."),
            );
            c
        }
        Err(e) => {
            r.check(Status::Fail, e.to_string(), None);
            Config::default()
        }
    };

    let secrets = match Secrets::load(&home.secrets_path()) {
        Ok(s) => {
            if home.secrets_path().exists() {
                r.check(Status::Ok, "secrets.toml permissions are 0600", None);
            } else {
                r.check(
                    Status::Info,
                    "no secrets.toml (secrets will be read from environment variables only)",
                    None,
                );
            }
            s
        }
        Err(e) => {
            r.check(Status::Fail, e.to_string(), None);
            Secrets::default()
        }
    };

    let workspace = config.workspace_dir();
    if workspace.is_dir() {
        r.check(
            Status::Ok,
            format!("workspace {}", workspace.display()),
            None,
        );
    } else {
        r.check(
            Status::Warn,
            format!("workspace {} does not exist", workspace.display()),
            Some("`tern config init` creates it, or set `workspace` in config.toml."),
        );
    }

    r.section("Model providers");
    if config.providers.is_empty() {
        r.check(Status::Warn, "no providers configured", Some(PROVIDER_HINT));
    }
    match config.default_model_ref() {
        None => r.check(
            Status::Warn,
            "default_model is not set",
            Some("Pick one with `tern setup` (or `/model` in the chat)."),
        ),
        Some(Err(e)) => r.check(Status::Fail, e.to_string(), None),
        Some(Ok(m)) if !config.providers.contains_key(&m.provider) => r.check(
            Status::Fail,
            format!(
                "default_model {m} uses provider {:?}, which is not configured",
                m.provider
            ),
            None,
        ),
        Some(Ok(m)) => r.check(Status::Ok, format!("default_model {m}"), None),
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    for (name, p) in &config.providers {
        check_provider(&mut r, &runtime, &secrets, name, p, args.offline);
    }

    r.section("Database");
    check_database(&mut r, &home.db_path());

    r.section("Daemon");
    check_daemon(&mut r, home);

    r.section("Schedules");
    check_schedules(&mut r, home, &config);

    r.section("Sandbox");
    match config.sandbox.backend {
        SandboxBackendKind::Host => {
            let backend =
                tern_sandbox::HostBackend::new(crate::runtime::host_policy(&config, home));
            let net = if config.sandbox.network {
                "network on"
            } else {
                "network off"
            };
            match backend.confinement() {
                tern_sandbox::Confinement::Off => r.check(
                    Status::Warn,
                    "host backend without an OS sandbox (sandbox.confine = \"off\"): approved commands can read and write anything you can",
                    Some("Set sandbox.confine = \"auto\"."),
                ),
                tern_sandbox::Confinement::Unavailable(why) => r.check(
                    if config.sandbox.confine == ConfineMode::Required {
                        Status::Fail
                    } else {
                        Status::Warn
                    },
                    format!("host backend, OS sandbox unavailable: {why}"),
                    Some("Shell commands run unconfined (or not at all with confine = \"required\"). Use a kernel with Landlock, or sandbox.backend = \"docker\"."),
                ),
                c => r.check(
                    Status::Ok,
                    format!("host backend in {c}, {net} (writes: workspace and temp dirs)"),
                    None,
                ),
            }
        }
        SandboxBackendKind::Docker => match find_on_path("docker") {
            Some(p) => r.check(
                Status::Ok,
                format!("docker backend ({})", p.display()),
                None,
            ),
            None => r.check(
                Status::Fail,
                "sandbox.backend = \"docker\" but `docker` is not on PATH",
                Some("Install Docker, or set sandbox.backend = \"host\"."),
            ),
        },
    }

    r.section("Remote access");
    check_listeners(&mut r, &config);

    r.section("Sub-agents");
    check_subagents(&mut r, &config);

    r.section("Gateways");
    match &config.gateway.telegram {
        Some(t) if t.enabled => {
            check_secret(&mut r, &secrets, "telegram bot token", &t.bot_token_secret)
        }
        _ => r.check(Status::Info, "telegram disabled", None),
    }

    r.section("MCP servers");
    if config.mcp.servers.is_empty() {
        r.check(Status::Info, "none configured", None);
    }
    for (name, s) in &config.mcp.servers {
        match s.transport {
            McpTransport::Stdio => match s.command.as_deref() {
                None => r.check(
                    Status::Fail,
                    format!("{name}: stdio transport needs `command`"),
                    None,
                ),
                Some(cmd) => match find_on_path(cmd) {
                    Some(p) => r.check(Status::Ok, format!("{name}: {}", p.display()), None),
                    None => r.check(
                        Status::Fail,
                        format!("{name}: `{cmd}` not found on PATH"),
                        None,
                    ),
                },
            },
            McpTransport::Http => match s.url.as_deref() {
                None => r.check(
                    Status::Fail,
                    format!("{name}: http transport needs `url`"),
                    None,
                ),
                Some(url) => r.check(Status::Ok, format!("{name}: {url}"), None),
            },
        }
        for secret in &s.env_secrets {
            check_secret(&mut r, &secrets, &format!("{name} env"), secret);
        }
    }

    println!();
    if r.fails > 0 {
        println!("{} problem(s), {} warning(s).", r.fails, r.warns);
        Ok(ExitCode::FAILURE)
    } else if r.warns > 0 {
        println!("No problems; {} thing(s) to set up.", r.warns);
        Ok(ExitCode::SUCCESS)
    } else {
        println!("All good.");
        Ok(ExitCode::SUCCESS)
    }
}

fn check_secret(r: &mut Report, secrets: &Secrets, what: &str, name: &str) {
    match secrets.source(name) {
        Some(SecretSource::Env) => r.check(
            Status::Ok,
            format!("{what}: {name} (from environment)"),
            None,
        ),
        Some(SecretSource::File) => r.check(
            Status::Ok,
            format!("{what}: {name} (from secrets.toml)"),
            None,
        ),
        None => r.check(
            Status::Fail,
            format!("{what}: secret {name} is not set"),
            Some(&format!(
                "export {name}=... or add `{name} = \"...\"` to secrets.toml (chmod 600)."
            )),
        ),
    }
}

fn check_provider(
    r: &mut Report,
    runtime: &tokio::runtime::Runtime,
    secrets: &Secrets,
    name: &str,
    p: &ProviderConfig,
    offline: bool,
) {
    let key = match &p.api_key_secret {
        Some(secret) => {
            check_secret(r, secrets, name, secret);
            secrets.get(secret)
        }
        None if p.kind == ProviderKind::Anthropic => {
            r.check(
                Status::Fail,
                format!("{name}: anthropic needs api_key_secret"),
                None,
            );
            return;
        }
        None => None,
    };
    if p.effective_base_url().is_none() {
        r.check(
            Status::Fail,
            format!("{name}: openai_compat needs base_url"),
            None,
        );
        return;
    }
    if offline {
        r.check(
            Status::Info,
            format!("{name}: reachability skipped (--offline)"),
            None,
        );
        return;
    }
    let (status, msg) = runtime.block_on(probe(name, p, key.as_deref()));
    r.check(status, format!("{name}: {msg}"), None);
}

/// List models as a cheap authenticated reachability probe.
async fn probe(name: &str, p: &ProviderConfig, key: Option<&str>) -> (Status, String) {
    let base = p.effective_base_url().unwrap_or_default();
    match tern_providers::list_models(p, key).await {
        Ok(l) => {
            let mut msg = format!("reachable ({base}, {} models)", l.models.len());
            if let Some(url) = l.suggested_base_url {
                msg.push_str(&format!("; models answered at {url}, set base_url to that"));
            }
            (Status::Ok, msg)
        }
        Err(e) => {
            let status = match e {
                ListError::RateLimited | ListError::NotFound | ListError::Forbidden => Status::Warn,
                _ => Status::Fail,
            };
            (status, tern_core::models::describe(&e, name, Some(base)))
        }
    }
}

fn check_database(r: &mut Report, path: &Path) {
    if !path.exists() {
        r.check(Status::Info, "not created yet (created on first run)", None);
        return;
    }
    match Store::open(path) {
        Err(e) => r.check(Status::Fail, format!("{}: {e}", path.display()), None),
        Ok(store) => match (store.schema_version(), store.integrity_check()) {
            (Ok(v), Ok(problems)) if problems.is_empty() => r.check(
                Status::Ok,
                format!("{} (schema v{v}, integrity ok)", path.display()),
                None,
            ),
            (_, Ok(problems)) => r.check(
                Status::Fail,
                format!("integrity check failed: {}", problems.join("; ")),
                None,
            ),
            (Err(e), _) | (_, Err(e)) => r.check(Status::Fail, e.to_string(), None),
        },
    }
}

/// Schedules a daily cap has paused, USD caps that can't apply because the
/// model's price is unknown, and deliveries waiting for quiet hours to end.
fn check_schedules(r: &mut Report, home: &TernHome, config: &Config) {
    let path = home.db_path();
    if !path.exists() {
        r.check(Status::Info, "none yet", None);
        return;
    }
    let Ok(store) = Store::open(&path) else {
        return;
    };
    let Ok(schedules) = store.schedules() else {
        return;
    };
    if schedules.is_empty() {
        r.check(Status::Info, "none", None);
        return;
    }
    let (tx, _) = tokio::sync::broadcast::channel(1);
    let models =
        crate::providers::ModelCache::load(home.root().join("cache").join("models.json"), tx);
    let now = tern_store::now_ms();
    let mut fine = 0;
    for s in &schedules {
        let title = crate::daemon::scheduler::title(s);
        let mut ok = true;
        if let Some(until) = s.paused_until.filter(|u| *u > now) {
            ok = false;
            let when = chrono::DateTime::from_timestamp_millis(until)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format("%a %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            r.check(
                Status::Warn,
                format!(
                    "“{title}” is paused by its budget ({}) until {when}",
                    s.paused_reason.as_deref().unwrap_or("daily cap")
                ),
                Some("Resume it in the app or ask tern to (schedule_update with resume), or raise the cap."),
            );
        }
        if s.usd_per_day.or(config.schedules.usd_per_day).is_some() {
            let model = store
                .session(&s.session_id)
                .ok()
                .flatten()
                .and_then(|sess| sess.model)
                .or_else(|| config.default_model.clone());
            if let Some(m) =
                model.filter(|m| crate::providers::price_of(config, &models, m).is_none())
            {
                ok = false;
                r.check(
                    Status::Warn,
                    format!("“{title}”: usd_per_day is ignored, no price is known for {m}"),
                    Some("Add it under [providers.<name>.prices] in config.toml (USD per million tokens)."),
                );
            }
        }
        if ok && s.enabled {
            fine += 1;
        }
    }
    if fine > 0 {
        r.check(
            Status::Ok,
            format!("{fine} schedule(s) running normally"),
            None,
        );
    }
    if let Ok(n) = store.held_delivery_count()
        && n > 0
    {
        r.check(
            Status::Info,
            format!("{n} delivery(ies) held until quiet hours end"),
            None,
        );
    }
}

/// Read-only: inspects the PID file's lock without creating anything.
fn check_daemon(r: &mut Report, home: &TernHome) {
    if let Some(s) = crate::service::state() {
        r.check(
            Status::Info,
            format!("managed by: {}", crate::service::describe(&s)),
            None,
        );
    }
    let path = home.pid_path();
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    else {
        r.check(
            Status::Info,
            "not running (start it with `tern start`)",
            None,
        );
        return;
    };
    match file.try_lock() {
        Ok(()) => r.check(
            Status::Info,
            "not running (start it with `tern start`)",
            None,
        ),
        Err(_) => {
            let pid = std::fs::read_to_string(&path).unwrap_or_default();
            r.check(
                Status::Ok,
                format!(
                    "running (pid {}), API on {}",
                    pid.trim(),
                    home.socket_path().display()
                ),
                None,
            );
        }
    }
}

/// `[api]` (deployment spec §6.1, §6.6).
fn check_listeners(r: &mut Report, config: &tern_core::Config) {
    let addrs = match crate::daemon::listen::check(&config.api) {
        Ok(a) => a,
        Err(e) => return r.check(Status::Fail, format!("{e:#}"), None),
    };
    if addrs.is_empty() {
        return r.check(Status::Info, "local socket only (no TCP listener)", None);
    }
    let list: Vec<String> = addrs.iter().map(|a| format!("tcp://{a}")).collect();
    r.check(
        Status::Ok,
        format!(
            "listening on {} ({:?} TLS)",
            list.join(", "),
            config.api.tls
        ),
        None,
    );
    if config.sandbox.backend == SandboxBackendKind::Host {
        r.check(
            Status::Warn,
            "the API is reachable over TCP and sandbox.backend = \"host\": approved shell commands from paired devices run on this machine",
            Some("Consider sandbox.backend = \"docker\", or pair phones with --scope read."),
        );
    }
}

/// `[subagents]`: every model it names must parse and use a configured
/// provider.
fn check_subagents(r: &mut Report, config: &tern_core::Config) {
    let sub = &config.subagents;
    if !sub.enabled {
        r.check(Status::Info, "disabled ([subagents] enabled = false)", None);
        return;
    }
    r.check(
        Status::Ok,
        format!(
            "enabled: depth {}, {} at once per run ({} in all), {} per run",
            sub.max_depth, sub.max_concurrent, sub.max_concurrent_global, sub.max_children
        ),
        None,
    );
    let named = sub
        .default_model
        .iter()
        .map(|m| ("default_model".to_owned(), m))
        .chain(sub.models.iter().map(|(k, m)| (format!("models.{k}"), m)));
    for (key, model) in named {
        match model.parse::<tern_core::config::ModelRef>() {
            Err(e) => r.check(Status::Fail, format!("subagents.{key}: {e}"), None),
            Ok(m) if !config.providers.contains_key(&m.provider) => r.check(
                Status::Fail,
                format!(
                    "subagents.{key} = {m} uses provider {:?}, which is not configured",
                    m.provider
                ),
                None,
            ),
            Ok(_) => {}
        }
    }
}

fn find_on_path(cmd: &str) -> Option<std::path::PathBuf> {
    let candidate = Path::new(cmd);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(cmd))
        .find(|p| p.is_file())
}
