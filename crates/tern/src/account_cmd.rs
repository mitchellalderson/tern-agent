//! `tern account status` (deployment spec §10.4a).

use std::process::ExitCode;

use tern_core::{Config, TernHome};

use crate::cli::AccountCommand;
use crate::daemon::account::AccountVerifier;

pub fn run(home: &TernHome, command: Option<AccountCommand>) -> anyhow::Result<ExitCode> {
    match command.unwrap_or(AccountCommand::Status) {
        AccountCommand::Status => status(home),
    }
}

fn status(home: &TernHome) -> anyhow::Result<ExitCode> {
    let (config, _) = Config::load(&home.config_path())?;
    let acct = &config.account;
    if !acct.enabled() {
        println!("Accounts: off (set [account] in config.toml to turn them on)");
        return Ok(ExitCode::SUCCESS);
    }
    println!("Accounts: on");
    println!("  issuer:    {}", acct.issuer.as_deref().unwrap_or("-"));
    println!("  audience:  {}", acct.audience.as_deref().unwrap_or("-"));
    match acct.owner.as_deref() {
        Some(o) if !o.is_empty() => println!("  owner:     {o}"),
        _ => println!("  owner:     not set (no account can sign in)"),
    }
    if let Err(e) = acct.check() {
        println!("  problem:   {e}");
        return Ok(ExitCode::FAILURE);
    }
    let Some(verifier) = AccountVerifier::from_config(acct) else {
        return Ok(ExitCode::FAILURE);
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match runtime.block_on(verifier.fetch()) {
        Ok(keys) => println!("  keys:      {} fetched", keys.keys.len()),
        Err(e) => {
            println!("  keys:      unreachable ({e})");
            return Ok(ExitCode::FAILURE);
        }
    }
    Ok(ExitCode::SUCCESS)
}
