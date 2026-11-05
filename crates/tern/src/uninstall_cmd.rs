//! `tern uninstall`: stop tern and take it out of the login items, then say
//! what is still on the machine. Your data is kept unless `--purge` is given
//! (which is `tern reset`, with its confirmation).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tern_core::TernHome;

use crate::{lifecycle, reset_cmd, service};

pub fn run(home: &TernHome, purge: bool, yes: bool) -> anyhow::Result<ExitCode> {
    if lifecycle::running_pid(home).is_some() {
        lifecycle::stop(home)?;
    }
    if service::state().is_some_and(|s| s.installed) {
        service::uninstall(home)?;
    }
    let mut purged = false;
    if purge {
        if reset_cmd::run(home, false, yes)? != ExitCode::SUCCESS {
            println!("tern is stopped and won't start at login; your data was kept.");
            return Ok(ExitCode::FAILURE);
        }
        purged = true;
    }
    let exe = std::env::current_exe().ok();
    let home_dir = tern_core::home::user_home();
    println!("\ntern is stopped and won't start at login.");
    let rest = leftovers(
        exe.as_deref(),
        home.root(),
        purged,
        cfg!(target_os = "macos"),
        home_dir.as_deref(),
    );
    if !rest.is_empty() {
        println!("Still on this computer (remove by hand if you want them gone):");
        for line in rest {
            println!("  - {line}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// What `tern uninstall` leaves for the user to remove, one line each.
pub(crate) fn leftovers(
    exe: Option<&Path>,
    data: &Path,
    purged: bool,
    macos: bool,
    user_home: Option<&Path>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(exe) = exe {
        out.push(format!("the program: {}", exe.display()));
    }
    if !purged {
        out.push(format!(
            "your data: {} (conversations, memory, config, API keys in secrets.toml); \
             `tern uninstall --purge` deletes it, or copy it first to keep a backup",
            data.display()
        ));
    }
    let settings: Option<PathBuf> = user_home.map(|h| {
        if macos {
            h.join("Library/Application Support/tern")
        } else {
            h.join(".config/tern")
        }
    });
    if let Some(s) = settings {
        out.push(format!("the desktop app's own settings: {}", s.display()));
    }
    if macos {
        out.push(
            "the desktop app (tern.app in Applications) and, in Keychain Access, \
             any \"tern\" entries for paired servers"
                .to_owned(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::leftovers;
    use std::path::Path;

    #[test]
    fn kept_data_is_named_with_the_purge_hint() {
        let l = leftovers(
            Some(Path::new("/usr/local/bin/tern")),
            Path::new("/home/a/.tern"),
            false,
            false,
            Some(Path::new("/home/a")),
        );
        let all = l.join("\n");
        assert!(all.contains("/usr/local/bin/tern"), "{all}");
        assert!(
            all.contains("/home/a/.tern") && all.contains("--purge"),
            "{all}"
        );
        assert!(all.contains("/home/a/.config/tern"), "{all}");
        assert!(!all.contains("Keychain"), "{all}");
    }

    #[test]
    fn purged_data_is_not_listed_and_macos_adds_the_app() {
        let l = leftovers(
            None,
            Path::new("/Users/a/.tern"),
            true,
            true,
            Some(Path::new("/Users/a")),
        );
        let all = l.join("\n");
        assert!(!all.contains("your data"), "{all}");
        assert!(all.contains("Library/Application Support/tern"), "{all}");
        assert!(all.contains("Keychain"), "{all}");
    }
}
