//! `tern service install | uninstall | status` (personal-agent-tooling-spec
//! §11.5, T8a): run the daemon under the user's service manager.
//!
//! - macOS: a LaunchAgent `ai.tern.daemon` with
//!   `KeepAlive = { SuccessfulExit = false }`, so launchd restarts a crash
//!   but not a clean exit. That one key is what lets `tern stop` and the
//!   app's Quit stick.
//! - Linux: a systemd user unit `tern-daemon.service` with
//!   `Restart=on-failure`, for the same reason.
//!
//! The service sets `TERN_SERVICE` in the daemon's environment, which the
//! daemon reports in `/v1/status` and uses to exit cleanly (rather than in
//! a restart loop) when another daemon already holds the lock.
//!
//! On macOS there are two ways to register (T8b):
//! - **From the app bundle** (`tern.app/Contents/Library/LoginItems/tern
//!   Tray.app`, whose `tern-tray` is this binary): `SMAppService`, an agent
//!   for `ai.tern.daemon` from the plist bundled beside it and the tray
//!   bundle itself as a login item. Both show in System Settings › General
//!   › Login Items under tern.
//! - **Anywhere else** (a CLI install): plain LaunchAgent plists in
//!   `~/Library/LaunchAgents`, `ai.tern.daemon` and, with `--tray`,
//!   `ai.tern.tray`.
//!
//! `TERN_LAUNCHCTL` and `TERN_SYSTEMCTL` replace the service manager's
//! command, so tests never touch the real launchd or systemd.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, bail};
use tern_core::TernHome;

pub const LABEL: &str = "ai.tern.daemon";
/// The plain LaunchAgent that starts `tern tray` at login.
pub const TRAY_LABEL: &str = "ai.tern.tray";
pub const UNIT: &str = "tern-daemon.service";
/// Set in the daemon's environment by the service: `launchd` or `systemd`.
pub const SERVICE_ENV: &str = "TERN_SERVICE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Launchd,
    Systemd,
}

impl Kind {
    pub fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::Launchd)
        } else if cfg!(target_os = "linux") {
            Some(Self::Systemd)
        } else {
            None
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd",
        }
    }

    /// The plain tray LaunchAgent (launchd only).
    pub fn tray_path() -> anyhow::Result<PathBuf> {
        let home = tern_core::home::user_home().context("$HOME is not set")?;
        Ok(home
            .join("Library/LaunchAgents")
            .join(format!("{TRAY_LABEL}.plist")))
    }

    /// Where the plist or unit lives.
    pub fn path(self) -> anyhow::Result<PathBuf> {
        let home = tern_core::home::user_home().context("$HOME is not set")?;
        Ok(match self {
            Self::Launchd => home
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
            Self::Systemd => std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map_or_else(|| home.join(".config"), PathBuf::from)
                .join("systemd/user")
                .join(UNIT),
        })
    }
}

/// How tern is registered with launchd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// A plist or unit file tern wrote.
    File,
    /// `SMAppService`, from the app bundle.
    AppService,
}

/// What the service manager knows about tern.
#[derive(Debug, Clone, Copy)]
pub struct State {
    pub kind: Kind,
    pub method: Method,
    /// The plist or unit file exists, or the app service is registered.
    pub installed: bool,
    /// launchd has the job loaded / systemd has the unit enabled or active.
    pub loaded: bool,
    /// The service manager's own process is running (not a hand-started
    /// daemon).
    pub active: bool,
    /// Starts at login.
    pub at_login: bool,
    /// Registered, but waiting for the user to allow it in System Settings.
    pub needs_approval: bool,
    /// Whether the tray starts at login; `None` when that can't be told
    /// from here (an app-service login item, seen from outside the app).
    pub tray_at_login: Option<bool>,
}

/// Paths in and around the macOS app bundles. Pure, so tested anywhere,
/// but only the macOS tray and its login items use it.
#[cfg_attr(not(all(feature = "tray", target_os = "macos")), allow(dead_code))]
pub mod bundle {
    use std::path::{Path, PathBuf};

    /// The desktop app.
    pub const DESKTOP_ID: &str = "dev.tern.ternDesktop";
    /// The daemon's agent plist, in the tray bundle's
    /// `Contents/Library/LaunchAgents`.
    pub const DAEMON_PLIST: &str = "ai.tern.daemon.plist";

    /// The `.app` whose main executable is `exe`
    /// (`<X>.app/Contents/MacOS/<exe>`).
    pub fn app_of(exe: &Path) -> Option<PathBuf> {
        let macos = exe.parent()?;
        let contents = macos.parent()?;
        let app = contents.parent()?;
        (macos.file_name()? == "MacOS"
            && contents.file_name()? == "Contents"
            && app.extension()? == "app")
            .then(|| app.to_path_buf())
    }

    /// The bundled daemon plist of the `.app` around `exe`.
    pub fn daemon_plist(exe: &Path) -> Option<PathBuf> {
        Some(
            app_of(exe)?
                .join("Contents/Library/LaunchAgents")
                .join(DAEMON_PLIST),
        )
    }

    /// `tern.app` around the tray bundle that `exe` runs in.
    pub fn host_app(exe: &Path) -> Option<PathBuf> {
        let tray = app_of(exe)?;
        let items = tray.parent()?;
        let library = items.parent()?;
        let contents = library.parent()?;
        let app = contents.parent()?;
        (items.file_name()? == "LoginItems"
            && library.file_name()? == "Library"
            && contents.file_name()? == "Contents"
            && app.extension()? == "app")
            .then(|| app.to_path_buf())
    }

    /// Whether launchd loaded a job from a plist inside an app bundle,
    /// which is what an `SMAppService` agent looks like from outside.
    pub fn is_bundled(plist: &str) -> bool {
        plist.contains(".app/Contents/Library/LaunchAgents/")
    }
}

/// The plist path in `launchctl print` output.
fn job_path(print: &str) -> Option<&str> {
    print
        .lines()
        .find_map(|l| l.trim().strip_prefix("path = "))
        .map(str::trim)
}

/// `SMAppService` registrations (the `tray` feature, on macOS), used when
/// this binary runs from the tray bundle that carries the daemon plist.
#[cfg(all(feature = "tray", target_os = "macos"))]
mod app {
    use std::path::PathBuf;

    use anyhow::Context;
    use smappservice_rs::{AppService, ServiceStatus, ServiceType};

    use super::bundle;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Reg {
        Off,
        On,
        NeedsApproval,
    }

    fn reg(s: ServiceStatus) -> Reg {
        match s {
            ServiceStatus::Enabled => Reg::On,
            ServiceStatus::RequiresApproval => Reg::NeedsApproval,
            ServiceStatus::NotRegistered | ServiceStatus::NotFound => Reg::Off,
        }
    }

    /// The tray bundle this binary runs from, if it carries the plist and
    /// macOS has `SMAppService` (13+; on 12 the plain plists are used).
    pub fn bundle() -> Option<PathBuf> {
        let v13 = objc2_foundation::NSOperatingSystemVersion {
            majorVersion: 13,
            minorVersion: 0,
            patchVersion: 0,
        };
        if !objc2_foundation::NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(v13) {
            return None;
        }
        let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
        bundle::daemon_plist(&exe)
            .filter(|p| p.exists())
            .and(bundle::app_of(&exe))
    }

    fn daemon() -> AppService {
        AppService::new(ServiceType::Agent {
            plist_name: bundle::DAEMON_PLIST,
        })
    }

    fn tray() -> AppService {
        AppService::new(ServiceType::MainApp)
    }

    /// (daemon, tray)
    pub fn status() -> (Reg, Reg) {
        (reg(daemon().status()), reg(tray().status()))
    }

    pub fn register(with_tray: bool) -> anyhow::Result<(Reg, Reg)> {
        let d = daemon();
        if d.status() != ServiceStatus::Enabled {
            d.register()
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("registering tern with macOS (SMAppService)")?;
        }
        if with_tray {
            let t = tray();
            if t.status() != ServiceStatus::Enabled {
                t.register()
                    .map_err(|e| anyhow::anyhow!("{e}"))
                    .context("registering the tern tray as a login item")?;
            }
        }
        Ok(status())
    }

    /// Unregister both; one that isn't registered is fine.
    pub fn unregister() -> anyhow::Result<()> {
        for (what, s) in [("tern", daemon()), ("the tern tray", tray())] {
            if s.status() != ServiceStatus::NotRegistered
                && let Err(e) = s.unregister()
            {
                anyhow::bail!("unregistering {what}: {e}");
            }
        }
        Ok(())
    }

    /// Load the daemon's job again after `launchctl bootout`.
    pub fn reload_daemon() -> anyhow::Result<()> {
        let d = daemon();
        let _ = d.unregister();
        d.register()
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("registering tern with macOS (SMAppService)")
    }

    pub fn open_login_items() {
        AppService::open_system_settings_login_items();
    }
}

/// Without the `tray` feature (or off macOS) nothing is ever registered
/// through `SMAppService`.
#[cfg(not(all(feature = "tray", target_os = "macos")))]
mod app {
    use std::path::PathBuf;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Reg {
        Off,
        On,
        NeedsApproval,
    }

    pub fn bundle() -> Option<PathBuf> {
        None
    }

    pub fn status() -> (Reg, Reg) {
        (Reg::Off, Reg::Off)
    }

    pub fn register(_with_tray: bool) -> anyhow::Result<(Reg, Reg)> {
        anyhow::bail!("this tern can't register login items")
    }

    pub fn unregister() -> anyhow::Result<()> {
        Ok(())
    }

    pub fn reload_daemon() -> anyhow::Result<()> {
        anyhow::bail!("this tern can't register login items")
    }

    pub fn open_login_items() {}
}

pub fn state() -> Option<State> {
    let kind = Kind::current()?;
    let path = kind.path().ok()?;
    let file = path.exists();
    if kind == Kind::Launchd {
        let out = launchctl()
            .args(["print", &launchd_target()])
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let loaded = out.is_some();
        let active = out
            .as_deref()
            .is_some_and(|o| o.lines().any(|l| l.trim() == "state = running"));
        let from_bundle = out
            .as_deref()
            .and_then(job_path)
            .is_some_and(bundle::is_bundled);
        let plain_tray = Kind::tray_path().is_ok_and(|p| p.exists());
        // Inside the app SMAppService says; outside it, launchd's job path.
        let (daemon_reg, tray_reg) = match app::bundle() {
            Some(_) => {
                let (d, t) = app::status();
                (Some(d), Some(t))
            }
            None => (None, None),
        };
        let app_registered = daemon_reg.is_some_and(|r| r != app::Reg::Off);
        if app_registered || (from_bundle && !file) {
            return Some(State {
                kind,
                method: Method::AppService,
                installed: true,
                loaded,
                active,
                at_login: daemon_reg.map_or(loaded, |r| r == app::Reg::On),
                needs_approval: daemon_reg == Some(app::Reg::NeedsApproval),
                tray_at_login: match tray_reg {
                    Some(r) => Some(r == app::Reg::On || plain_tray),
                    None => plain_tray.then_some(true),
                },
            });
        }
        let at_login = file
            && std::fs::read_to_string(&path)
                .is_ok_and(|p| p.contains("<key>RunAtLoad</key>\n\t<true/>"));
        return Some(State {
            kind,
            method: Method::File,
            installed: file,
            loaded,
            active,
            at_login,
            needs_approval: false,
            tray_at_login: Some(plain_tray || tray_reg == Some(app::Reg::On)),
        });
    }
    let quiet = |args: &[&str]| {
        systemctl()
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let enabled = quiet(&["--user", "is-enabled", "--quiet", UNIT]);
    let active = quiet(&["--user", "is-active", "--quiet", UNIT]);
    Some(State {
        kind,
        method: Method::File,
        installed: file,
        loaded: enabled || active,
        active,
        at_login: enabled,
        needs_approval: false,
        tray_at_login: Some(false),
    })
}

/// One line for `tern doctor` and `tern service status`.
pub fn describe(s: &State) -> String {
    let when = |s: &State| {
        let mut w = if s.needs_approval {
            "waiting for you to allow it in System Settings › General › Login Items".to_owned()
        } else if s.at_login {
            "starts at login".to_owned()
        } else {
            "not at login".to_owned()
        };
        if s.active {
            w.push_str(", running");
        }
        match s.tray_at_login {
            Some(true) => w.push_str("; menu-bar icon at login"),
            Some(false) if s.kind == Kind::Launchd => w.push_str("; no menu-bar icon at login"),
            _ => {}
        }
        w
    };
    match (s.kind, s.method, s.installed) {
        (_, _, false) if s.loaded => format!(
            "{} still has tern loaded, but its file is gone; it won't start at the next login",
            s.kind.name()
        ),
        (_, _, false) => "no service installed: tern runs only when started by hand or by the app \
                          (`tern service install` starts it at login)"
            .to_owned(),
        (Kind::Launchd, Method::AppService, true) => {
            format!(
                "macOS login item from tern.app (SMAppService) ({})",
                when(s)
            )
        }
        (Kind::Launchd, Method::File, true) => {
            format!("launchd LaunchAgent {LABEL} plist ({})", when(s))
        }
        (Kind::Systemd, _, true) => format!("systemd user unit {UNIT} ({})", when(s)),
    }
}

fn launchctl() -> Command {
    Command::new(std::env::var_os("TERN_LAUNCHCTL").unwrap_or_else(|| "launchctl".into()))
}

fn systemctl() -> Command {
    Command::new(std::env::var_os("TERN_SYSTEMCTL").unwrap_or_else(|| "systemctl".into()))
}

fn launchd_domain() -> String {
    format!("gui/{}", tern_core::home::current_uid().unwrap_or(0))
}

fn launchd_target() -> String {
    format!("{}/{LABEL}", launchd_domain())
}

fn run(mut cmd: Command) -> anyhow::Result<()> {
    let shown = format!("{cmd:?}");
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {shown}"))?;
    if !out.status.success() {
        bail!(
            "{shown} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Start tern through the service manager (loading it first if needed).
pub fn start(s: &State) -> anyhow::Result<()> {
    match s.kind {
        Kind::Launchd if s.method == Method::AppService => {
            if !s.loaded {
                if app::bundle().is_none() {
                    bail!(
                        "tern is registered by tern.app but isn't loaded; start it from the \
                         menu-bar icon or the app"
                    );
                }
                // Registering loads the job, and `RunAtLoad` starts it.
                return app::reload_daemon();
            }
            let mut c = launchctl();
            c.args(["kickstart", &launchd_target()]);
            run(c)
        }
        Kind::Launchd => {
            if !s.loaded {
                let mut c = launchctl();
                c.args(["bootstrap", &launchd_domain()])
                    .arg(Kind::Launchd.path()?);
                run(c)?;
            }
            let mut c = launchctl();
            c.args(["kickstart", &launchd_target()]);
            run(c)
        }
        Kind::Systemd => {
            let mut c = systemctl();
            c.args(["--user", "start", UNIT]);
            run(c)
        }
    }
}

/// Make the service manager stop tern and not bring it back: launchd
/// unloads the job (the plist stays, so the next login still starts it);
/// systemd stops the unit (it stays enabled).
pub fn stop(s: &State) -> anyhow::Result<()> {
    match s.kind {
        Kind::Launchd => {
            let mut c = launchctl();
            c.args(["bootout", &launchd_target()]);
            run(c)
        }
        Kind::Systemd => {
            let mut c = systemctl();
            c.args(["--user", "stop", UNIT]);
            run(c)
        }
    }
}

/// Whether a clean exit would be restarted anyway: an older plist written
/// with `KeepAlive = true`. Ours never does.
pub fn restarts_clean_exit(s: &State) -> bool {
    s.kind == Kind::Launchd
        && s.method == Method::File
        && s.loaded
        && Kind::Launchd
            .path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_none_or(|p| !p.contains("<key>SuccessfulExit</key>"))
}

pub struct Spec<'a> {
    pub exe: &'a Path,
    pub tern_home: &'a Path,
    pub logs: &'a Path,
    pub path_env: &'a str,
    pub at_login: bool,
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn plist(spec: &Spec<'_>) -> String {
    let p = |p: &Path| xml(&p.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>daemon</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>TERN_HOME</key>
		<string>{home}</string>
		<key>{SERVICE_ENV}</key>
		<string>launchd</string>
		<key>PATH</key>
		<string>{path}</string>
	</dict>
	<key>RunAtLoad</key>
	<{at_login}/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>StandardOutPath</key>
	<string>{out}</string>
	<key>StandardErrorPath</key>
	<string>{err}</string>
</dict>
</plist>
"#,
        exe = p(spec.exe),
        home = p(spec.tern_home),
        path = xml(spec.path_env),
        at_login = spec.at_login,
        out = p(&spec.logs.join("daemon.out.log")),
        err = p(&spec.logs.join("daemon.err.log")),
    )
}

/// The plain LaunchAgent for `tern tray`: started at login in the GUI
/// session, restarted after a crash but not after Quit or "Close this icon".
pub fn tray_plist(spec: &Spec<'_>) -> String {
    let p = |p: &Path| xml(&p.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{TRAY_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>tray</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>TERN_HOME</key>
		<string>{home}</string>
	</dict>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>StandardErrorPath</key>
	<string>{err}</string>
</dict>
</plist>
"#,
        exe = p(spec.exe),
        home = p(spec.tern_home),
        err = p(&spec.logs.join("tray.err.log")),
    )
}

/// The daemon's agent plist bundled in `tern Tray.app` for `SMAppService`
/// (`tern service bundle-plist`, used by scripts/package-macos-app.sh).
/// Nothing in it may depend on the user: no home, no log paths (the
/// daemon logs to `~/.tern/logs` itself) and a fixed `PATH`.
pub fn bundled_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>BundleProgram</key>
	<string>Contents/MacOS/{exe}</string>
	<key>ProgramArguments</key>
	<array>
		<string>tern</string>
		<string>daemon</string>
	</array>
	<key>AssociatedBundleIdentifiers</key>
	<array>
		<string>{app}</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>{SERVICE_ENV}</key>
		<string>launchd</string>
		<key>PATH</key>
		<string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
</dict>
</plist>
"#,
        exe = crate::tray::TRAY_EXE,
        app = bundle::DESKTOP_ID,
    )
}

/// A systemd quoted word: `%` is a specifier and `\`/`"` need escaping.
fn sd(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

pub fn unit(spec: &Spec<'_>) -> String {
    let p = |p: &Path| p.display().to_string();
    format!(
        "[Unit]
Description=tern personal agent
After=network-online.target

[Service]
Type=simple
ExecStart={exe} daemon
Environment={home} {service} {path}
Restart=on-failure
RestartSec=5
TimeoutStopSec=15
StandardOutput=append:{out}
StandardError=append:{err}

[Install]
WantedBy=default.target
",
        exe = sd(&p(spec.exe)),
        home = sd(&format!("TERN_HOME={}", p(spec.tern_home))),
        service = sd(&format!("{SERVICE_ENV}=systemd")),
        path = sd(&format!("PATH={}", spec.path_env)),
        // `append:` takes a bare path; systemd still expands specifiers.
        out = p(&spec.logs.join("daemon.out.log")).replace('%', "%%"),
        err = p(&spec.logs.join("daemon.err.log")).replace('%', "%%"),
    )
}

/// `tern service install [--no-login] [--tray]`. From the app bundle (and
/// at login) through `SMAppService`, otherwise with plain files.
pub fn install(home: &TernHome, at_login: bool, tray: bool) -> anyhow::Result<ExitCode> {
    let Some(kind) = Kind::current() else {
        bail!("tern service supports macOS (launchd) and Linux (systemd) only");
    };
    home.ensure_layout()?;
    if kind == Kind::Launchd
        && let Some(bundle) = app::bundle()
    {
        if at_login {
            return install_app(home, &bundle, tray);
        }
        // A plist and an app service with the same label would clash.
        app::unregister()?;
    }
    let exe = std::env::current_exe()
        .and_then(|e| e.canonicalize())
        .context("finding the tern binary")?;
    let path_env = std::env::var("PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_owned());
    let root = home.root().canonicalize()?;
    let logs = TernHome::new(&root).logs_dir();
    let spec = Spec {
        exe: &exe,
        tern_home: &root,
        logs: &logs,
        path_env: &path_env,
        at_login,
    };
    let path = kind.path()?;
    let before = state();
    let text = match kind {
        Kind::Launchd => plist(&spec),
        Kind::Systemd => unit(&spec),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let changed = std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str());
    std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;

    match kind {
        Kind::Launchd => {
            // launchd reads the plist when loading it: reload a changed one.
            // A job that's loaded and running keeps running until it next
            // exits, so this never stops tern.
            if before.is_some_and(|s| s.loaded && !s.active && changed) {
                let mut c = launchctl();
                c.args(["bootout", &launchd_target()]);
                let _ = run(c);
            }
            if at_login && !before.is_some_and(|s| s.loaded && !changed) {
                let mut c = launchctl();
                c.args(["bootstrap", &launchd_domain()]).arg(&path);
                // Already loaded (and running) is fine.
                if let Err(e) = run(c)
                    && !before.is_some_and(|s| s.active)
                {
                    return Err(e);
                }
            }
        }
        Kind::Systemd => {
            let mut c = systemctl();
            c.args(["--user", "daemon-reload"]);
            run(c)?;
            let mut c = systemctl();
            c.args(["--user", if at_login { "enable" } else { "disable" }, UNIT]);
            run(c)?;
        }
    }
    println!("Installed {}.", path.display());
    if at_login {
        println!(
            "tern will start when you log in. Logs go to {}.",
            logs.display()
        );
    } else {
        println!("tern won't start at login; `tern start` starts it through the service.");
    }
    if tray {
        match kind {
            Kind::Launchd => install_tray_plist(&spec)?,
            Kind::Systemd => println!("The menu-bar icon isn't available on Linux yet."),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `ai.tern.tray`: loading it starts the tray now (a second one leaves).
fn install_tray_plist(spec: &Spec<'_>) -> anyhow::Result<()> {
    let path = Kind::tray_path()?;
    let text = tray_plist(spec);
    let changed = std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str());
    std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    let target = format!("{}/{TRAY_LABEL}", launchd_domain());
    let loaded = launchctl()
        .args(["print", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if loaded && changed {
        let mut c = launchctl();
        c.args(["bootout", &target]);
        let _ = run(c);
    }
    if !loaded || changed {
        let mut c = launchctl();
        c.args(["bootstrap", &launchd_domain()]).arg(&path);
        run(c)?;
    }
    println!("The menu-bar icon starts at login too.");
    Ok(())
}

/// Register through `SMAppService` from the tray bundle at `bundle`.
fn install_app(home: &TernHome, bundle: &Path, tray: bool) -> anyhow::Result<ExitCode> {
    // The plain plists have the same labels: leave one way of starting.
    let before = state();
    let plain = Kind::Launchd.path()?;
    if plain.exists() {
        if before.is_some_and(|s| s.method == Method::File && s.loaded && !s.active) {
            let mut c = launchctl();
            c.args(["bootout", &launchd_target()]);
            let _ = run(c);
        }
        std::fs::remove_file(&plain).with_context(|| format!("removing {}", plain.display()))?;
    }
    let plain_tray = Kind::tray_path()?;
    if tray && plain_tray.exists() {
        // A tray it started keeps running until logout or Quit.
        std::fs::remove_file(&plain_tray)
            .with_context(|| format!("removing {}", plain_tray.display()))?;
    }
    let (daemon, tray_reg) = app::register(tray)?;
    println!("Registered tern with macOS from {}.", bundle.display());
    if daemon == app::Reg::NeedsApproval || (tray && tray_reg == app::Reg::NeedsApproval) {
        println!(
            "macOS needs your OK first: in System Settings › General › Login Items, turn on tern."
        );
        app::open_login_items();
    } else {
        println!(
            "tern will start when you log in. Logs go to {}.",
            home.logs_dir().display()
        );
    }
    if tray {
        if !crate::tray::running(home) {
            // A login item isn't started by registering it.
            let _ = Command::new("open")
                .arg("-g")
                .arg(bundle)
                .stdin(Stdio::null())
                .status();
        }
        println!("The menu-bar icon starts at login too.");
    }
    Ok(ExitCode::SUCCESS)
}

pub fn uninstall(home: &TernHome) -> anyhow::Result<ExitCode> {
    let Some(s) = state() else {
        bail!("tern service supports macOS (launchd) and Linux (systemd) only");
    };
    if s.method == Method::AppService {
        return uninstall_app(home, &s);
    }
    if app::bundle().is_some() {
        // A tray login item left from an earlier app-service install.
        app::unregister()?;
    }
    remove_tray_plist()?;
    let path = s.kind.path()?;
    match s.kind {
        Kind::Launchd => {
            // A running job is left alone (unticking "start at login" doesn't
            // stop tern); launchd forgets it at logout.
            if s.loaded && !s.active {
                let mut c = launchctl();
                c.args(["bootout", &launchd_target()]);
                let _ = run(c);
            }
        }
        Kind::Systemd => {
            if s.installed {
                let mut c = systemctl();
                c.args(["--user", "disable", UNIT]);
                let _ = run(c);
            }
        }
    }
    if s.installed {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        if s.kind == Kind::Systemd {
            let mut c = systemctl();
            c.args(["--user", "daemon-reload"]);
            let _ = run(c);
        }
        println!("Removed {}. tern won't start at login.", path.display());
    } else {
        println!("No tern service was installed.");
    }
    if s.active {
        println!("tern is still running; `tern stop` stops it.");
    }
    Ok(ExitCode::SUCCESS)
}

/// The tray's plain LaunchAgent, if any. A tray it started keeps running
/// (launchd forgets the job at logout).
fn remove_tray_plist() -> anyhow::Result<()> {
    if Kind::current() != Some(Kind::Launchd) {
        return Ok(());
    }
    let path = Kind::tray_path()?;
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

fn uninstall_app(home: &TernHome, s: &State) -> anyhow::Result<ExitCode> {
    if app::bundle().is_none() {
        bail!(
            "tern was registered by tern.app: untick Start at login in the app or the menu-bar \
             icon, or turn tern off in System Settings › General › Login Items"
        );
    }
    let pid = crate::lifecycle::running_pid(home);
    app::unregister()?;
    remove_tray_plist()?;
    println!("Unregistered tern from macOS. tern won't start at login.");
    // Unregistering an agent stops it, but unticking "start at login"
    // shouldn't turn tern off: start it again on its own. Runs resume.
    if s.active
        && let Some(pid) = pid
    {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while crate::lifecycle::running_pid(home) == Some(pid) && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if crate::lifecycle::running_pid(home).is_none() {
            crate::lifecycle::spawn_detached(home)?;
            println!("tern keeps running; `tern stop` stops it.");
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn status(home: &TernHome, json: bool) -> anyhow::Result<ExitCode> {
    let Some(s) = state() else {
        bail!("tern service supports macOS (launchd) and Linux (systemd) only");
    };
    if json {
        let v = serde_json::json!({
            "kind": s.kind.name(),
            "method": s.installed.then_some(match s.method {
                Method::File => "file",
                Method::AppService => "app_service",
            }),
            "installed": s.installed,
            "loaded": s.loaded,
            "active": s.active,
            "at_login": s.at_login,
            "needs_approval": s.needs_approval,
            "tray_at_login": s.tray_at_login,
            "running_pid": crate::lifecycle::running_pid(home),
            "describe": describe(&s),
        });
        println!("{v}");
        return Ok(ExitCode::SUCCESS);
    }
    println!("Service: {}", describe(&s));
    if s.installed && s.method == Method::File {
        println!("  file:      {}", s.kind.path()?.display());
    }
    println!("  installed: {}", yes(s.installed));
    println!("  loaded:    {}", yes(s.loaded));
    match crate::lifecycle::running_pid(home) {
        Some(pid) => println!("  running:   yes (pid {pid})"),
        None => println!("  running:   no"),
    }
    Ok(ExitCode::SUCCESS)
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (PathBuf, PathBuf, PathBuf) {
        (
            PathBuf::from("/opt/tern & co/tern"),
            PathBuf::from("/Users/a/.tern"),
            PathBuf::from("/Users/a/.tern/logs"),
        )
    }

    #[test]
    fn plist_restarts_crashes_only_and_escapes() {
        let (exe, home, logs) = paths();
        let text = plist(&Spec {
            exe: &exe,
            tern_home: &home,
            logs: &logs,
            path_env: "/usr/bin:/bin",
            at_login: true,
        });
        assert!(text.contains("<string>/opt/tern &amp; co/tern</string>"));
        assert!(text.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(text.contains("/Users/a/.tern/logs/daemon.err.log"));
        assert!(text.contains("<key>TERN_SERVICE</key>\n\t\t<string>launchd</string>"));
        let off = plist(&Spec {
            exe: &exe,
            tern_home: &home,
            logs: &logs,
            path_env: "/usr/bin",
            at_login: false,
        });
        assert!(off.contains("<key>RunAtLoad</key>\n\t<false/>"));
    }

    #[test]
    fn tray_plist_runs_in_the_gui_and_stays_quit() {
        let (exe, home, logs) = paths();
        let text = tray_plist(&Spec {
            exe: &exe,
            tern_home: &home,
            logs: &logs,
            path_env: "/usr/bin",
            at_login: true,
        });
        assert!(text.contains("<string>ai.tern.tray</string>"));
        assert!(
            text.contains("<string>/opt/tern &amp; co/tern</string>\n\t\t<string>tray</string>")
        );
        assert!(text.contains("<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>"));
        assert!(text.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(text.contains("/Users/a/.tern/logs/tray.err.log"));
    }

    #[test]
    fn bundled_plist_runs_the_tray_bundles_binary() {
        let text = bundled_plist();
        assert!(text.contains("<key>Label</key>\n\t<string>ai.tern.daemon</string>"));
        assert!(
            text.contains("<key>BundleProgram</key>\n\t<string>Contents/MacOS/tern-tray</string>")
        );
        assert!(text.contains("<string>tern</string>\n\t\t<string>daemon</string>"));
        assert!(text.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(text.contains("<string>dev.tern.ternDesktop</string>"));
        // Nothing user-specific: the same file serves every user.
        assert!(!text.contains("/Users/"));
        assert!(!text.contains("TERN_HOME"));
    }

    #[test]
    fn bundle_paths() {
        use std::path::Path;
        let exe = Path::new(
            "/Applications/tern.app/Contents/Library/LoginItems/tern Tray.app/Contents/MacOS/tern-tray",
        );
        assert_eq!(
            bundle::app_of(exe).as_deref(),
            Some(Path::new(
                "/Applications/tern.app/Contents/Library/LoginItems/tern Tray.app"
            ))
        );
        assert_eq!(
            bundle::daemon_plist(exe).as_deref(),
            Some(Path::new(
                "/Applications/tern.app/Contents/Library/LoginItems/tern Tray.app/Contents/Library/LaunchAgents/ai.tern.daemon.plist"
            ))
        );
        assert_eq!(
            bundle::host_app(exe).as_deref(),
            Some(Path::new("/Applications/tern.app"))
        );
        // A CLI install isn't in a bundle.
        assert_eq!(bundle::app_of(Path::new("/Users/a/.cargo/bin/tern")), None);
        assert_eq!(
            bundle::host_app(Path::new("/Users/a/.cargo/bin/tern")),
            None
        );
        // A bundle that isn't inside tern.app has no host.
        assert_eq!(
            bundle::host_app(Path::new("/tmp/tern Tray.app/Contents/MacOS/tern-tray")),
            None
        );
    }

    #[test]
    fn launchctl_print_job_path() {
        let out = "gui/501/ai.tern.daemon = {\n\tactive count = 1\n\tpath = /Applications/tern.app/Contents/Library/LoginItems/tern Tray.app/Contents/Library/LaunchAgents/ai.tern.daemon.plist\n\ttype = LaunchAgent\n";
        let path = job_path(out).unwrap();
        assert!(bundle::is_bundled(path), "{path}");
        let plain = "x = {\n\tpath = /Users/a/Library/LaunchAgents/ai.tern.daemon.plist\n}";
        assert!(!bundle::is_bundled(job_path(plain).unwrap()));
        assert_eq!(job_path("nothing here"), None);
    }

    #[test]
    fn describe_names_the_method() {
        let mut s = State {
            kind: Kind::Launchd,
            method: Method::AppService,
            installed: true,
            loaded: true,
            active: true,
            at_login: true,
            needs_approval: false,
            tray_at_login: Some(true),
        };
        assert_eq!(
            describe(&s),
            "macOS login item from tern.app (SMAppService) (starts at login, running; menu-bar icon at login)"
        );
        s.needs_approval = true;
        s.active = false;
        s.tray_at_login = None;
        assert!(describe(&s).contains("(waiting for you to allow it in System Settings"));
        s.method = Method::File;
        s.needs_approval = false;
        s.tray_at_login = Some(false);
        assert_eq!(
            describe(&s),
            "launchd LaunchAgent ai.tern.daemon plist (starts at login; no menu-bar icon at login)"
        );
    }

    #[test]
    fn unit_restarts_on_failure_and_quotes() {
        let (exe, home, logs) = paths();
        let text = unit(&Spec {
            exe: &exe,
            tern_home: &home,
            logs: &logs,
            path_env: "/usr/bin:/odd%dir",
            at_login: true,
        });
        assert!(text.contains("ExecStart=\"/opt/tern & co/tern\" daemon\n"));
        assert!(text.contains("Restart=on-failure\n"));
        assert!(text.contains("\"PATH=/usr/bin:/odd%%dir\""));
        assert!(text.contains("\"TERN_SERVICE=systemd\""));
        assert!(text.contains("StandardError=append:/Users/a/.tern/logs/daemon.err.log"));
        assert!(text.contains("WantedBy=default.target"));
    }
}
