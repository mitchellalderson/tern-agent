//! `tern config init | get | set`.

use std::path::Path;

use anyhow::Context;
use tern_core::config::edit::ConfigFile;
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
    println!("\nNext: run `tern setup` to connect a model provider.");
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
    let mut file = ConfigFile::open(&path)?;
    file.set(key, raw)?;
    file.save().context("refusing to write an invalid config")?;
    Ok(())
}

fn write_atomic(path: &Path, body: &str) -> anyhow::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}
