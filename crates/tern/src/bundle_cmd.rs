//! `tern bundle`: install, update, remove and list bundles; signing keys
//! and trust (tooling spec §6.5).

use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;

use anyhow::Context;
use tern_bundle::{InstallOptions, Installer, Paths, PublicKey, SecretKey, TrustStore};
use tern_core::TernHome;
use tern_store::Store;

use crate::cli::{BundleCommand, ImportCommand};

pub fn run(home: &TernHome, command: Option<BundleCommand>) -> anyhow::Result<ExitCode> {
    let paths = Paths::new(home);
    match command.unwrap_or(BundleCommand::List) {
        BundleCommand::List => list(home),
        BundleCommand::Install {
            source,
            sha256,
            agent,
            dry_run,
            yes,
            accept_new_key,
        } => install(
            home,
            &source,
            sha256.as_deref(),
            &InstallOptions {
                agent,
                accept_new_key,
            },
            dry_run,
            yes,
            false,
        ),
        BundleCommand::Update {
            name,
            sha256,
            dry_run,
            yes,
            accept_new_key,
        } => {
            let installer = Installer::new(home);
            let Some(b) = installer.installed(&name)? else {
                eprintln!("no bundle named {name} is installed (see `tern bundle list`)");
                return Ok(ExitCode::FAILURE);
            };
            if b.source.starts_with("openclaw:") {
                eprintln!("{name} was made by `tern import openclaw`; run that again instead");
                return Ok(ExitCode::FAILURE);
            }
            install(
                home,
                &b.source,
                sha256.as_deref(),
                &InstallOptions {
                    agent: None,
                    accept_new_key,
                },
                dry_run,
                yes,
                true,
            )
        }
        BundleCommand::Remove { name, yes } => remove(home, &name, yes),
        BundleCommand::Export {
            name,
            version,
            description,
            author,
            connectors,
            skills,
            routines,
            agents,
            all,
            agent,
            output,
            dir,
            unsigned,
        } => {
            let req = tern_bundle::export::Request {
                header: tern_bundle::manifest::Header {
                    name: name.clone(),
                    version: version.clone(),
                    description,
                    author,
                    key: None,
                },
                connectors,
                skills,
                routines,
                agents,
                all,
                agent,
            };
            export(home, &paths, &req, output, dir, unsigned)
        }
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

fn open_store(home: &TernHome) -> anyhow::Result<Store> {
    home.ensure_layout()?;
    Store::open(&home.db_path()).context("opening tern's database")
}

/// Ask on the terminal; `false` without one.
pub(crate) fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        eprintln!("{question} Not asking without a terminal: pass --yes to approve.");
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn block_on<F: std::future::Future>(f: F) -> anyhow::Result<F::Output> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(f))
}

fn install(
    home: &TernHome,
    source: &str,
    sha256: Option<&str>,
    opts: &InstallOptions,
    dry_run: bool,
    yes: bool,
    update: bool,
) -> anyhow::Result<ExitCode> {
    let installer = Installer::new(home);
    let bundle = block_on(installer.read(source, sha256))??;
    let store = open_store(home)?;
    let prepared = installer.prepare(bundle, Some(&store))?;
    let plan = &prepared.plan;
    print!("{}", plan.render());
    println!();
    let conflicts = plan.conflicts();
    if !conflicts.is_empty() {
        for c in conflicts {
            eprintln!("✗ {c}");
        }
        return Ok(ExitCode::FAILURE);
    }
    if dry_run {
        println!("(dry run: nothing installed)");
        return Ok(ExitCode::SUCCESS);
    }
    let same = plan
        .installed
        .as_ref()
        .is_some_and(|i| i.sha256 == plan.sha256);
    if update && same {
        println!("{} is up to date ({}).", plan.name, plan.version);
        return Ok(ExitCode::SUCCESS);
    }
    // An update that can't do anything the approved version couldn't goes
    // ahead; anything else asks.
    let grows =
        plan.changes.as_ref().is_none_or(|c| c.grows()) || plan.signature.state == "new_key";
    let question = match &plan.installed {
        Some(i) => format!(
            "Update {} from {} to {}?",
            plan.name, i.version, plan.version
        ),
        None => format!("Install {} {}?", plan.name, plan.version),
    };
    if grows && !yes && !confirm(&question) {
        println!("Not installed.");
        return Ok(ExitCode::FAILURE);
    }
    let out = installer.install(&prepared, &store, opts)?;
    for l in &out.lines {
        println!("  {l}");
    }
    println!(
        "{} {} {}. Pinned in {}.",
        if plan.installed.is_some() {
            "Updated"
        } else {
            "Installed"
        },
        plan.name,
        plan.version,
        installer.paths.lock().display()
    );
    if !plan.connectors.is_empty() {
        println!(
            "Connectors are drafts until activated: ask tern to activate one, or use the app's Library."
        );
    }
    let missing: Vec<&str> = plan
        .secrets
        .iter()
        .filter(|s| !s.set)
        .map(|s| s.name.as_str())
        .collect();
    if !missing.is_empty() {
        println!(
            "Set these secrets before using them: {}",
            missing.join(", ")
        );
    }
    for c in plan.connections.iter().filter(|c| !c.connected) {
        println!(
            "Sign in for {}: tern connections add {}",
            c.connector, c.connector
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn list(home: &TernHome) -> anyhow::Result<ExitCode> {
    let bundles = tern_bundle::installed(home);
    if bundles.is_empty() {
        println!("No bundles installed. Install one with `tern bundle install <url|file|dir>`.");
    }
    for b in &bundles {
        println!("{} {} — {}", b.name, b.version, b.signature_words());
        println!("    from {}", b.source);
        println!("    sha256 {}", b.sha256);
        let mut parts = Vec::new();
        if !b.connectors.is_empty() {
            parts.push(format!("connectors {}", b.connectors.join(", ")));
        }
        if !b.skills.is_empty() {
            parts.push(format!("skills {}", b.skills.join(", ")));
        }
        if !b.routines.is_empty() {
            let names: Vec<&str> = b.routines.iter().map(|r| r.name.as_str()).collect();
            parts.push(format!("routines {}", names.join(", ")));
        }
        if !parts.is_empty() {
            println!("    {}", parts.join("; "));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn remove(home: &TernHome, name: &str, yes: bool) -> anyhow::Result<ExitCode> {
    let installer = Installer::new(home);
    let Some(b) = installer.installed(name)? else {
        eprintln!("no bundle named {name} is installed (see `tern bundle list`)");
        return Ok(ExitCode::FAILURE);
    };
    println!(
        "Removing {} {} removes connectors {}, skills {} and {} routine(s).",
        b.name,
        b.version,
        if b.connectors.is_empty() {
            "(none)".into()
        } else {
            b.connectors.join(", ")
        },
        if b.skills.is_empty() {
            "(none)".into()
        } else {
            b.skills.join(", ")
        },
        b.routines.len()
    );
    if !yes && !confirm(&format!("Remove {name}?")) {
        return Ok(ExitCode::FAILURE);
    }
    let store = open_store(home)?;
    let out = installer.remove(name, &store)?;
    for l in &out.lines {
        println!("  {l}");
    }
    println!("Removed {name}.");
    Ok(ExitCode::SUCCESS)
}

fn export(
    home: &TernHome,
    paths: &Paths,
    req: &tern_bundle::export::Request,
    output: Option<std::path::PathBuf>,
    dir: bool,
    unsigned: bool,
) -> anyhow::Result<ExitCode> {
    let key = if unsigned || !paths.signing_key().exists() {
        None
    } else {
        Some(SecretKey::load(&paths.signing_key())?)
    };
    let store = home
        .db_path()
        .exists()
        .then(|| Store::open(&home.db_path()))
        .transpose()?;
    let out = tern_bundle::export::export(home, store.as_ref(), req, key.as_ref())?;
    let h = &req.header;
    let target = output.unwrap_or_else(|| {
        if dir {
            format!("{}-{}", h.name, h.version).into()
        } else {
            format!("{}-{}.tar.zst", h.name, h.version).into()
        }
    });
    let sha = if dir {
        out.write_dir(&target)?
    } else {
        out.write_archive(&target)?
    };
    for n in &out.notes {
        println!("  note: {n}");
    }
    let listed: Vec<&str> = out
        .files
        .keys()
        .map(String::as_str)
        .filter(|p| !p.starts_with("bundle.toml"))
        .collect();
    println!(
        "Wrote {} ({} file{}): {}",
        target.display(),
        listed.len(),
        if listed.len() == 1 { "" } else { "s" },
        listed.join(", ")
    );
    match &out.signed_by {
        Some(id) => println!("Signed by {id}."),
        None if unsigned => println!("Unsigned (--unsigned)."),
        None => println!("Unsigned: make a key with `tern bundle keygen` to sign what you share."),
    }
    println!("sha256 {sha}");
    println!(
        "Others install it with: tern bundle install <url>#sha256={sha}{}",
        if dir {
            " (the hash of its bundle.toml)"
        } else {
            ""
        }
    );
    Ok(ExitCode::SUCCESS)
}

/// `tern import openclaw`: the same plan and approval as a bundle, plus the
/// profile files and what to set up by hand.
pub fn import(home: &TernHome, command: ImportCommand) -> anyhow::Result<ExitCode> {
    let ImportCommand::Openclaw {
        dir,
        agent,
        dry_run,
        yes,
    } = command;
    let dir = match dir.or_else(tern_bundle::openclaw::default_dir) {
        Some(d) => d,
        None => anyhow::bail!("give OpenClaw's directory: tern import openclaw <dir>"),
    };
    home.ensure_layout()?;
    let imp = tern_bundle::openclaw::scan(&dir, home)?;
    let installer = Installer::new(home);
    let store = open_store(home)?;
    let prepared = match tern_bundle::openclaw::as_raw(&imp, &dir)? {
        Some(raw) => {
            Some(installer.prepare(tern_bundle::source::verify(raw, None)?, Some(&store))?)
        }
        None => None,
    };
    print!("{}", imp.render());
    if let Some(p) = &prepared {
        println!();
        print!("{}", p.plan.render());
        let conflicts = p.plan.conflicts();
        if !conflicts.is_empty() {
            println!();
            for c in conflicts {
                eprintln!("✗ {c}");
            }
            return Ok(ExitCode::FAILURE);
        }
    }
    println!();
    if prepared.is_none() && imp.profile.is_empty() {
        println!("Nothing to import.");
        return Ok(ExitCode::SUCCESS);
    }
    if dry_run {
        println!("(dry run: nothing imported)");
        return Ok(ExitCode::SUCCESS);
    }
    if !yes && !confirm("Import this?") {
        println!("Nothing imported.");
        return Ok(ExitCode::FAILURE);
    }
    if let Some(p) = &prepared {
        let out = installer.install(
            p,
            &store,
            &InstallOptions {
                agent,
                accept_new_key: false,
            },
        )?;
        for l in &out.lines {
            println!("  {l}");
        }
    }
    for l in tern_bundle::openclaw::apply_profile(&imp)? {
        println!("  {l}");
    }
    println!(
        "Imported. Skills and routines are the bundle {} (`tern bundle remove {}` takes them back).",
        tern_bundle::openclaw::BUNDLE_NAME,
        tern_bundle::openclaw::BUNDLE_NAME
    );
    Ok(ExitCode::SUCCESS)
}
