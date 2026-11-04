//! `tern mcp`: manage MCP servers from the shell (docs/mcp-management-spec.md
//! §8.3). Like `tern providers`, it talks to the running daemon when there
//! is one, else to an embedded runtime.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use tern_core::TernHome;
use tern_core::config::{McpServerConfig, McpTransport};
use tern_core::mcp::{McpPatch, McpState, McpStatus};
use tern_tui::Backend;

use crate::cli::McpCommand;
use crate::providers_cmd::{read_hidden, with_backend};

pub fn run(home: &TernHome, command: Option<McpCommand>) -> anyhow::Result<ExitCode> {
    match command.unwrap_or(McpCommand::List { json: false }) {
        McpCommand::List { json } => with_backend(home, async |b| list(b, json)),
        McpCommand::Show { name } => with_backend(home, async |b| show(b, &name)),
        McpCommand::Add {
            name,
            url,
            env,
            secrets,
            cwd,
            bearer_secret,
            header_secrets,
            approve,
            no_test,
            command,
        } => {
            let mut cfg = McpServerConfig {
                enabled: true,
                requires_approval: approve,
                ..Default::default()
            };
            match url {
                Some(url) => {
                    if !env.is_empty() || !secrets.is_empty() || cwd.is_some() {
                        bail!("--env, --secret and --cwd are for local commands, not --url");
                    }
                    cfg.transport = McpTransport::Http;
                    cfg.url = Some(url);
                    cfg.bearer_token_secret = bearer_secret;
                    cfg.header_secrets = pairs(&header_secrets, "--header-secret")?;
                }
                None => {
                    let Some((cmd, args)) = command.split_first() else {
                        bail!(
                            "give the command after `--`, e.g. `tern mcp add {name} -- npx -y some-mcp-server`, or use --url"
                        );
                    };
                    cfg.command = Some(cmd.clone());
                    cfg.args = args.to_vec();
                    cfg.env = pairs(&env, "--env")?;
                    cfg.env_secrets = secrets;
                    cfg.cwd = cwd;
                }
            }
            with_backend(home, async |b| {
                add(&b, &name, cfg, BTreeMap::new(), no_test).await
            })
        }
        McpCommand::Import { file, no_test } => {
            let text = match file {
                Some(f) => std::fs::read_to_string(&f)
                    .with_context(|| format!("reading {}", f.display()))?,
                None => {
                    if std::io::stdin().is_terminal() {
                        eprintln!("Paste the mcpServers JSON, then press Ctrl+D:");
                    }
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
            };
            with_backend(home, async |b| {
                let drafts = b.import_mcp(&text).map_err(anyhow::Error::msg)?;
                let mut failed = false;
                for d in drafts {
                    for w in &d.warnings {
                        eprintln!("{}: {w}", d.name);
                    }
                    let known = d
                        .secrets
                        .into_iter()
                        .filter_map(|(k, v)| Some((k, v?)))
                        .collect();
                    match add(&b, &d.name, d.config, known, no_test).await {
                        Ok(code) if code == ExitCode::SUCCESS => {}
                        Ok(_) => failed = true,
                        Err(e) => {
                            eprintln!("{}: {e:#}", d.name);
                            failed = true;
                        }
                    }
                }
                Ok(if failed {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                })
            })
        }
        McpCommand::Test { name } => with_backend(home, async |b| {
            let s = find(&b, &name)?;
            test(&b, &name, s.config, BTreeMap::new()).await
        }),
        McpCommand::Enable { name } => toggle(home, name, true),
        McpCommand::Disable { name } => toggle(home, name, false),
        McpCommand::Restart { name } => with_backend(home, async |b| {
            let s = b
                .restart_mcp(name.clone())
                .await
                .map_err(anyhow::Error::msg)?;
            println!("{name}: {}", state_line(&s));
            Ok(match s.state {
                McpState::Running { .. } => ExitCode::SUCCESS,
                _ => ExitCode::FAILURE,
            })
        }),
        McpCommand::Rm { name, keep_secrets } => with_backend(home, async |b| {
            b.remove_mcp(&name, !keep_secrets)
                .map_err(anyhow::Error::msg)?;
            println!("removed {name}");
            Ok(ExitCode::SUCCESS)
        }),
    }
}

fn pairs(items: &[String], flag: &str) -> anyhow::Result<BTreeMap<String, String>> {
    items
        .iter()
        .map(|p| match p.split_once('=') {
            Some((k, v)) if !k.is_empty() => Ok((k.to_owned(), v.to_owned())),
            _ => bail!("{flag} takes NAME=value, got {p:?}"),
        })
        .collect()
}

fn find(b: &Arc<dyn Backend>, name: &str) -> anyhow::Result<McpStatus> {
    b.mcp_servers()
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .find(|s| s.name == name)
        .with_context(|| format!("no MCP server named {name:?}; `tern mcp list` shows them"))
}

fn state_line(s: &McpStatus) -> String {
    let now = chrono::Utc::now().timestamp();
    match &s.state {
        McpState::Disabled => "disabled".into(),
        McpState::NotStarted => "not started (starts on first use)".into(),
        McpState::Connecting => "connecting".into(),
        McpState::Running { tools, .. } => format!("running, {tools} tools"),
        McpState::Unavailable {
            error, retry_at, ..
        } => match retry_at {
            Some(t) if *t > now => format!("unavailable, retry in {}s: {error}", t - now),
            _ => format!("unavailable: {error}"),
        },
    }
}

fn list(b: Arc<dyn Backend>, json: bool) -> anyhow::Result<ExitCode> {
    let servers = b.mcp_servers().map_err(anyhow::Error::msg)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&servers)?);
        return Ok(ExitCode::SUCCESS);
    }
    if servers.is_empty() {
        println!("No MCP servers yet. Add one with `tern mcp add` or `tern mcp import`.");
    }
    for s in servers {
        let transport = match s.config.transport {
            McpTransport::Stdio => "stdio",
            McpTransport::Http => "http",
        };
        println!("{:24} {transport:6} {}", s.name, state_line(&s));
        if !s.missing_secrets.is_empty() {
            println!(
                "{:24} missing secrets: {}",
                "",
                s.missing_secrets.join(", ")
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn show(b: Arc<dyn Backend>, name: &str) -> anyhow::Result<ExitCode> {
    let s = find(&b, name)?;
    let c = &s.config;
    println!("{}: {}", s.name, state_line(&s));
    if let Some(i) = &s.server_info {
        println!("  server     {i}");
    }
    match c.transport {
        McpTransport::Stdio => {
            println!(
                "  command    {} {}",
                c.command.as_deref().unwrap_or(""),
                c.args.join(" ")
            );
            if let Some(d) = &c.cwd {
                println!("  directory  {d}");
            }
            for (k, v) in &c.env {
                println!("  env        {k}={v}");
            }
        }
        McpTransport::Http => println!("  url        {}", c.url.as_deref().unwrap_or("")),
    }
    for n in c.secret_names() {
        let state = if s.missing_secrets.iter().any(|m| m == n) {
            "missing"
        } else {
            "set"
        };
        println!("  secret     {n} ({state})");
    }
    if !s.tools.is_empty() {
        println!("  tools:");
        for t in &s.tools {
            let hidden = if t.disabled { "  (hidden)" } else { "" };
            println!("    {}{hidden}", t.remote);
        }
    }
    if !s.stderr_tail.is_empty() {
        println!("  stderr:");
        for l in &s.stderr_tail {
            println!("    {l}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn toggle(home: &TernHome, name: String, enabled: bool) -> anyhow::Result<ExitCode> {
    with_backend(home, async |b| {
        let patch = McpPatch {
            enabled: Some(enabled),
            ..Default::default()
        };
        b.patch_mcp(&name, &patch).map_err(anyhow::Error::msg)?;
        println!("{name} {}", if enabled { "enabled" } else { "disabled" });
        Ok(ExitCode::SUCCESS)
    })
}

/// Ask for each secret `cfg` needs that isn't set or given.
fn ask_secrets(
    b: &Arc<dyn Backend>,
    name: &str,
    cfg: &McpServerConfig,
    values: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let set: Vec<String> = b
        .secrets()
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .filter(|s| s.in_env || s.in_file)
        .map(|s| s.name)
        .collect();
    for secret in cfg.secret_names() {
        if values.contains_key(secret) || set.iter().any(|s| s == secret) {
            continue;
        }
        if !std::io::stdin().is_terminal() {
            bail!(
                "{name} needs secret {secret}; set it with /secrets in the TUI, or run this in a terminal"
            );
        }
        let v = read_hidden(&format!("{name} needs {secret} (hidden): "))?;
        if !v.trim().is_empty() {
            values.insert(secret.to_owned(), v.trim().to_owned());
        }
    }
    Ok(())
}

/// Verify, print the result, and say whether it worked.
async fn test(
    b: &Arc<dyn Backend>,
    name: &str,
    cfg: McpServerConfig,
    secrets: BTreeMap<String, String>,
) -> anyhow::Result<ExitCode> {
    eprintln!("Starting {name}…");
    match b
        .verify_mcp(name.to_owned(), cfg, secrets)
        .await
        .map_err(anyhow::Error::msg)?
    {
        Ok(v) => {
            let info = v.server_info.map(|i| format!(" ({i})")).unwrap_or_default();
            println!("{name}: connected{info}, {} tools", v.tools.len());
            for t in &v.tools {
                let hidden = if t.disabled { "  (hidden)" } else { "" };
                println!("  {}{hidden}", t.remote);
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("{name}: {}", e.message);
            for l in &e.stderr_tail {
                eprintln!("  | {l}");
            }
            Ok(ExitCode::FAILURE)
        }
    }
}

async fn add(
    b: &Arc<dyn Backend>,
    name: &str,
    cfg: McpServerConfig,
    mut secrets: BTreeMap<String, String>,
    no_test: bool,
) -> anyhow::Result<ExitCode> {
    tern_core::mcp::check_name(name).map_err(anyhow::Error::msg)?;
    tern_core::mcp::check_config(&cfg).map_err(anyhow::Error::msg)?;
    ask_secrets(b, name, &cfg, &mut secrets)?;
    if !no_test && test(b, name, cfg.clone(), secrets.clone()).await? != ExitCode::SUCCESS {
        eprintln!("Not saved. Fix it, or add --no-test to save anyway.");
        return Ok(ExitCode::FAILURE);
    }
    let s = b
        .save_mcp(None, name.to_owned(), cfg, secrets)
        .await
        .map_err(anyhow::Error::msg)?;
    println!("added {} (tools appear as mcp__{}__*)", s.name, s.name);
    Ok(ExitCode::SUCCESS)
}
