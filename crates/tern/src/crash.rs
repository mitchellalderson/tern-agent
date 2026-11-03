//! Crash reports: on a panic, write what happened to `logs/crash-<time>.txt`
//! and say where it is. (Native aborts, such as a C++ exception the Rust
//! side can't catch, never reach this hook; the unbuffered log is what
//! survives those.)

use std::path::{Path, PathBuf};

use tern_core::TernHome;

/// Env var that makes tern panic right after startup (tests the hook).
pub const TEST_PANIC_ENV: &str = "TERN_TEST_PANIC";

pub fn install(home: &TernHome) {
    let dir = home.logs_dir();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_owned();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = format!(
            "tern {} crashed\n\nwhen: {}\nos: {} {}\nthread: {thread}\nat: {location}\nmessage: {message}\n\nbacktrace:\n{backtrace}\n",
            env!("CARGO_PKG_VERSION"),
            chrono::Local::now().to_rfc3339(),
            std::env::consts::OS,
            std::env::consts::ARCH,
        );
        let saved = save(&dir, &report);
        tracing::error!(%location, %message, crash_file = ?saved, "panic");
        previous(info);
        match saved {
            Some(path) => eprintln!(
                "\ntern hit an internal error: {message}\nDetails were saved to {}.\nRun `tern report` to collect them (with recent logs) for a bug report.",
                path.display()
            ),
            None => eprintln!(
                "\ntern hit an internal error: {message}\nRun `tern report` to collect recent logs for a bug report."
            ),
        }
    }));
}

fn save(dir: &Path, report: &str) -> Option<PathBuf> {
    let dir = if dir.is_dir() {
        dir.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    let name = format!(
        "crash-{}.txt",
        chrono::Local::now().format("%Y%m%d-%H%M%S%.3f")
    );
    let path = dir.join(name);
    std::fs::write(&path, report).ok().map(|()| path)
}

/// The most recent crash files, newest first.
pub fn recent(dir: &Path, n: usize) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("crash-"))
        })
        .collect();
    files.sort();
    files.reverse();
    files.truncate(n);
    files
}
