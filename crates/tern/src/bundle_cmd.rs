//! `tern bundle`: signing keys and trust (tooling spec §6.5).

use std::process::ExitCode;

use tern_bundle::{Paths, PublicKey, SecretKey, TrustStore};
use tern_core::TernHome;

use crate::cli::BundleCommand;

pub fn run(home: &TernHome, command: Option<BundleCommand>) -> anyhow::Result<ExitCode> {
    let paths = Paths::new(home);
    match command.unwrap_or(BundleCommand::Trust {
        key: None,
        name: None,
    }) {
        BundleCommand::Keygen => keygen(&paths),
        BundleCommand::Trust { key: None, .. } => trusted(&paths),
        BundleCommand::Trust {
            key: Some(key),
            name,
        } => trust(&paths, &key, name.as_deref().unwrap_or_default()),
        BundleCommand::Untrust { key } => {
            let mut t = TrustStore::load(&paths.trusted())?;
            if t.remove(&key) {
                t.save(&paths.trusted())?;
                println!("No longer trusted: {key}");
                Ok(ExitCode::SUCCESS)
            } else {
                eprintln!("no trusted key matches {key:?}");
                Ok(ExitCode::FAILURE)
            }
        }
    }
}

fn keygen(paths: &Paths) -> anyhow::Result<ExitCode> {
    let key = SecretKey::generate()?;
    key.save(&paths.signing_key())?;
    std::fs::write(paths.signing_pub(), key.public.to_file())?;
    // Your own bundles install without the unknown-key warning.
    let mut t = TrustStore::load(&paths.trusted())?;
    t.add(&key.public, "you");
    t.save(&paths.trusted())?;
    println!("Made signing key {}.", key.public.id_hex());
    println!(
        "  secret: {} (keep it private)",
        paths.signing_key().display()
    );
    println!("  public: {}", paths.signing_pub().display());
    println!();
    println!("Share the public key so people can trust your bundles:");
    println!("  {}", key.public.to_base64());
    println!("They can check a bundle with minisign too: minisign -Vm bundle.toml -P <key>");
    Ok(ExitCode::SUCCESS)
}

fn read_key(key: &str) -> anyhow::Result<PublicKey> {
    let path = std::path::Path::new(key);
    let text = if !key.starts_with("RW") && path.is_file() {
        std::fs::read_to_string(path)?
    } else {
        key.to_owned()
    };
    Ok(PublicKey::parse(&text)?)
}

fn trust(paths: &Paths, key: &str, name: &str) -> anyhow::Result<ExitCode> {
    let key = read_key(key)?;
    let mut t = TrustStore::load(&paths.trusted())?;
    if t.add(&key, name) {
        t.save(&paths.trusted())?;
        println!(
            "Trusted {}{}.",
            key.id_hex(),
            if name.is_empty() {
                String::new()
            } else {
                format!(" ({name})")
            }
        );
    } else {
        println!("{} was already trusted.", key.id_hex());
    }
    Ok(ExitCode::SUCCESS)
}

fn trusted(paths: &Paths) -> anyhow::Result<ExitCode> {
    let t = TrustStore::load(&paths.trusted())?;
    if t.keys.is_empty() {
        println!("No trusted keys. Add one with `tern bundle trust <key> --name <whose>`.");
    }
    for k in &t.keys {
        let id = PublicKey::parse(&k.key)
            .map(|p| p.id_hex())
            .unwrap_or_else(|_| "?".into());
        let name = if k.name.is_empty() {
            "(no name)"
        } else {
            &k.name
        };
        println!("{id}  {name}  {}  added {}", k.key, k.added_at);
    }
    Ok(ExitCode::SUCCESS)
}
