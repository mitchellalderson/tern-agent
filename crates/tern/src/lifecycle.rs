//! `tern stop` and `tern start` (personal-agent-tooling-spec §11.5a, T8a).
//!
//! Stopping always goes through tern first (`POST /v1/shutdown`, a clean
//! exit the service manager doesn't restart). Only a daemon that doesn't
//! exit within 10 s is stopped by force: the service manager lets go of it
//! first (so it isn't brought back), then SIGTERM, 5 s, SIGKILL, and only
//! for a pid that is a `tern` process of this user.

use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use tern_core::TernHome;

use crate::daemon::api_lifecycle::ShutdownReply;
use crate::service;

/// Test switch: milliseconds to use for both of `tern stop`'s waits (10 s
/// for a clean exit, 5 s after SIGTERM).
const TEST_WAIT_ENV: &str = "TERN_TEST_STOP_WAIT_MS";

/// The running daemon's pid: its PID file is locked while it runs (M5).
pub fn running_pid(home: &TernHome) -> Option<u32> {
    let path = home.pid_path();
    let file = std::fs::File::open(&path).ok()?;
    if file.try_lock().is_ok() {
        return None;
    }
    std::fs::read_to_string(&path).ok()?.trim().parse().ok()
}

fn waits() -> (Duration, Duration) {
    match std::env::var(TEST_WAIT_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
    {
        Some(ms) => (Duration::from_millis(ms), Duration::from_millis(ms)),
        None => (Duration::from_secs(10), Duration::from_secs(5)),
    }
}

pub(crate) fn socket_client(home: &TernHome) -> Option<reqwest::Client> {
    let socket = home.socket_connect_path().ok()?;
    if !socket.exists() {
        return None;
    }
    reqwest::Client::builder()
        .unix_socket(socket)
        .timeout(Duration::from_secs(5))
        .build()
        .ok()
}

pub(crate) fn block_on<T>(fut: impl std::future::Future<Output = T>) -> anyhow::Result<T> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(fut))
}

fn ask_to_stop(home: &TernHome) -> anyhow::Result<Option<ShutdownReply>> {
    let Some(http) = socket_client(home) else {
        return Ok(None);
    };
    block_on(async move {
        let resp = http.post("http://tern/v1/shutdown").send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<ShutdownReply>().await.ok()
    })
}

fn healthy(home: &TernHome) -> bool {
    let Some(http) = socket_client(home) else {
        return false;
    };
    block_on(async move {
        http.get("http://tern/v1/health")
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    })
    .unwrap_or(false)
}

/// The version the running daemon reports on `/v1/health`, if it answers.
pub(crate) fn running_version(home: &TernHome) -> Option<String> {
    let http = socket_client(home)?;
    block_on(async move {
        let resp = http.get("http://tern/v1/health").send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let j = resp.json::<serde_json::Value>().await.ok()?;
        j.get("version")?.as_str().map(str::to_owned)
    })
    .ok()
    .flatten()
}

/// What to say when the daemon that's running isn't this binary's version.
/// `None` when they match.
pub(crate) fn skew_note(running: &str, installed: &str) -> Option<String> {
    (running != installed).then(|| {
        format!(
            "The running daemon is v{running} but this `tern` is v{installed}. \
             Restart it to use the new version: `tern stop && tern start`."
        )
    })
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    i32::try_from(pid)
        .is_ok_and(|p| nix::sys::signal::kill(nix::unistd::Pid::from_raw(p), None).is_ok())
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    false
}

/// Wait until the daemon with `pid` has let go of its lock and exited.
fn wait_gone(home: &TernHome, pid: u32, limit: Duration) -> bool {
    let end = Instant::now() + limit;
    loop {
        if running_pid(home) != Some(pid) {
            // The lock goes with the process; give the exit a moment to
            // finish (a zombie still answers `kill 0`, so don't insist).
            let settle = Instant::now() + Duration::from_secs(1);
            while alive(pid) && Instant::now() < settle {
                std::thread::sleep(Duration::from_millis(50));
            }
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Refuse to signal anything but a `tern` process owned by this user.
fn verify_ours(pid: u32) -> anyhow::Result<()> {
    let out = Command::new("ps")
        .args(["-o", "uid=", "-o", "comm=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
        .context("running ps")?;
    let line = String::from_utf8_lossy(&out.stdout);
    let mut words = line.split_whitespace();
    let uid = words.next().and_then(|u| u.parse::<u32>().ok());
    let comm = words.collect::<Vec<_>>().join(" ");
    let name = std::path::Path::new(&comm)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if uid.is_none() || uid != tern_core::home::current_uid() || !name.starts_with("tern") {
        bail!(
            "pid {pid} from the PID file isn't a tern process of yours ({}), so it wasn't touched",
            if comm.is_empty() {
                "no such process"
            } else {
                comm.as_str()
            }
        );
    }
    Ok(())
}

#[cfg(unix)]
fn signal(pid: u32, sig: nix::sys::signal::Signal) {
    if let Ok(p) = i32::try_from(pid) {
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(p), sig);
    }
}

pub fn stop(home: &TernHome) -> anyhow::Result<ExitCode> {
    let svc = service::state();
    let Some(pid) = running_pid(home) else {
        println!("tern isn't running.");
        return Ok(ExitCode::SUCCESS);
    };
    let (grace, term_wait) = waits();

    // An old plist that restarts even a clean exit: let launchd go first.
    if let Some(s) = svc.filter(service::restarts_clean_exit) {
        let _ = service::stop(&s);
    }

    match ask_to_stop(home)? {
        Some(r) if !r.runs.is_empty() => {
            let names: Vec<String> = r.runs.iter().map(|x| format!("\"{}\"", x.title)).collect();
            println!(
                "Stopping tern. {} in progress ({}) will pick up where {} left off next time tern starts.",
                if r.runs.len() == 1 {
                    "1 task is".to_owned()
                } else {
                    format!("{} tasks are", r.runs.len())
                },
                names.join(", "),
                if r.runs.len() == 1 { "it" } else { "they" },
            );
        }
        Some(_) => println!("Stopping tern…"),
        None => println!("Stopping tern (it isn't answering)…"),
    }
    if wait_gone(home, pid, grace) {
        println!("tern is stopped.");
        return Ok(ExitCode::SUCCESS);
    }

    println!(
        "tern didn't stop within {} s, so it's being stopped by force.",
        grace.as_secs().max(1)
    );
    // The service manager would bring a killed tern back: make it let go.
    // The plist or unit stays, so tern still starts at the next login.
    if let Some(s) = svc.filter(|s| s.loaded) {
        if let Err(e) = service::stop(&s) {
            tracing::warn!(error = %e, "service manager didn't stop tern");
        }
        if wait_gone(home, pid, term_wait) {
            println!("tern is stopped.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    verify_ours(pid)?;
    #[cfg(unix)]
    {
        use nix::sys::signal::Signal;
        signal(pid, Signal::SIGTERM);
        if !wait_gone(home, pid, term_wait) {
            signal(pid, Signal::SIGKILL);
            if !wait_gone(home, pid, Duration::from_secs(5)) {
                bail!("tern (pid {pid}) is still running after SIGKILL");
            }
        }
    }
    println!(
        "tern was stopped by force. If this keeps happening, `tern report` writes a report to send."
    );
    Ok(ExitCode::SUCCESS)
}

pub fn start(home: &TernHome) -> anyhow::Result<ExitCode> {
    if let Some(pid) = running_pid(home) {
        println!("tern is already running (pid {pid}).");
        if let Some(note) =
            running_version(home).and_then(|v| skew_note(&v, env!("CARGO_PKG_VERSION")))
        {
            println!("{note}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    home.ensure_layout()?;
    let svc = service::state().filter(|s| s.installed);
    let err_log = home.logs_dir().join(match svc {
        Some(_) => "daemon.err.log",
        None => "daemon.log",
    });
    if let Some(s) = svc {
        println!("Starting tern through {}…", s.kind.name());
        service::start(&s)?;
    } else {
        println!("Starting tern…");
        spawn_detached(home)?;
    }
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(pid) = running_pid(home)
            && healthy(home)
        {
            println!("tern is running (pid {pid}).");
            return Ok(ExitCode::SUCCESS);
        }
        if Instant::now() >= end {
            bail!(
                "tern didn't start within 15 s; see {} (or run `tern daemon` to see why)",
                err_log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `tern daemon` in its own process group, so it outlives this command and
/// the terminal's Ctrl-C; its output goes to `logs/daemon.log`.
pub fn spawn_detached(home: &TernHome) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("finding the tern binary")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.logs_dir().join("daemon.log"))?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .env("TERN_HOME", home.root())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    // Not waited for: it's meant to outlive us, and init reaps it.
    let child = cmd.spawn().context("starting tern daemon")?;
    drop(child);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::skew_note;

    #[test]
    fn same_version_is_quiet() {
        assert_eq!(skew_note("0.1.0", "0.1.0"), None);
    }

    #[test]
    fn different_version_says_how_to_restart() {
        let n = skew_note("0.1.0", "0.1.1").unwrap();
        assert!(n.contains("v0.1.0") && n.contains("v0.1.1"), "{n}");
        assert!(n.contains("tern stop && tern start"), "{n}");
    }
}
