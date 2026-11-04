//! `tern gateway pair | add | list`.

use std::io::IsTerminal;
use std::process::ExitCode;

use tern_core::config::GatewayConnectorConfig;
use tern_core::config::edit::ConfigFile;
use tern_core::{Config, Secrets, TernHome};
use tern_lua::{EXTENSION, load_fingerprinted, record_approval};
use tern_store::Store;

const CODE_TTL_MS: i64 = 10 * 60 * 1000;
/// No 0/O or 1/I, so the code survives being read off a screen.
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// An 8-character single-use code, also used for device pairing.
pub fn new_code() -> String {
    // UUIDv4 bytes come from the OS's CSPRNG; 32 symbols divide 256 evenly,
    // so taking each byte mod 32 is unbiased.
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    bytes[..8]
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect()
}

pub fn pair(home: &TernHome) -> anyhow::Result<ExitCode> {
    let (config, _) = Config::load(&home.config_path())?;
    let store = Store::open(&home.db_path())?;
    let code = new_code();
    store.create_pairing_code(&code, CODE_TTL_MS)?;
    println!("{code}");
    eprintln!(
        "Send this code to your Telegram bot (or as your first message on a gateway connector) within 10 minutes to pair."
    );
    let connectors = config.gateway.connectors.values().any(|c| c.enabled);
    if !connectors && !config.gateway.telegram.as_ref().is_some_and(|t| t.enabled) {
        eprintln!(
            "note: the Telegram gateway isn't enabled; set [gateway.telegram] enabled = true and bot_token_secret in config.toml, then run `tern daemon`."
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// A configured gateway connector, as `tern gateway list` and `doctor`
/// report it.
pub(crate) struct ConnectorCheck {
    pub name: String,
    pub enabled: bool,
    /// What it can do (`ntfy · push, replies, buttons`), or why it can't run.
    pub state: Result<String, String>,
    /// Secrets its `auth` sends that aren't set.
    pub missing: Vec<String>,
    pub paired: usize,
}

/// Check each `[gateway.connectors]` entry: the file is active, approved,
/// has a gateway that can send, and its credentials are set.
pub(crate) fn check_connectors(home: &TernHome, config: &Config) -> Vec<ConnectorCheck> {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return Vec::new();
    };
    let host = crate::connectors_cmd::host(home);
    // The real store for pairings, only when it exists (doctor creates nothing).
    let store = home
        .db_path()
        .exists()
        .then(|| Store::open(&home.db_path()).ok())
        .flatten();
    let dir = home.connectors_dir();
    config
        .gateway
        .connectors
        .iter()
        .map(|(name, cfg)| {
            let mut c = ConnectorCheck {
                name: name.clone(),
                enabled: cfg.enabled,
                state: Err(String::new()),
                missing: Vec::new(),
                paired: store
                    .as_ref()
                    .map_or(0, |s| tern_gateway::lua::paired_count(s, name)),
            };
            let host = match &host {
                Ok(h) => h,
                Err(e) => {
                    c.state = Err(format!("{e:#}"));
                    return c;
                }
            };
            if let Ok(vm) = tern_lua::load_file(host, &dir.join(format!("{name}.{EXTENSION}"))) {
                c.missing = vm
                    .spec
                    .capabilities
                    .auth_secrets()
                    .into_iter()
                    .filter(|s| host.secrets.get(s).is_none())
                    .collect();
            }
            c.state = runtime
                .block_on(tern_lua::gateway::info(host, &dir, name))
                .map(|i| {
                    let mut can = vec!["push"];
                    if i.poll {
                        can.push("replies");
                    }
                    if i.buttons {
                        can.push("buttons");
                    }
                    if i.threads {
                        can.push("threads");
                    }
                    format!("{} · {}", i.kind, can.join(", "))
                });
            c
        })
        .collect()
}

/// `tern gateway list`.
pub fn list(home: &TernHome) -> anyhow::Result<ExitCode> {
    let (config, _) = Config::load(&home.config_path())?;
    match &config.gateway.telegram {
        Some(t) if t.enabled => println!("telegram  enabled (bot token in {})", t.bot_token_secret),
        Some(_) => println!("telegram  disabled"),
        None => {}
    }
    let checks = check_connectors(home, &config);
    for c in &checks {
        let on = if c.enabled { "enabled" } else { "disabled" };
        match &c.state {
            Ok(what) => println!("{:<9} {on}: {what}; {} paired", c.name, c.paired),
            Err(e) => println!("{:<9} {on}: can't run: {e}", c.name),
        }
        for s in &c.missing {
            println!("          secret {s} is not set");
        }
    }
    if config.gateway.telegram.is_none() && checks.is_empty() {
        println!(
            "No gateways. `tern gateway add ntfy` sends routine results and approvals to your phone."
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn random_topic() -> String {
    format!("tern-{}", &uuid::Uuid::new_v4().simple().to_string()[..20])
}

/// `tern gateway add <ntfy|slack>`: install the built-in connector (approved,
/// since the user asked for it) and enable it in config.toml.
pub fn add(
    home: &TernHome,
    kind: &str,
    server: Option<&str>,
    channel: Option<&str>,
    force: bool,
) -> anyhow::Result<ExitCode> {
    let source = tern_gateway::builtin::source(kind, server).map_err(anyhow::Error::msg)?;
    let (config, _) = Config::load(&home.config_path())?;
    let old = config.gateway.connectors.get(kind);
    let mut settings = old.map(|c| c.settings.clone()).unwrap_or_default();
    let text = |s: &str| toml::Value::String(s.to_owned());
    match kind {
        "ntfy" => {
            for key in ["topic", "reply_topic"] {
                settings
                    .entry(key.into())
                    .or_insert_with(|| text(&random_topic()));
            }
        }
        _ => {
            if let Some(c) = channel {
                settings.insert("channel".into(), text(c));
            }
            if !settings.contains_key("channel") {
                anyhow::bail!(
                    "say which channel: `tern gateway add slack --channel C0123456789` (the id is at the bottom of the channel's details)"
                );
            }
        }
    }

    let path = home.connectors_dir().join(format!("{kind}.{EXTENSION}"));
    match std::fs::read_to_string(&path) {
        Ok(existing) if existing != source && !force => anyhow::bail!(
            "{} exists and differs from the built-in {kind} connector; pass --force to replace it",
            path.display()
        ),
        _ => std::fs::write(&path, &source)?,
    }
    // The user installed it: record it as approved, as activation would.
    let host = tern_lua::Host::new(
        Store::open(&home.db_path())?,
        Secrets::load(&home.secrets_path())?,
    )?;
    let loaded = load_fingerprinted(&host, &path)?;
    record_approval(&host, kind, &loaded.fingerprint).map_err(anyhow::Error::msg)?;

    let cfg = GatewayConnectorConfig {
        enabled: true,
        settings,
        ..old.cloned().unwrap_or_default()
    };
    let mut file = ConfigFile::open(&home.config_path())?;
    file.upsert_gateway_connector(kind, &cfg)?;
    file.save()?;

    let setting = |k: &str| {
        cfg.settings
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    println!(
        "Installed {} and enabled [gateway.connectors.{kind}].",
        path.display()
    );
    if kind == "ntfy" {
        let server = server.unwrap_or("https://ntfy.sh").trim_end_matches('/');
        let (topic, reply) = (setting("topic"), setting("reply_topic"));
        println!(
            "\n1. In the ntfy app, subscribe to {server}/{topic} (tern pushes there).\n\
             2. Also subscribe to {server}/{reply} and write there to talk to tern.\n\
             3. Run `tern gateway pair` and send the code as your first message on {reply}.\n\
             4. Restart the daemon. Routines can then deliver to ntfy:{topic}.\n\n\
             Anyone who knows these topic names can read or write them: keep them private."
        );
    } else {
        let secret = "SLACK_BOT_TOKEN";
        if host.secrets.get(secret).is_none() {
            if std::io::stdin().is_terminal() {
                let v = crate::providers_cmd::read_hidden(&format!(
                    "Slack bot token, xoxb-… ({secret}, hidden; Enter to skip): "
                ))?;
                if !v.trim().is_empty() {
                    host.secrets.set(secret, v.trim())?;
                }
            } else {
                eprintln!(
                    "note: set the secret {secret} (the bot's xoxb-… token) before starting the daemon."
                );
            }
        }
        println!(
            "\n1. The Slack app needs the bot scopes chat:write and channels:history (groups:history\n   \
             for a private channel), installed to your workspace; invite it to the channel.\n\
             2. Run `tern gateway pair` and post the code in the channel.\n\
             3. Restart the daemon. Routines can then deliver to slack:{}.",
            setting("channel")
        );
    }
    Ok(ExitCode::SUCCESS)
}
