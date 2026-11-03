//! `tern config init | get | set`.

use std::path::Path;

use anyhow::{Context, bail};
use tern_core::{Config, TernHome, defaults};

pub fn init(home: &TernHome, force: bool) -> anyhow::Result<()> {
    home.ensure_layout()
        .with_context(|| format!("creating {}", home.root().display()))?;

    let config_path = home.config_path();
    if force || !config_path.exists() {
        write_atomic(&config_path, defaults::CONFIG_TOML)?;
        println!("wrote   {}", config_path.display());
    } else {
        println!("exists  {}", config_path.display());
    }
    for (path, body) in [
        (home.persona_path(), defaults::PERSONA_MD),
        (home.user_path(), defaults::USER_MD),
    ] {
        if path.exists() {
            println!("exists  {}", path.display());
        } else {
            std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
            println!("wrote   {}", path.display());
        }
    }

    let (config, _) = Config::load(&config_path)?;
    let workspace = config.workspace_dir();
    std::fs::create_dir_all(&workspace)
        .with_context(|| format!("creating workspace {}", workspace.display()))?;
    println!("ready   {} (workspace)", workspace.display());
    println!("\nNext: add a provider to config.toml, then run `tern doctor`.");
    Ok(())
}

pub fn get(home: &TernHome, key: Option<&str>) -> anyhow::Result<()> {
    let (config, _) = Config::load(&home.config_path())?;
    let mut value = toml::Value::try_from(&config).context("serializing config")?;
    if let Some(key) = key {
        for part in key.split('.') {
            value = match value {
                toml::Value::Table(mut t) => t.remove(part),
                _ => None,
            }
            .with_context(|| format!("no config value at {key:?}"))?;
        }
    }
    match value {
        toml::Value::String(s) => println!("{s}"),
        toml::Value::Table(t) => print!("{}", toml::to_string_pretty(&t)?),
        other => println!("{other}"),
    }
    Ok(())
}

pub fn set(home: &TernHome, key: &str, raw: &str) -> anyhow::Result<()> {
    let path = home.config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;

    let value: toml_edit::Value = raw.parse().unwrap_or_else(|_| raw.into());
    let parts: Vec<&str> = key.split('.').collect();
    let Some((leaf, tables)) = parts.split_last() else {
        bail!("empty key");
    };
    if parts.iter().any(|p| p.is_empty()) {
        bail!("invalid key {key:?}");
    }
    let mut table = doc.as_table_mut();
    for part in tables {
        let entry = table
            .entry(part)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        table = entry
            .as_table_mut()
            .with_context(|| format!("{part:?} in {key:?} is not a table"))?;
    }
    table.insert(leaf, toml_edit::value(value));

    let updated = doc.to_string();
    Config::parse(&updated, &path).context("refusing to write an invalid config")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(&path, &updated)
}

fn write_atomic(path: &Path, body: &str) -> anyhow::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}
