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
//! `TERN_LAUNCHCTL` and `TERN_SYSTEMCTL` replace the service manager's
//! command, so tests never touch the real launchd or systemd.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, bail};
use tern_core::TernHome;

pub const LABEL: &str = "ai.tern.daemon";
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

/// What the service manager knows about tern.
#[derive(Debug, Clone, Copy)]
pub struct State {
    pub kind: Kind,
    /// The plist or unit file exists.
    pub installed: bool,
    /// launchd has the job loaded / systemd has the unit enabled or active.
    pub loaded: bool,
    /// The service manager's own process is running (not a hand-started
    /// daemon).
    pub active: bool,
    /// Starts at login.
    pub at_login: bool,
}

pub fn state() -> Option<State> {
    let kind = Kind::current()?;
    let path = kind.path().ok()?;
    let installed = path.exists();
    let (loaded, active, at_login) = match kind {
        Kind::Launchd => {
            let out = launchctl()
                .args(["print", &launchd_target()])
                .stderr(Stdio::null())
                .output()
                .ok();
            let loaded = out.as_ref().is_some_and(|o| o.status.success());
            let active = out.is_some_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|l| l.trim() == "state = running")
            });
            let at_login = installed
                && std::fs::read_to_string(&path)
                    .is_ok_and(|p| p.contains("<key>RunAtLoad</key>\n\t<true/>"));
            (loaded, active, at_login)
        }
        Kind::Systemd => {
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
            (enabled || active, active, enabled)
        }
    };
    Some(State {
        kind,
        installed,
        loaded,
        active,
        at_login,
    })
}

/// One line for `tern doctor` and `tern service status`.
pub fn describe(s: &State) -> String {
    match (s.kind, s.installed) {
        (_, false) if s.loaded => format!(
            "{} still has tern loaded, but its file is gone; it won't start at the next login",
            s.kind.name()
        ),
        (_, false) => "no service installed: tern runs only when started by hand or by the app \
                       (`tern service install` starts it at login)"
            .to_owned(),
        (Kind::Launchd, true) => format!(
            "launchd LaunchAgent {LABEL} ({}{})",
            if s.at_login {
                "starts at login"
            } else {
                "not at login"
            },
            if s.active { ", running" } else { "" }
        ),
        (Kind::Systemd, true) => format!(
            "systemd user unit {UNIT} ({}{})",
            if s.at_login {
                "starts at login"
            } else {
                "not at login"
            },
            if s.active { ", running" } else { "" }
        ),
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

pub fn install(home: &TernHome, at_login: bool) -> anyhow::Result<ExitCode> {
    let Some(kind) = Kind::current() else {
        bail!("tern service supports macOS (launchd) and Linux (systemd) only");
    };
    home.ensure_layout()?;
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
    Ok(ExitCode::SUCCESS)
}

pub fn uninstall() -> anyhow::Result<ExitCode> {
    let Some(s) = state() else {
        bail!("tern service supports macOS (launchd) and Linux (systemd) only");
    };
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

pub fn status(home: &TernHome) -> anyhow::Result<ExitCode> {
    let Some(s) = state() else {
        bail!("tern service supports macOS (launchd) and Linux (systemd) only");
    };
    println!("Service: {}", describe(&s));
    if s.installed {
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
