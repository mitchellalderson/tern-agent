//! `tern devices`: pair, list, revoke and rename remote devices
//! (deployment spec §6.2), and `join` to pair this machine with a core.

use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use tern_core::{Config, TernHome};
use tern_store::Store;

use crate::cli::DevicesCommand;
use crate::daemon::auth::{self, PairBody, PairLink, Paired, Scope};

pub fn run(home: &TernHome, command: Option<DevicesCommand>) -> anyhow::Result<ExitCode> {
    match command.unwrap_or(DevicesCommand::List) {
        DevicesCommand::Pair { scope, name, url } => pair(home, &scope, name, url)?,
        DevicesCommand::List => list(home)?,
        DevicesCommand::Revoke { id } => revoke(home, &id)?,
        DevicesCommand::Rename { id, name } => {
            if !open_store(home)?.rename_device(&id, &name)? {
                bail!("no device {id}");
            }
        }
        DevicesCommand::Join { link, name } => join(&link, name)?,
    }
    Ok(ExitCode::SUCCESS)
}

fn open_store(home: &TernHome) -> anyhow::Result<Store> {
    home.ensure_layout()?;
    Ok(Store::open(&home.db_path())?)
}

fn pair(
    home: &TernHome,
    scope: &str,
    name: Option<String>,
    url: Option<String>,
) -> anyhow::Result<()> {
    let scope: Scope = scope.parse()?;
    let (config, _) = Config::load(&home.config_path())?;
    let store = open_store(home)?;
    let offer = auth::offer_pairing(home, &config.api, &store, scope, name, url)?;
    match &offer.link {
        Some(link) => {
            if std::io::stdout().is_terminal()
                && let Ok(qr) = qrcode::QrCode::new(link.as_bytes())
            {
                println!(
                    "{}",
                    qr.render::<qrcode::render::unicode::Dense1x2>()
                        .quiet_zone(true)
                        .build()
                );
            }
            println!("{link}");
            eprintln!(
                "Scan or paste this link on the device within 10 minutes (code {}, {} scope).",
                offer.code,
                scope.as_str()
            );
            if let Some(fp) = &offer.fingerprint {
                eprintln!("Certificate fingerprint: {fp}");
            }
        }
        None => {
            println!("{}", offer.code);
            eprintln!(
                "note: no TCP listener is configured, so devices can't reach this core yet. Add one, e.g.\n  tern config set api.listen '[\"unix\", \"tcp://0.0.0.0:7420\"]'\nthen restart `tern daemon` and pair again."
            );
        }
    }
    Ok(())
}

fn list(home: &TernHome) -> anyhow::Result<()> {
    let devices = open_store(home)?.devices()?;
    if devices.is_empty() {
        println!("No paired devices. Pair one with `tern devices pair`.");
        return Ok(());
    }
    let when = |ms: i64| {
        chrono::DateTime::from_timestamp_millis(ms)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default()
    };
    println!(
        "{:<36}  {:<20}  {:<5}  {:<8}  {:<16}  STATUS",
        "ID", "NAME", "SCOPE", "PLATFORM", "LAST SEEN"
    );
    for d in devices {
        let status = match d.revoked_at {
            Some(t) => format!("revoked {}", when(t)),
            None => "active".into(),
        };
        println!(
            "{:<36}  {:<20}  {:<5}  {:<8}  {:<16}  {status}",
            d.id,
            d.name.chars().take(20).collect::<String>(),
            d.scope,
            d.platform.unwrap_or_default(),
            d.last_seen_at.map(when).unwrap_or_else(|| "never".into()),
        );
    }
    Ok(())
}

/// Through the daemon when it's running, so the device's open streams close
/// at once; otherwise straight in the database.
fn revoke(home: &TernHome, id: &str) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let via_daemon = runtime.block_on(async {
        let socket = home.socket_connect_path().ok()?;
        if !socket.exists() {
            return None;
        }
        let http = reqwest::Client::builder()
            .unix_socket(socket)
            .timeout(Duration::from_secs(5))
            .build()
            .ok()?;
        let resp = http
            .delete(format!("http://tern/v1/devices/{id}"))
            .send()
            .await
            .ok()?;
        Some(resp.status().is_success())
    });
    let revoked = match via_daemon {
        Some(ok) => ok,
        None => open_store(home)?.revoke_device(id)?,
    };
    if !revoked {
        bail!("no active device {id}");
    }
    eprintln!("Revoked {id}.");
    Ok(())
}

fn join(link: &str, name: Option<String>) -> anyhow::Result<()> {
    let link: PairLink = link.parse()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let paired: Paired = runtime.block_on(async {
        let http = crate::daemon::tls::client(link.fingerprint.as_deref())?
            .timeout(Duration::from_secs(15))
            .build()?;
        let resp = http
            .post(format!("{}/v1/auth/pair", link.url.trim_end_matches('/')))
            .json(&PairBody {
                code: link.code.clone(),
                device_name: Some(name.unwrap_or_else(crate::daemon::tls::host_name)),
                platform: Some(std::env::consts::OS.into()),
            })
            .send()
            .await
            .with_context(|| format!("reaching {}", link.url))?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        if !status.is_success() {
            bail!(
                "pairing failed: {}",
                body["error"].as_str().unwrap_or(status.as_str())
            );
        }
        Ok(serde_json::from_value(body)?)
    })?;
    println!("{}", paired.token);
    let pin = link
        .fingerprint
        .map(|fp| format!(" --pin {fp}"))
        .unwrap_or_default();
    eprintln!(
        "Paired with {} as device {} ({} scope). Keep the token above secret; to chat:\n  TERN_REMOTE_TOKEN=<token> tern --remote {}{pin}",
        link.name.as_deref().unwrap_or(&link.url),
        paired.device_id,
        paired.scope.as_str(),
        link.url,
    );
    Ok(())
}
