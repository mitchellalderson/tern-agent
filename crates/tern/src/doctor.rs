//! `tern doctor`: explain what is configured, what is missing, and how to fix it.
//!
//! Read-only: never creates the data directory or the database.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use tern_core::config::{McpTransport, ProviderConfig, ProviderKind, SandboxBackendKind};
use tern_core::secrets::SecretSource;
use tern_core::{Config, Secrets, TernHome};
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

const PROVIDER_HINT: &str = "\
Add a provider to config.toml, for example:
  [providers.anthropic]
  kind = \"anthropic\"
  api_key_secret = \"ANTHROPIC_API_KEY\"
and set default_model = \"anthropic:<model>\".";

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
            Some("Set it with `tern config set default_model provider:model`."),
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

    r.section("Sandbox");
    match config.sandbox.backend {
        SandboxBackendKind::Host => r.check(
            Status::Ok,
            "host backend (shell commands run on this machine, gated by approvals)",
            None,
        ),
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
    let Some(base) = p.effective_base_url() else {
        r.check(
            Status::Fail,
            format!("{name}: openai_compat needs base_url"),
            None,
        );
        return;
    };
    if offline {
        r.check(
            Status::Info,
            format!("{name}: reachability skipped (--offline)"),
            None,
        );
        return;
    }
    let (status, msg) = runtime.block_on(probe(p.kind, base, key.as_deref()));
    r.check(status, format!("{name}: {msg}"), None);
}

/// List models as a cheap authenticated reachability probe.
async fn probe(kind: ProviderKind, base: &str, key: Option<&str>) -> (Status, String) {
    let client = match tern_core::net::http_client()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (Status::Fail, format!("http client: {e}")),
    };
    let req = match kind {
        ProviderKind::Anthropic => client
            .get(format!("{base}/v1/models"))
            .header("anthropic-version", "2023-06-01")
            .header("x-api-key", key.unwrap_or_default()),
        ProviderKind::OpenaiCompat => {
            let req = client.get(format!("{base}/models"));
            match key {
                Some(k) => req.bearer_auth(k),
                None => req,
            }
        }
    };
    match req.send().await {
        Ok(resp) if resp.status().is_success() => (Status::Ok, format!("reachable ({base})")),
        Ok(resp) if matches!(resp.status().as_u16(), 401 | 403) => (
            Status::Fail,
            format!(
                "{base} rejected the API key (HTTP {})",
                resp.status().as_u16()
            ),
        ),
        Ok(resp) => (
            Status::Warn,
            format!("{base} answered HTTP {}", resp.status().as_u16()),
        ),
        Err(e) => (
            Status::Fail,
            format!("cannot reach {base}: {}", root_cause(&e)),
        ),
    }
}

fn root_cause(e: &(dyn std::error::Error + 'static)) -> String {
    let mut cur = e;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
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

/// Read-only: inspects the PID file's lock without creating anything.
fn check_daemon(r: &mut Report, home: &TernHome) {
    let path = home.pid_path();
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    else {
        r.check(
            Status::Info,
            "not running (start it with `tern daemon`)",
            None,
        );
        return;
    };
    match file.try_lock() {
        Ok(()) => r.check(
            Status::Info,
            "not running (start it with `tern daemon`)",
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

fn find_on_path(cmd: &str) -> Option<std::path::PathBuf> {
    let candidate = Path::new(cmd);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(cmd))
        .find(|p| p.is_file())
}
