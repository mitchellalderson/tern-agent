//! `tern grants`: the standing "always" grants approvals have recorded
//! (`git *` for shell, `*` for a tool, `host:example.com` for reads from a
//! site in a tainted run), and removing them.

use std::process::ExitCode;

use anyhow::Context;
use tern_core::TernHome;
use tern_store::Store;

use crate::cli::GrantsCommand;

pub fn run(home: &TernHome, command: Option<GrantsCommand>) -> anyhow::Result<ExitCode> {
    let path = home.db_path();
    if !path.exists() {
        println!("No grants yet.");
        return Ok(ExitCode::SUCCESS);
    }
    let store = Store::open(&path).with_context(|| format!("opening {}", path.display()))?;
    match command.unwrap_or(GrantsCommand::List) {
        GrantsCommand::List => {
            let grants = store.allowlist_all()?;
            if grants.is_empty() {
                println!("No grants yet.");
            }
            for (tool, pattern) in grants {
                println!("{tool:<24} {}", describe(&pattern));
            }
        }
        GrantsCommand::Remove { tool, pattern } => {
            if store.remove_allowlist(&tool, &pattern)? {
                println!("Removed {tool} {pattern}.");
            } else {
                println!("No grant {tool} {pattern}; `tern grants` lists them.");
                return Ok(ExitCode::FAILURE);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn describe(pattern: &str) -> String {
    if let Some(key) = pattern.strip_prefix("key:") {
        return match key.strip_prefix("python script ") {
            Some(_) => format!(
                "{pattern}   (this exact python script version with these arguments only; `tern grants remove shell '{pattern}'` revokes it)"
            ),
            None => format!("{pattern}   (one exact target, as shown when you approved it)"),
        };
    }
    match pattern.strip_prefix("host:") {
        Some(h) => format!("{pattern}   (reads from {h}, even after untrusted content)"),
        None if pattern == "*" => "*   (every call)".into(),
        None => pattern.to_owned(),
    }
}
