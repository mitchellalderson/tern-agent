//! `tern tray`: the menu-bar icon (personal-agent-tooling-spec §11, T8b, T8c).
//!
//! A client of the daemon like the TUI and the app, in its own process:
//! it reads `/v1/status`, sleeps on `/v1/events` and only asks again when
//! something happens, and controls tern by running this same binary's
//! `start`, `stop` and `service` commands, so Quit takes exactly the path
//! (and the escalation) `tern stop` does. It supervises nothing.
//!
//! - `model`: what the icon and menu show for a state; pure, tested.
//! - `worker`: the daemon side, on a background thread (macOS) or task
//!   (Linux).
//! - `macos`: AppKit, on the main thread (`tray-icon` + `muda`).
//! - `linux`: a StatusNotifierItem over D-Bus (`ksni`).

#[cfg(all(feature = "tray", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "tray", target_os = "macos"))]
mod macos;
// Pure and tested everywhere; only the real trays use it.
#[cfg_attr(
    not(all(feature = "tray", any(target_os = "macos", target_os = "linux"))),
    allow(dead_code)
)]
pub mod model;
#[cfg(all(feature = "tray", any(target_os = "macos", target_os = "linux")))]
mod worker;

use std::ffi::OsString;
use std::process::ExitCode;

use tern_core::TernHome;

/// The executable's name inside `tern Tray.app`. Started with no
/// arguments under that name (as Login Items and Finder do), `tern` runs
/// the tray.
pub const TRAY_EXE: &str = "tern-tray";

/// `tern-tray` with no arguments means `tern tray`.
pub fn rewrite_args(mut args: Vec<OsString>) -> Vec<OsString> {
    let as_tray = args.len() == 1
        && args.first().is_some_and(|a| {
            std::path::Path::new(a)
                .file_name()
                .is_some_and(|n| n == TRAY_EXE)
        });
    if as_tray {
        args.push("tray".into());
    }
    args
}

/// Whether a tray is running for `home` (it holds `tray.lock`).
pub fn running(home: &TernHome) -> bool {
    std::fs::File::open(lock_path(home)).is_ok_and(|f| f.try_lock().is_err())
}

pub fn lock_path(home: &TernHome) -> std::path::PathBuf {
    home.root().join("tray.lock")
}

#[cfg(all(feature = "tray", target_os = "macos"))]
pub fn run(home: &TernHome) -> anyhow::Result<ExitCode> {
    macos::run(home)
}

#[cfg(all(feature = "tray", target_os = "linux"))]
pub fn run(home: &TernHome) -> anyhow::Result<ExitCode> {
    linux::run(home)
}

#[cfg(not(all(feature = "tray", any(target_os = "macos", target_os = "linux"))))]
pub fn run(_home: &TernHome) -> anyhow::Result<ExitCode> {
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        anyhow::bail!("this tern was built without the `tray` feature");
    }
    anyhow::bail!("tern tray runs on macOS and Linux");
}

/// Whether a graphical session is available for the tray: an X11 or
/// Wayland display and a D-Bus session bus. Linux only; false elsewhere.
pub fn desktop_session() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let bus = env_set("DBUS_SESSION_BUS_ADDRESS")
        || std::env::var_os("XDG_RUNTIME_DIR")
            .is_some_and(|d| std::path::Path::new(&d).join("bus").exists());
    session_ok(env_set("DISPLAY"), env_set("WAYLAND_DISPLAY"), bus)
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

fn session_ok(display: bool, wayland: bool, bus: bool) -> bool {
    (display || wayland) && bus
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_desktop_session_needs_a_display_and_a_bus() {
        assert!(session_ok(true, false, true));
        assert!(session_ok(false, true, true));
        assert!(!session_ok(false, false, true));
        assert!(!session_ok(true, true, false));
    }

    #[test]
    fn tern_tray_alone_runs_the_tray() {
        assert_eq!(
            rewrite_args(args(&["/A/tern Tray.app/Contents/MacOS/tern-tray"])),
            args(&["/A/tern Tray.app/Contents/MacOS/tern-tray", "tray"])
        );
        // Anything else is an ordinary command line.
        assert_eq!(
            rewrite_args(args(&["tern-tray", "daemon"])),
            args(&["tern-tray", "daemon"])
        );
        assert_eq!(rewrite_args(args(&["tern"])), args(&["tern"]));
    }
}
