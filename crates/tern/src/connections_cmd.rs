//! `tern connections`: accounts of connectors and MCP servers (tooling
//! spec §6.8.3). Works on the database and secrets.toml directly, so it
//! needs no daemon; a running one sees changes on its next run. Signing in
//! runs here: the browser opens on this machine (or `--device` prints a
//! code to enter anywhere).

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use tern_auth::Conn;
use tern_auth::signin::{Answer, Recorded, Shown};
use tern_auth::tools::{ConnectError, Target};
use tern_core::TernHome;
use tern_core::approval::AutoDeny;

use crate::cli::ConnectionsCommand;
use crate::runtime::Runtime;

/// One account, as `tern connections` and `/connections` print it.
pub(crate) fn line(c: &Conn) -> String {
    let mut out = format!("{} {} · {}", c.kind.as_str(), c.target, c.slug);
    if let Some(l) = c.label.as_deref().filter(|l| *l != c.slug) {
        out.push_str(&format!(" \"{l}\""));
    }
    if let Some(a) = &c.account {
        out.push_str(&format!(" ({a})"));
    }
    out.push_str(&format!(" · {}", c.auth.method()));
    if c.is_default {
        out.push_str(" · default");
    }
    if c.status != "ok" {
        out.push_str(&format!(" · {}", c.status.replace('_', " ")));
        if let Some(e) = &c.last_error {
            out.push_str(&format!(": {e}"));
        }
    }
    out
}

pub fn run(home: &TernHome, command: Option<ConnectionsCommand>) -> anyhow::Result<ExitCode> {
    let rt = Runtime::open(home, Arc::new(AutoDeny))?;
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    tokio.block_on(async move {
        match command.unwrap_or(ConnectionsCommand::List) {
            ConnectionsCommand::List => list(&rt),
            ConnectionsCommand::Add {
                target,
                connection,
                label,
                kind,
                key,
                device,
            } => add(&rt, &target, connection, label, kind, key, device).await,
            ConnectionsCommand::Reconnect {
                target,
                connection,
                kind,
                device,
            } => {
                let c = find(&rt, &target, &connection, kind.as_deref())?;
                let key = c.auth.method() == "key";
                add(&rt, &target, Some(connection), None, kind, key, device).await
            }
            ConnectionsCommand::Default {
                target,
                connection,
                kind,
            } => {
                let c = find(&rt, &target, &connection, kind.as_deref())?;
                rt.connections.set_default(&c).map_err(anyhow::Error::msg)?;
                println!("{target} · {connection} is now the default");
                Ok(ExitCode::SUCCESS)
            }
            ConnectionsCommand::Rename {
                target,
                connection,
                label,
                kind,
            } => {
                let c = find(&rt, &target, &connection, kind.as_deref())?;
                rt.connections.rename(&c, &label).map_err(anyhow::Error::msg)?;
                println!("renamed {target} · {connection} to \"{label}\"");
                Ok(ExitCode::SUCCESS)
            }
            ConnectionsCommand::Remove {
                target,
                connection,
                kind,
            } => {
                let c = find(&rt, &target, &connection, kind.as_deref())?;
                rt.connections.remove(&c).await.map_err(anyhow::Error::msg)?;
                println!(
                    "removed {target} · {connection}: signed out at the provider where it allows it, and its tokens, keys, cache and grants deleted"
                );
                Ok(ExitCode::SUCCESS)
            }
        }
    })
}

fn list(rt: &Runtime) -> anyhow::Result<ExitCode> {
    let all = rt.connections.all();
    if all.is_empty() {
        println!(
            "No accounts yet: every connector and MCP server uses its default one.\nAdd one with `tern connections add <connector or server>`."
        );
    }
    for c in &all {
        println!("{}", line(c));
    }
    Ok(ExitCode::SUCCESS)
}

fn find(rt: &Runtime, target: &str, slug: &str, kind: Option<&str>) -> anyhow::Result<Conn> {
    rt.connections
        .all()
        .into_iter()
        .find(|c| c.target == target && c.slug == slug && kind.is_none_or(|k| c.kind.as_str() == k))
        .with_context(|| format!("no connection {target} · {slug} (`tern connections` lists them)"))
}

/// The terminal's side of a sign-in: open the page (or print the code) and
/// wait; Ctrl-C ends it.
fn present(shown: Shown) -> Answer {
    match shown {
        Shown::Url {
            url,
            device_available,
        } => {
            eprintln!("Opening your browser to sign in. If it doesn't open, visit:\n\n  {url}\n");
            if device_available {
                eprintln!("(On another machine? Run this again with --device for a code.)");
            }
            open_browser(&url);
        }
        Shown::Device {
            user_code,
            verification_uri,
            complete,
        } => {
            eprintln!("Go to {verification_uri} and enter the code:\n\n  {user_code}\n");
            if let Some(c) = complete {
                eprintln!("(or open {c})");
            }
        }
    }
    eprintln!("Waiting for the sign-in… (Ctrl-C to stop)");
    tern_auth::signin::never()
}

fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

async fn add(
    rt: &Runtime,
    name: &str,
    slug: Option<String>,
    label: Option<String>,
    kind: Option<String>,
    key: bool,
    device: bool,
) -> anyhow::Result<ExitCode> {
    let targets = rt.auth_targets.clone();
    let target: Target = tern_auth::tools::find_target(&targets, name, kind.as_deref())
        .map_err(anyhow::Error::msg)?;
    let conns = &rt.connections;
    let slug = slug.map(|s| s.to_ascii_lowercase());
    let existing = slug
        .as_deref()
        .and_then(|s| conns.get(target.kind, &target.name, s));
    if let Some(s) = &slug
        && existing.is_none()
    {
        conns
            .check_new(target.kind, &target.name, s)
            .map_err(anyhow::Error::msg)?;
    }
    let oauth = !key && rt.auth.config_for(conns, &target).is_some();
    let result = if oauth {
        let req = tern_auth::tools::Connect {
            target: &target,
            slug: slug.as_deref(),
            label: label.as_deref(),
            kept: None,
            device,
        };
        tern_auth::tools::connect_oauth(&rt.auth, conns, req, present).await
    } else {
        if target.key_names.is_empty() {
            bail!(
                "{} has no sign-in tern knows and names no secrets; ask the agent to set it up (auth_discover)",
                target.name
            );
        }
        if !std::io::stdin().is_terminal() {
            bail!("entering a key needs a terminal");
        }
        let slug = slug
            .clone()
            .unwrap_or_else(|| tern_auth::signin::new_slug(conns, target.kind, &target.name, None));
        let mut values = BTreeMap::new();
        for n in &target.key_names {
            let v = crate::providers_cmd::read_hidden(&format!(
                "{} · {slug}: {n} (hidden): ",
                target.name
            ))?;
            values.insert(n.clone(), v);
        }
        tern_auth::tools::connect_key(conns, &target, &slug, label.as_deref(), &values)
    };
    match result {
        Ok(r) => {
            let (what, c) = match &r {
                Recorded::Connected(c) => ("connected", c),
                Recorded::Reconnected(c) => ("reconnected", c),
            };
            println!("{what}: {}", line(c));
            Ok(ExitCode::SUCCESS)
        }
        Err(ConnectError::Skipped(why)) => {
            eprintln!("not connected: {why}");
            Ok(ExitCode::FAILURE)
        }
        Err(ConnectError::TimedOut) => {
            eprintln!("not connected: the sign-in timed out");
            Ok(ExitCode::FAILURE)
        }
        Err(ConnectError::Failed(e)) => bail!(e),
    }
}
