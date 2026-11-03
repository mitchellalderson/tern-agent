//! `tern connectors [list | test <name>]`.

use std::process::ExitCode;

use tern_core::{Secrets, TernHome};
use tern_lua::{EXTENSION, Host, load_file};
use tern_store::Store;

/// A host for loading connectors to inspect them: real secrets (so missing
/// ones can be reported) but a scratch store, so nothing is written.
fn host(home: &TernHome) -> anyhow::Result<Host> {
    let secrets = Secrets::load(&home.secrets_path())?;
    Ok(Host::new(Store::open_in_memory()?, secrets)?)
}

fn luau_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == EXTENSION))
        .collect();
    files.sort();
    files
}

pub fn list(home: &TernHome) -> anyhow::Result<ExitCode> {
    let host = host(home)?;
    let secrets = Secrets::load(&home.secrets_path())?;
    for (label, dir) in [
        ("active", home.connectors_dir()),
        ("drafts", home.drafts_dir()),
    ] {
        let files = luau_files(&dir);
        println!("{label} ({}):", dir.display());
        if files.is_empty() {
            println!("  (none)");
        }
        for path in files {
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match load_file(&host, &path) {
                Err(e) => println!("  ✗ {file}: {e}"),
                Ok(vm) => {
                    let s = &vm.spec;
                    println!("  ✓ {} — {}", s.name, s.description);
                    println!("      {}", s.capability_summary());
                    let missing: Vec<&str> = s
                        .capabilities
                        .secrets
                        .iter()
                        .filter(|k| secrets.get(k).is_none())
                        .map(String::as_str)
                        .collect();
                    if !missing.is_empty() {
                        println!("      ! secrets not set: {}", missing.join(", "));
                    }
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Load and validate a connector (active first, then draft) and print its tools.
pub fn test(home: &TernHome, name: &str) -> anyhow::Result<ExitCode> {
    let host = host(home)?;
    let candidates = [
        home.connectors_dir().join(format!("{name}.{EXTENSION}")),
        home.drafts_dir().join(format!("{name}.{EXTENSION}")),
    ];
    let Some(path) = candidates.iter().find(|p| p.exists()) else {
        eprintln!(
            "no connector named {name:?} in {} or its drafts/",
            home.connectors_dir().display()
        );
        return Ok(ExitCode::FAILURE);
    };
    match load_file(&host, path) {
        Err(e) => {
            eprintln!("✗ {}: {e}", path.display());
            Ok(ExitCode::FAILURE)
        }
        Ok(vm) => {
            println!("✓ {} is valid ({})", vm.spec.name, path.display());
            println!("  {}", vm.spec.capability_summary());
            for t in &vm.spec.tools {
                let flags = [
                    (t.idempotent, "idempotent"),
                    (t.requires_approval, "requires approval"),
                ]
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, f)| *f)
                .collect::<Vec<_>>()
                .join(", ");
                println!(
                    "  - {}: {} {}",
                    tern_lua::tool_name(&vm.spec.name, &t.name),
                    t.description,
                    if flags.is_empty() {
                        String::new()
                    } else {
                        format!("({flags})")
                    }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
