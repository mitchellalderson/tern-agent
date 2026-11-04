//! `tern doctor`: explain what is configured, what is missing, and how to fix it.
//!
//! Read-only: never creates the data directory or the database.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use tern_core::config::{
    ConfineMode, McpServerConfig, McpTransport, ProviderConfig, ProviderKind, SandboxBackendKind,
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
    /// A warning shown in red: a setting that turns safety off (`yolo`).
    Danger,
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
            Status::Danger => {
                self.warns += 1;
                use std::io::IsTerminal;
                if std::io::stdout().is_terminal() {
                    println!("  \x1b[1;31m[warn] {}\x1b[0m", msg.as_ref());
                } else {
                    println!("  [warn] {}", msg.as_ref());
                }
                for line in hint.unwrap_or_default().lines() {
                    println!("         {line}");
                }
                return;
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
        Some(Ok(_)) => r.check(
            Status::Ok,
            format!(
                "default_model {}",
                config
                    .default_model
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default()
            ),
            None,
        ),
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut listed = BTreeMap::new();
    for (name, p) in &config.providers {
        if let Some(ids) = check_provider(&mut r, &runtime, &secrets, name, p, args.offline) {
            listed.insert(name.clone(), ids);
        }
    }
    check_chains(&mut r, &config, &listed);

    r.section("Database");
    check_database(&mut r, &home.db_path());

    r.section("Daemon");
    check_daemon(&mut r, home);

    r.section("Schedules");
    check_schedules(&mut r, home, &config);

    r.section("Memory");
    check_memory(&mut r, home, &config);

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

    check_shell_network(&mut r, home, &config);

    r.section("Autonomy");
    check_autonomy(&mut r, home, &config);

    r.section("Remote access");
    check_listeners(&mut r, &config);

    r.section("Webhooks");
    check_webhooks(&mut r, &runtime, home, &config, args.offline);

    r.section("Sub-agents");
    check_subagents(&mut r, &config);

    r.section("Gateways");
    match &config.gateway.telegram {
        Some(t) if t.enabled => {
            check_secret(&mut r, &secrets, "telegram bot token", &t.bot_token_secret)
        }
        _ => r.check(Status::Info, "telegram disabled", None),
    }
    for c in crate::gateway_cmd::check_connectors(home, &config) {
        if !c.enabled {
            r.check(Status::Info, format!("{}: disabled", c.name), None);
            continue;
        }
        match &c.state {
            Ok(what) if c.missing.is_empty() && c.paired == 0 => r.check(
                Status::Warn,
                format!("{}: {what}; nobody has paired", c.name),
                Some("run `tern gateway pair` and send the code as your first message"),
            ),
            Ok(what) if c.missing.is_empty() => r.check(
                Status::Ok,
                format!("{}: {what}; {} paired", c.name, c.paired),
                None,
            ),
            Ok(what) => r.check(
                Status::Fail,
                format!("{}: {what}; secret {} is not set", c.name, c.missing.join(", ")),
                Some("set it in the TUI with /secrets, or in secrets.toml (chmod 600)"),
            ),
            Err(e) => r.check(
                Status::Fail,
                format!("{}: can't run: {e}", c.name),
                Some(&format!(
                    "`tern gateway add {}` installs the built-in one, or fix ~/.tern/connectors/{}.luau",
                    c.name, c.name
                )),
            ),
        }
    }

    r.section("Connectors");
    check_connectors(&mut r, home);

    r.section("MCP servers");
    if config.mcp.servers.is_empty() {
        r.check(Status::Info, "none configured", None);
    }
    for (name, s) in &config.mcp.servers {
        check_mcp(&mut r, &runtime, &secrets, name, s, args.offline);
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

/// Active Luau connectors: each one's credentials and rate, with a warning
/// for those that read secret values themselves (`ctx.secret`, spec W2).
fn check_connectors(r: &mut Report, home: &TernHome) {
    let files = crate::connectors_cmd::luau_files_in(&home.connectors_dir());
    if files.is_empty() {
        r.check(Status::Info, "none active", None);
        return;
    }
    let host = match crate::connectors_cmd::host(home) {
        Ok(h) => h,
        Err(e) => return r.check(Status::Warn, format!("can't inspect connectors: {e}"), None),
    };
    for path in files {
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match tern_lua::load_file(&host, &path) {
            Err(e) => r.check(
                Status::Warn,
                format!("{file}: {e}"),
                Some("`tern connectors list` shows every connector"),
            ),
            Ok(vm) => {
                let c = &vm.spec.capabilities;
                let rate = c.rate.unwrap_or(host.default_rate);
                match crate::connectors_cmd::legacy_note(&vm.spec) {
                    Some(note) => r.check(
                        Status::Warn,
                        format!("{}: {note}", vm.spec.name),
                        Some(
                            "ask tern to rewrite it with capabilities.auth, then activate it again",
                        ),
                    ),
                    None if !c.auth.is_empty() => r.check(
                        Status::Ok,
                        format!(
                            "{}: credentials sent by tern ({}), {rate}",
                            vm.spec.name,
                            c.auth_secrets().join(", ")
                        ),
                        None,
                    ),
                    None => r.check(
                        Status::Ok,
                        format!("{}: no credentials, {rate}", vm.spec.name),
                        None,
                    ),
                }
            }
        }
    }
}

/// Start the server and list its tools, so the real startup error shows
/// (not just whether the command is on `PATH`). `--offline` checks only the
/// command and secrets.
fn check_mcp(
    r: &mut Report,
    runtime: &tokio::runtime::Runtime,
    secrets: &Secrets,
    name: &str,
    s: &McpServerConfig,
    offline: bool,
) {
    if !s.enabled {
        r.check(Status::Info, format!("{name}: disabled"), None);
        return;
    }
    if let Err(e) = tern_core::mcp::check_config(s) {
        r.check(Status::Fail, format!("{name}: {e}"), None);
        return;
    }
    for secret in s.secret_names() {
        check_secret(r, secrets, &format!("{name} secret"), secret);
    }
    if offline {
        match (s.transport, s.command.as_deref()) {
            (McpTransport::Stdio, Some(cmd)) => match find_on_path(cmd) {
                Some(p) => r.check(Status::Ok, format!("{name}: {}", p.display()), None),
                None => r.check(
                    Status::Fail,
                    format!("{name}: `{cmd}` not found on PATH"),
                    None,
                ),
            },
            _ => r.check(
                Status::Info,
                format!("{name}: connection skipped (--offline)"),
                None,
            ),
        }
        return;
    }
    let verified = runtime.block_on(tern_mcp::verify(
        name,
        s,
        std::sync::Arc::new(secrets.clone()),
        Default::default(),
    ));
    match verified {
        Ok(v) => r.check(
            Status::Ok,
            format!(
                "{name}: connected{}, {} tools",
                v.server_info.map(|i| format!(" ({i})")).unwrap_or_default(),
                v.tools.len()
            ),
            None,
        ),
        Err(e) => r.check(Status::Fail, format!("{name}: {}", e.message), None),
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
) -> Option<Vec<String>> {
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
            return None;
        }
        None => None,
    };
    if p.effective_base_url().is_none() {
        r.check(
            Status::Fail,
            format!("{name}: openai_compat needs base_url"),
            None,
        );
        return None;
    }
    if offline {
        r.check(
            Status::Info,
            format!("{name}: reachability skipped (--offline)"),
            None,
        );
        return None;
    }
    let (status, msg, ids) = runtime.block_on(probe(name, p, key.as_deref()));
    r.check(status, format!("{name}: {msg}"), None);
    ids
}

/// List models as a cheap authenticated reachability probe. Also returns
/// the model ids, for checking fallback chains.
async fn probe(
    name: &str,
    p: &ProviderConfig,
    key: Option<&str>,
) -> (Status, String, Option<Vec<String>>) {
    let base = p.effective_base_url().unwrap_or_default();
    match tern_providers::list_models(p, key).await {
        Ok(l) => {
            let mut msg = format!("reachable ({base}, {} models)", l.models.len());
            if let Some(url) = l.suggested_base_url {
                msg.push_str(&format!("; models answered at {url}, set base_url to that"));
            }
            let ids = l.models.into_iter().map(|m| m.id).collect();
            (Status::Ok, msg, Some(ids))
        }
        Err(e) => {
            let status = match e {
                ListError::RateLimited | ListError::NotFound | ListError::Forbidden => Status::Warn,
                _ => Status::Fail,
            };
            (
                status,
                tern_core::models::describe(&e, name, Some(base)),
                None,
            )
        }
    }
}

/// Every model in every fallback chain (T6a): it parses, its provider is
/// configured, and, where the provider answered, it is in the model list.
fn check_chains(
    r: &mut Report,
    config: &tern_core::Config,
    listed: &BTreeMap<String, Vec<String>>,
) {
    let mut heads: Vec<(String, String)> = Vec::new();
    for (key, chain) in config.model_chains() {
        if chain.fallbacks().is_empty() {
            continue;
        }
        let mut ok = true;
        for entry in chain.entries() {
            let m = match entry.parse::<tern_core::config::ModelRef>() {
                Ok(m) => m,
                Err(e) => {
                    ok = false;
                    r.check(Status::Fail, format!("{key}: {e}"), None);
                    continue;
                }
            };
            if !config.providers.contains_key(&m.provider) {
                ok = false;
                r.check(
                    Status::Fail,
                    format!(
                        "{key}: {m} uses provider {:?}, which is not configured",
                        m.provider
                    ),
                    Some("A fallback that can't be built is skipped when a run starts."),
                );
            } else if let Some(ids) = listed.get(&m.provider)
                && !ids.contains(&m.model)
            {
                ok = false;
                r.check(
                    Status::Warn,
                    format!("{key}: {m} is not in {}'s model list", m.provider),
                    Some("Check the model id; `tern models` lists what the provider offers."),
                );
            }
        }
        if let Some((earlier, _)) = heads.iter().find(|(_, h)| h == chain.primary()) {
            ok = false;
            r.check(
                Status::Warn,
                format!("{key} starts with the same model as {earlier}; only {earlier}'s fallbacks apply"),
                None,
            );
        } else {
            heads.push((key.clone(), chain.primary().to_owned()));
        }
        if ok {
            r.check(Status::Ok, format!("{key}: fallback chain {chain}"), None);
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

/// With integrations active, a shell that reaches the network is a way
/// around them: a script can call the same API directly, skipping the
/// connector's host allowlist, effect and the journal. Network is off by
/// default (a command asks with `network: true`), so this is about configs
/// that turn it on for every command, and hosts where nothing enforces off.
fn check_shell_network(r: &mut Report, home: &TernHome, config: &Config) {
    let unenforced = config.sandbox.backend == SandboxBackendKind::Host
        && !config.sandbox.network
        && matches!(
            tern_sandbox::HostBackend::new(crate::runtime::host_policy(config, home)).confinement(),
            tern_sandbox::Confinement::Off | tern_sandbox::Confinement::Unavailable(_)
        );
    if !config.sandbox.network && !unenforced {
        return;
    }
    let luau = std::fs::read_dir(home.connectors_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file() && e.path().extension().is_some_and(|x| x == "luau"))
        .count();
    let mcp = config.mcp.servers.values().filter(|s| s.enabled).count();
    if luau + mcp == 0 {
        return;
    }
    let (why, fix) = if unenforced {
        (
            "[sandbox] network = false isn't enforced without an OS sandbox",
            "Use an OS sandbox (sandbox.confine = \"auto\" on a kernel with Landlock) or \
             sandbox.backend = \"docker\". Shell output is treated as untrusted meanwhile.",
        )
    } else {
        (
            "[sandbox] network = true",
            "Remove network = true under [sandbox] (off is the default): integrations then go \
             through their tools, and a command that needs the network asks for it each time. \
             Shell output is treated as untrusted while the network is on.",
        )
    };
    r.check(
        Status::Info,
        format!(
            "shell commands can reach the network directly ({why}), \
             bypassing the allowlists of your {} active connector(s) and MCP server(s)",
            luau + mcp
        ),
        Some(fix),
    );
}

const YOLO_HINT: &str = "Nothing asks before acting, even after reading a web page or an email that could \
     carry injected instructions. Set autonomy = \"supervised\" unless you mean it.";

/// The autonomy profile (tooling spec §5.4): `yolo` anywhere is shown in
/// red, globally or on a routine.
fn check_autonomy(r: &mut Report, home: &TernHome, config: &Config) {
    use tern_core::approval::Autonomy;
    match config.autonomy {
        Autonomy::Yolo => r.check(
            Status::Danger,
            "autonomy = \"yolo\": nothing asks",
            Some(YOLO_HINT),
        ),
        a => r.check(Status::Ok, format!("autonomy = \"{a}\""), None),
    }
    let path = home.db_path();
    if !path.exists() {
        return;
    }
    let Ok(store) = Store::open(&path) else {
        return;
    };
    for s in store.schedules().unwrap_or_default() {
        let Some(a) = s.autonomy.as_deref().and_then(Autonomy::parse) else {
            continue;
        };
        let title = crate::daemon::scheduler::title(&s);
        match a {
            Autonomy::Yolo => r.check(
                Status::Danger,
                format!("routine {title:?} runs with autonomy = \"yolo\""),
                Some(YOLO_HINT),
            ),
            a => r.check(
                Status::Info,
                format!("routine {title:?}: autonomy = \"{a}\""),
                None,
            ),
        }
    }
    if let Ok(pending) = store.outbox(Some("pending"), 200)
        && !pending.is_empty()
    {
        r.check(
            Status::Info,
            format!("{} outbox item(s) waiting for you", pending.len()),
            Some("See them with `tern outbox`."),
        );
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
                .or_else(|| config.default_model.as_deref().map(str::to_owned));
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

/// `user.md` and `memory.md` against their caps (spec §7, T5a).
fn check_memory(r: &mut Report, home: &TernHome, config: &Config) {
    use tern_core::memory;
    for s in memory::statuses(home, &config.memory) {
        let name = s.file.file_name();
        match s.tokens {
            None | Some(0) => r.check(Status::Info, format!("{name}: empty"), None),
            Some(t) if s.over() => r.check(
                Status::Warn,
                format!(
                    "{name} is about {t} tokens, over its cap of {}: only the first {} are in the prompt",
                    s.cap, s.cap
                ),
                Some(&format!(
                    "Shorten {}, ask tern to consolidate it, or raise [memory].{} in config.toml.",
                    s.file.path(home).display(),
                    s.file.cap_key()
                )),
            ),
            Some(t) => r.check(
                Status::Ok,
                format!("{name}: about {t} of {} tokens", s.cap),
                None,
            ),
        }
    }
    if !config.memory.flush {
        r.check(
            Status::Info,
            "flush before compaction is off ([memory].flush = false)",
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

/// `[webhooks]` (T1c): whether a sender out there can reach this core's
/// webhook routines.
fn check_webhooks(
    r: &mut Report,
    runtime: &tokio::runtime::Runtime,
    home: &TernHome,
    config: &Config,
    offline: bool,
) {
    let path = home.db_path();
    let active = path
        .exists()
        .then(|| Store::open(&path).ok())
        .flatten()
        .and_then(|s| s.webhooks().ok())
        .map_or(0, |w| w.iter().filter(|w| w.secret.is_some()).count());
    let addrs = match crate::daemon::hooks::check(config) {
        Ok(a) => a,
        Err(e) => return r.check(Status::Fail, format!("{e:#}"), None),
    };
    if addrs.is_empty() {
        if active > 0 {
            r.check(
                Status::Warn,
                format!("{active} webhook routine(s), but no [webhooks] listener: senders can't reach them"),
                Some("Add [webhooks] listen = [\"tcp://127.0.0.1:8712\"] behind a tunnel or reverse proxy, and set webhooks.public_url."),
            );
        } else {
            r.check(
                Status::Info,
                "off (no [webhooks] listener); `tern hooks fire` works locally",
                None,
            );
        }
        return;
    }
    let tls = config.webhooks.tls.unwrap_or(config.api.tls);
    let list: Vec<String> = addrs.iter().map(|a| format!("tcp://{a}")).collect();
    r.check(
        Status::Ok,
        format!(
            "listening on {} ({tls:?} TLS), {active} active hook(s)",
            list.join(", ")
        ),
        None,
    );
    let w = &config.webhooks;
    r.check(
        Status::Info,
        format!(
            "limits: headers within {}s, bodies within {}s, at most {} open connection(s), {} KB bodies, {} deliveries a minute per routine",
            w.header_timeout_secs, w.body_timeout_secs, w.max_connections, w.max_body_kb, w.per_minute
        ),
        None,
    );
    for addr in &addrs {
        let mut local = *addr;
        if local.ip().is_unspecified() {
            local.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
        }
        if std::net::TcpStream::connect_timeout(&local, std::time::Duration::from_secs(1)).is_err()
        {
            r.check(
                Status::Warn,
                format!("nothing answers on tcp://{local}"),
                Some("Is the daemon running? `tern start`."),
            );
        }
    }
    let Some(public) = &config.webhooks.public_url else {
        if addrs.iter().all(|a| a.ip().is_loopback()) {
            r.check(
                Status::Warn,
                "the listener is on loopback and no public_url is set: only this machine can deliver",
                Some("Put a tunnel (Cloudflare Tunnel, Tailscale Funnel) or a reverse proxy in front,\nand set webhooks.public_url to its address."),
            );
        } else if let Some(base) = crate::daemon::hooks::base_url(config) {
            r.check(Status::Info, format!("senders use {base}"), None);
        }
        return;
    };
    if offline {
        r.check(
            Status::Info,
            format!("public URL {public} (not probed: --offline)"),
            None,
        );
        return;
    }
    let probe = format!("{}/healthz", public.trim_end_matches('/'));
    let answer = runtime.block_on(async {
        let resp = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?
            .get(&probe)
            .send()
            .await?;
        resp.text().await
    });
    match answer {
        Ok(t) if t.trim() == "tern webhooks" => r.check(
            Status::Ok,
            format!("reachable from outside at {public}"),
            None,
        ),
        Ok(_) => r.check(
            Status::Warn,
            format!("{public} answered, but not as this core's webhook listener"),
            Some("Check that the tunnel or proxy points at the [webhooks] listener, not the API."),
        ),
        Err(e) => r.check(
            Status::Warn,
            format!("{public} is not reachable: {e}"),
            Some("Senders will fail too. Check the tunnel, proxy or DNS, and that the certificate is valid."),
        ),
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
