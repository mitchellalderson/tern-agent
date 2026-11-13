//! `tern reset`: delete the data directory (conversations, runs, memory,
//! skills, connectors, logs, and, unless `--keep-config`, config and
//! secrets), stopping the daemon first.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, bail};
use tern_core::TernHome;

use crate::{lifecycle, service};

/// Kept by `--keep-config`, relative to the data directory.
const CONFIG_FILES: [&str; 2] = ["config.toml", "secrets.toml"];

pub fn run(home: &TernHome, keep_config: bool, yes: bool) -> anyhow::Result<ExitCode> {
    let root = home.root();
    if !root.exists() {
        println!("Nothing to clear: {} doesn't exist.", root.display());
        return Ok(ExitCode::SUCCESS);
    }
    refuse_unsafe_root(root)?;

    let what = if keep_config {
        "everything except config.toml and secrets.toml"
    } else {
        "everything, including your config and API keys"
    };
    println!(
        "This deletes {what} in {} ({}).",
        root.display(),
        human_size(dir_size(root))
    );
    if !yes && !confirm()? {
        println!("Nothing was deleted.");
        return Ok(ExitCode::FAILURE);
    }

    if lifecycle::running_pid(home).is_some() {
        let _ = lifecycle::stop(home)?;
    }

    // A long data directory keeps its socket outside it.
    let bind = home.socket_bind_path();
    if bind != home.socket_path() {
        let _ = std::fs::remove_file(&bind);
    }

    if keep_config {
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            if CONFIG_FILES.iter().any(|f| entry.file_name() == *f) {
                continue;
            }
            remove(&entry.path())?;
        }
    } else {
        std::fs::remove_dir_all(root).with_context(|| format!("deleting {}", root.display()))?;
    }
    println!("Cleared {}.", root.display());

    if service::state().is_some_and(|s| s.installed) {
        println!(
            "The login service is still installed, so tern starts again at next login; \
             `tern service uninstall` removes it."
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Never delete a home directory or `/` because `$TERN_HOME` pointed there.
fn refuse_unsafe_root(root: &Path) -> anyhow::Result<()> {
    let root = root.canonicalize()?;
    let user_home = tern_core::home::user_home().and_then(|h| h.canonicalize().ok());
    if root.parent().is_none() || Some(&root) == user_home.as_ref() {
        bail!(
            "refusing to delete {}: TERN_HOME points at a directory that isn't tern's",
            root.display()
        );
    }
    Ok(())
}

fn confirm() -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("not deleting without confirmation; pass --yes to skip the prompt");
    }
    print!("Type \"reset\" to continue: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim() == "reset")
}

fn remove(path: &Path) -> anyhow::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .with_context(|| format!("deleting {}", path.display()))
}

fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| dir_size(&e.path())).sum())
        .unwrap_or(0)
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}
