//! `tern security audit` (tooling spec §8.4, W2, W6, W7): a read-only
//! report of how exposed this core is and what could act without asking.
//! Exits non-zero on a `high` finding. The same report backs
//! `GET /v1/security/audit` and the desktop's Settings, and `doctor` shows
//! its counts.

use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use tern_core::approval::{Autonomy, Egress};
use tern_core::config::{ConfineMode, SandboxBackendKind, Strangers, TlsMode};
use tern_core::{Config, TernHome};
use tern_store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
    Low,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
            Severity::Info => "info",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    /// `listeners`, `devices`, `autonomy`, `routines`, `gateways`,
    /// `connectors`, `bundles`, `sandbox`, `connections`, `egress`,
    /// `secrets`, `grants`.
    pub area: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// What it is about (a routine id, connector, device), when one thing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Audit {
    /// Most severe first.
    pub findings: Vec<Finding>,
    pub high: u32,
    pub medium: u32,
    pub low: u32,
}

impl Audit {
    fn add(&mut self, severity: Severity, area: &str, title: impl Into<String>, fix: Option<&str>) {
        self.findings.push(Finding {
            severity,
            area: area.into(),
            title: title.into(),
            fix: fix.map(Into::into),
            subject: None,
        });
    }

    fn about(&mut self, subject: &str) {
        if let Some(f) = self.findings.last_mut() {
            f.subject = Some(subject.into());
        }
    }

    fn finish(mut self) -> Self {
        self.findings.sort_by_key(|f| f.severity);
        let n = |s| self.findings.iter().filter(|f| f.severity == s).count() as u32;
        (self.high, self.medium, self.low) =
            (n(Severity::High), n(Severity::Medium), n(Severity::Low));
        self
    }

    /// `2 high, 1 medium, 3 low`, or `nothing found`.
    pub fn summary(&self) -> String {
        let parts: Vec<String> = [
            ("high", self.high),
            ("medium", self.medium),
            ("low", self.low),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(s, n)| format!("{n} {s}"))
        .collect();
        if parts.is_empty() {
            "nothing to fix".into()
        } else {
            parts.join(", ")
        }
    }
}

/// OAuth scopes that reach far more than one task needs: whole mailboxes
/// and drives, every repository, admin rights.
pub fn broad_scope(scope: &str) -> bool {
    const BROAD: &[&str] = &[
        "https://mail.google.com/",
        "https://www.googleapis.com/auth/drive",
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/gmail.modify",
        "repo",
        "delete_repo",
        "write:org",
        "admin:org",
        "workflow",
        "*",
        "all",
        "full_access",
    ];
    let s = scope.trim();
    BROAD.contains(&s)
        || s.starts_with("admin")
        || s.contains(":admin")
        || s.ends_with(".All")
        || s.ends_with(".ReadWrite.All")
}

const YOLO_FIX: &str =
    "Set autonomy = \"supervised\" (or the routine's to default) unless you mean it.";

/// Everything the audit looks at. `store` is `None` before tern has a
/// database.
pub fn audit(home: &TernHome, config: &Config, store: Option<&Store>) -> Audit {
    let mut a = Audit::default();
    listeners(&mut a, config);
    secrets_file(&mut a, home);
    autonomy(&mut a, config, store);
    code_auto(&mut a, home, config, store);
    sandbox(&mut a, home, config);
    egress(&mut a, config, store);
    gateways(&mut a, home, config, store);
    connectors(&mut a, home);
    bundles(&mut a, home);
    if let Some(store) = store {
        devices(&mut a, store);
        routines(&mut a, config, store);
        connections(&mut a, store);
        grants(&mut a, store);
    }
    a.finish()
}

fn listeners(a: &mut Audit, config: &Config) {
    match crate::daemon::listen::check(&config.api) {
        Err(e) => a.add(
            Severity::High,
            "listeners",
            format!("the API's listeners: {e:#}"),
            None,
        ),
        Ok(addrs) if addrs.is_empty() => a.add(
            Severity::Info,
            "listeners",
            "API: the local socket only (no TCP listener)",
            None,
        ),
        Ok(addrs) => {
            for addr in addrs {
                if addr.ip().is_loopback() {
                    a.add(
                        Severity::Info,
                        "listeners",
                        format!(
                            "API on tcp://{addr} (this machine only, {:?} TLS)",
                            config.api.tls
                        ),
                        None,
                    );
                } else {
                    a.add(
                        Severity::Low,
                        "listeners",
                        format!(
                            "API on tcp://{addr}: reachable from the network ({:?} TLS); paired device tokens guard it",
                            config.api.tls
                        ),
                        Some("Bind to 127.0.0.1 or a VPN address (Tailscale) unless phones need it elsewhere."),
                    );
                }
            }
            if config.api.remote_mcp_stdio {
                a.add(
                    Severity::High,
                    "listeners",
                    "api.remote_mcp_stdio = true: a paired device can start commands on this host outside the sandbox",
                    Some("Set api.remote_mcp_stdio = false; add stdio MCP servers on this machine."),
                );
            }
            if config.sandbox.backend == SandboxBackendKind::Host {
                a.add(
                    Severity::Medium,
                    "listeners",
                    "the API is on TCP and sandbox.backend = \"host\": shell commands approved from a paired device run on this machine",
                    Some("Consider sandbox.backend = \"docker\", or pair phones with --scope read."),
                );
            }
        }
    }
    match crate::daemon::hooks::check(config) {
        Err(e) => a.add(
            Severity::High,
            "listeners",
            format!("the webhook listener: {e:#}"),
            None,
        ),
        Ok(addrs) => {
            let tls = config.webhooks.tls.unwrap_or(config.api.tls);
            for addr in addrs {
                let (sev, reach) = if addr.ip().is_loopback() {
                    (Severity::Info, "this machine, or a tunnel in front")
                } else {
                    (Severity::Low, "the network")
                };
                a.add(
                    sev,
                    "listeners",
                    format!(
                        "webhooks on tcp://{addr} ({tls:?} TLS), reachable from {reach}: each routine's secret guards it, and its runs are tainted"
                    ),
                    None,
                );
                if tls == TlsMode::Off && !addr.ip().is_loopback() {
                    a.add(
                        Severity::High,
                        "listeners",
                        format!("webhooks on tcp://{addr} without TLS"),
                        None,
                    );
                }
            }
        }
    }
}

fn secrets_file(a: &mut Audit, home: &TernHome) {
    let path = home.secrets_path();
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(&path) {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            a.add(
                Severity::High,
                "secrets",
                format!("{} is readable by others (mode {mode:o})", path.display()),
                Some("chmod 600 ~/.tern/secrets.toml"),
            );
        } else {
            a.add(Severity::Info, "secrets", "secrets.toml is 0600", None);
        }
    }
    #[cfg(not(unix))]
    let _ = (a, path);
}

/// Agents the user opted in to code auto mode (docs/alpha-coding-agent.md):
/// medium, and high when the sandbox that bounds their commands isn't active.
fn code_auto(a: &mut Audit, home: &TernHome, config: &Config, store: Option<&Store>) {
    let Some(store) = store else { return };
    let agents: Vec<_> = store
        .agents(false)
        .unwrap_or_default()
        .into_iter()
        .filter(|g| g.code_auto)
        .collect();
    if agents.is_empty() {
        return;
    }
    let names = agents
        .iter()
        .map(|g| format!("@{}", g.slug))
        .collect::<Vec<_>>()
        .join(", ");
    let verified = crate::runtime::shell_sandbox_status(config, home).is_verified();
    if verified {
        a.add(
            Severity::Medium,
            "autonomy",
            format!(
                "code auto mode is on for {names}: file edits and sandboxed build and test commands run without asking, in the workspace; not fully sandboxed"
            ),
            Some("`tern agents code-auto <agent> off` turns it off. Review what it changed before you run it yourself."),
        );
    } else {
        a.add(
            Severity::High,
            "autonomy",
            format!(
                "code auto mode is on for {names} but the OS sandbox is not active: only file edits are automatic, commands still ask"
            ),
            Some("Keep sandbox.confine = \"auto\" on a host with Seatbelt or Landlock, or turn the mode off."),
        );
    }
}

fn autonomy(a: &mut Audit, config: &Config, store: Option<&Store>) {
    match config.autonomy {
        Autonomy::Yolo => a.add(
            Severity::High,
            "autonomy",
            "autonomy = \"yolo\": nothing asks, even after reading an email or a web page that could carry injected instructions",
            Some(YOLO_FIX),
        ),
        Autonomy::Autonomous => a.add(
            Severity::Info,
            "autonomy",
            "autonomy = \"autonomous\" (the default for a new install): untainted writes run without asking",
            Some("Tainted writes and destructive calls still ask, and commands run in the sandbox. Use \"supervised\" to be asked about every write."),
        ),
        other => a.add(Severity::Info, "autonomy", format!("autonomy = \"{other}\""), None),
    }
    let Some(store) = store else { return };
    for s in store.schedules().unwrap_or_default() {
        let Some(level) = s.autonomy.as_deref().and_then(Autonomy::parse) else {
            continue;
        };
        let title = crate::daemon::scheduler::title(&s);
        let (sev, what) = match level {
            Autonomy::Yolo => (Severity::High, "nothing it does asks"),
            Autonomy::Autonomous => (Severity::Medium, "its untainted writes don't ask"),
            _ => (Severity::Info, "stricter than the default"),
        };
        a.add(
            sev,
            "autonomy",
            format!("routine {title:?} runs with autonomy = \"{level}\": {what}"),
            (level == Autonomy::Yolo).then_some(YOLO_FIX),
        );
        a.about(&s.id);
    }
}

fn sandbox(a: &mut Audit, home: &TernHome, config: &Config) {
    let unconfined = config.sandbox.backend == SandboxBackendKind::Host
        && matches!(
            tern_sandbox::HostBackend::new(crate::runtime::host_policy(config, home)).confinement(),
            tern_sandbox::Confinement::Off | tern_sandbox::Confinement::Unavailable(_)
        );
    if unconfined && config.sandbox.confine == ConfineMode::Off {
        a.add(
            Severity::Medium,
            "sandbox",
            "sandbox.confine = \"off\": approved shell commands can read and write anything you can",
            Some("Set sandbox.confine = \"auto\"."),
        );
    } else if unconfined {
        a.add(
            Severity::Medium,
            "sandbox",
            "no OS sandbox for shell commands on this machine",
            Some("Use a kernel with Landlock, or sandbox.backend = \"docker\"."),
        );
    }
    match (config.sandbox.network, unconfined) {
        (true, true) => a.add(
            Severity::High,
            "sandbox",
            "[sandbox] network = true with no OS sandbox: shell commands reach any host, around every connector's allowlist",
            Some("Remove network = true under [sandbox]; a command that needs the network then asks each time."),
        ),
        (true, false) => a.add(
            Severity::Low,
            "sandbox",
            "[sandbox] network = true: shell commands reach the network directly (their output is treated as untrusted)",
            Some("Remove network = true under [sandbox] unless you need it."),
        ),
        (false, true) => a.add(
            Severity::Low,
            "sandbox",
            "[sandbox] network = false isn't enforced without an OS sandbox",
            None,
        ),
        (false, false) => {}
    }
    let v = crate::runtime::shell_sandbox_status(config, home);
    a.add(
        Severity::Info,
        "sandbox",
        if v.is_verified() {
            format!(
                "sandbox confirmed active ({}): approved python scripts can be allowed for a run",
                v.describe().trim_start_matches("verified: ")
            )
        } else {
            format!(
                "sandbox not confirmed active ({}): every python script asks",
                v.reason().unwrap_or_default()
            )
        },
        None,
    );
}

fn egress(a: &mut Audit, config: &Config, store: Option<&Store>) {
    if config.taint.egress == Egress::Allow {
        a.add(
            Severity::Medium,
            "egress",
            "[taint] egress = \"allow\": a run that read untrusted content can reach new sites without asking",
            Some("Remove egress = \"allow\" under [taint] (the default asks)."),
        );
    }
    let Some(store) = store else { return };
    for ag in store.agents(false).unwrap_or_default() {
        if ag.egress.as_deref() == Some(Egress::Allow.as_str()) {
            a.add(
                Severity::Medium,
                "egress",
                format!(
                    "agent {} has egress = \"allow\": its tainted runs reach new sites without asking",
                    ag.slug
                ),
                Some("Set the agent's \"New sites in a tainted run\" back to ask."),
            );
            a.about(&ag.id);
        }
    }
}

fn gateways(a: &mut Audit, home: &TernHome, config: &Config, store: Option<&Store>) {
    if config.gateway.telegram.as_ref().is_some_and(|t| t.enabled) {
        a.add(
            Severity::Info,
            "gateways",
            "telegram: only paired chats reach the agent",
            None,
        );
    }
    if let Some(store) = store {
        for ag in store.agents(false).unwrap_or_default() {
            if ag.telegram.is_some() {
                a.add(
                    Severity::Info,
                    "gateways",
                    format!(
                        "telegram bot for @{}: only paired chats reach it (pairing is shared with the main bot)",
                        ag.slug
                    ),
                    None,
                );
                a.about(&ag.id);
            }
        }
    }
    let checks = crate::gateway_cmd::check_connectors(home, config);
    for (name, g) in &config.gateway.connectors {
        if !g.enabled {
            continue;
        }
        let paired = checks
            .iter()
            .find(|c| &c.name == name)
            .map_or(0, |c| c.paired);
        match g.strangers {
            Strangers::Taint => a.add(
                Severity::Low,
                "gateways",
                format!("{name}: messages from anyone who hasn't paired start tainted runs"),
                Some("Their writes ask or go to the outbox. Set strangers = \"reply\" to answer them with a fixed note instead."),
            ),
            _ => a.add(
                Severity::Info,
                "gateways",
                format!("{name}: {paired} paired; others get no run"),
                None,
            ),
        }
        a.about(name);
    }
}

/// Installed bundles (tooling spec §6.5, W2): unsigned ones, and ones
/// signed by a key the user hasn't trusted.
fn bundles(a: &mut Audit, home: &TernHome) {
    for b in tern_bundle::installed(home) {
        let what = format!("bundle {} {} (from {})", b.name, b.version, b.source);
        if b.key.is_none() {
            a.add(
                Severity::Medium,
                "bundles",
                format!("{what} is unsigned"),
                Some("Install a signed release, or `tern bundle remove` it."),
            );
        } else if !b.key_trusted {
            a.add(
                Severity::Low,
                "bundles",
                format!("{what} is signed by a key you haven't trusted"),
                Some("Check the key with its author, then `tern bundle trust` it."),
            );
        }
    }
}

fn connectors(a: &mut Audit, home: &TernHome) {
    let files = crate::connectors_cmd::luau_files_in(&home.connectors_dir());
    if files.is_empty() {
        return;
    }
    // Inspecting needs no secret values: a secrets.toml the loader refuses
    // (its own finding) mustn't hide the connectors.
    let host = crate::connectors_cmd::host(home).or_else(|_| {
        let store = Store::open_in_memory()?;
        anyhow::Ok(tern_lua::Host::new(store, tern_core::Secrets::default())?)
    });
    let host = match host {
        Ok(h) => h,
        Err(e) => {
            return a.add(
                Severity::Info,
                "connectors",
                format!("can't inspect connectors: {e:#}"),
                None,
            );
        }
    };
    for path in files {
        let Ok(vm) = tern_lua::load_file(&host, &path) else {
            continue;
        };
        if vm.spec.capabilities.legacy_secrets() {
            a.add(
                Severity::Medium,
                "connectors",
                format!(
                    "connector {} reads {} itself with ctx.secret (legacy): it could send the value to any host it may reach",
                    vm.spec.name,
                    vm.spec.capabilities.secrets.join(", ")
                ),
                Some("Ask tern to move it to capabilities.auth, then activate it again."),
            );
            a.about(&vm.spec.name);
        }
    }
}

fn devices(a: &mut Audit, store: &Store) {
    let now = tern_store::now_ms();
    for d in store.devices().unwrap_or_default() {
        if d.revoked_at.is_some() {
            continue;
        }
        let idle_days = (now - d.last_seen_at.unwrap_or(d.created_at)) / 86_400_000;
        let (sev, can) = match d.scope.as_str() {
            "owner" => (
                Severity::Low,
                "everything, including config, secrets and devices",
            ),
            "chat" => (Severity::Info, "conversations, approvals and routines"),
            _ => (Severity::Info, "reading only"),
        };
        let stale = if idle_days >= 90 {
            format!("; unused for {idle_days} days")
        } else {
            String::new()
        };
        a.add(
            if stale.is_empty() {
                sev
            } else {
                Severity::Low.min(sev)
            },
            "devices",
            format!("device {} ({} scope): {can}{stale}", d.name, d.scope),
            (!stale.is_empty()).then_some("Revoke it with `tern devices revoke`."),
        );
        a.about(&d.id);
    }
}

fn routines(a: &mut Audit, config: &Config, store: &Store) {
    for s in store.schedules().unwrap_or_default() {
        if !s.enabled || !matches!(s.trigger.as_str(), "watch" | "webhook") {
            continue;
        }
        let level = s
            .autonomy
            .as_deref()
            .and_then(Autonomy::parse)
            .unwrap_or(config.autonomy);
        let source = if s.trigger == "webhook" {
            "a webhook delivery"
        } else {
            "what its watch finds"
        };
        let title = crate::daemon::scheduler::title(&s);
        let (sev, then) = match level {
            Autonomy::Yolo => (
                Severity::High,
                "and runs with yolo, so its writes don't ask",
            ),
            Autonomy::ReadOnly => (Severity::Info, "and is read-only"),
            _ => (Severity::Info, "so its writes ask or wait in the outbox"),
        };
        a.add(
            sev,
            "routines",
            format!("routine {title:?} starts tainted from {source}, {then}"),
            (level == Autonomy::Yolo).then_some(YOLO_FIX),
        );
        a.about(&s.id);
    }
}

fn connections(a: &mut Audit, store: &Store) {
    for c in store.all_connections().unwrap_or_default() {
        if c.auth_method != "oauth" {
            continue;
        }
        let scopes: Vec<String> = c
            .auth
            .as_deref()
            .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
            .and_then(|v| serde_json::from_value(v["scopes"].clone()).ok())
            .unwrap_or_default();
        let broad: Vec<&String> = scopes.iter().filter(|s| broad_scope(s)).collect();
        let name = format!("{} · {}", c.target, c.slug);
        if !broad.is_empty() {
            let shown: Vec<&str> = broad.iter().map(|s| s.as_str()).collect();
            a.add(
                Severity::Medium,
                "connections",
                format!(
                    "{name} was granted broad OAuth scopes: {}",
                    shown.join(", ")
                ),
                Some("Reconnect it with narrower scopes if its tools need less."),
            );
            a.about(&c.id);
        }
        if c.status == "needs_reauth" {
            a.add(
                Severity::Info,
                "connections",
                format!("{name} needs signing in again"),
                None,
            );
            a.about(&c.id);
        }
    }
}

fn grants(a: &mut Audit, store: &Store) {
    let all = store.allowlist_all().unwrap_or_default();
    let everything: Vec<&str> = all
        .iter()
        .filter(|(_, p)| p == "*")
        .map(|(t, _)| t.as_str())
        .collect();
    if !everything.is_empty() {
        a.add(
            Severity::Low,
            "grants",
            format!(
                "\"always\" grants for every call of: {} (they never cover tainted runs)",
                everything.join(", ")
            ),
            Some("Review them with `tern grants`."),
        );
    }
    let scripts = all
        .iter()
        .filter(|(t, p)| t == "shell" && p.starts_with("key:python script "))
        .count();
    if scripts > 0 {
        a.add(
            Severity::Info,
            "grants",
            format!(
                "{scripts} python script(s) always allowed (each one exact file version and arguments; they never cover tainted runs or an unverified sandbox)"
            ),
            Some("Review or revoke them with `tern grants`."),
        );
    }
}

pub fn render(a: &Audit) -> String {
    let mut out = String::new();
    for f in &a.findings {
        out.push_str(&format!(
            "[{:<6}] {:<11} {}\n",
            f.severity.as_str(),
            f.area,
            f.title
        ));
        if let Some(fix) = &f.fix {
            out.push_str(&format!("{:21}{fix}\n", ""));
        }
    }
    out.push_str(&format!("\n{}.\n", a.summary()));
    out
}

pub fn run(home: &TernHome, json: bool) -> anyhow::Result<ExitCode> {
    let (config, _) = Config::load(&home.config_path())?;
    let store = home
        .db_path()
        .exists()
        .then(|| Store::open(&home.db_path()).ok())
        .flatten();
    let report = audit(home, &config, store.as_ref());
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report));
    }
    Ok(if report.high > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broad_scopes_are_spotted() {
        for s in [
            "https://mail.google.com/",
            "repo",
            "admin:org",
            "Files.ReadWrite.All",
            "*",
        ] {
            assert!(broad_scope(s), "{s}");
        }
        for s in [
            "https://www.googleapis.com/auth/gmail.readonly",
            "public_repo",
            "read:user",
            "openid",
        ] {
            assert!(!broad_scope(s), "{s}");
        }
    }

    #[test]
    fn unsigned_and_untrusted_bundles_are_listed() {
        use tern_bundle::state::{InstalledBundle, Lock};
        let tmp = tempfile::tempdir().unwrap();
        let home = TernHome::new(tmp.path().join("tern"));
        let bundle = |name: &str, key: Option<&str>| InstalledBundle {
            name: name.into(),
            version: "1.0.0".into(),
            source: format!("https://example.com/{name}.tar.zst"),
            key: key.map(Into::into),
            ..InstalledBundle::default()
        };
        let lock = Lock {
            bundles: vec![bundle("plain", None), bundle("signed", Some("RWQkey"))],
        };
        let path = tern_bundle::Paths::new(&home).lock();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        lock.save(&path).unwrap();
        let mut a = Audit::default();
        bundles(&mut a, &home);
        let found: Vec<_> = a
            .findings
            .iter()
            .map(|f| (f.severity, f.title.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (
                    Severity::Medium,
                    "bundle plain 1.0.0 (from https://example.com/plain.tar.zst) is unsigned"
                ),
                (
                    Severity::Low,
                    "bundle signed 1.0.0 (from https://example.com/signed.tar.zst) is signed by a key you haven't trusted"
                ),
            ]
        );
    }

    #[test]
    fn findings_sort_and_count() {
        let mut a = Audit::default();
        a.add(Severity::Info, "x", "i", None);
        a.add(Severity::High, "x", "h", None);
        a.add(Severity::Low, "x", "l", None);
        let a = a.finish();
        assert_eq!(a.findings[0].title, "h");
        assert_eq!((a.high, a.medium, a.low), (1, 0, 1));
        assert_eq!(a.summary(), "1 high, 1 low");
        assert!(render(&a).starts_with("[high  ] x           h\n"));
    }
}
