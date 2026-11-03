mod cli;
mod config_cmd;
mod doctor;
mod logging;

use std::process::ExitCode;

use clap::Parser;
use tern_core::TernHome;

use cli::{Cli, Command, ConfigCommand};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let home = TernHome::resolve()?;
    // Only log to file when the data directory already exists, so read-only
    // commands like `doctor` don't create ~/.tern as a side effect.
    let _log_guard = logging::init(&home);
    tracing::debug!(command = ?cli.command, "starting");

    match cli.command {
        Some(Command::Doctor(args)) => doctor::run(&home, &args),
        Some(Command::Config { command }) => {
            match command {
                ConfigCommand::Init { force } => config_cmd::init(&home, force)?,
                ConfigCommand::Get { key } => config_cmd::get(&home, key.as_deref())?,
                ConfigCommand::Set { key, value } => config_cmd::set(&home, &key, &value)?,
            }
            Ok(ExitCode::SUCCESS)
        }
        None => not_yet("the TUI", "M3"),
        Some(Command::Run { .. }) => not_yet("`tern run`", "M1"),
        Some(Command::Resume { .. }) => not_yet("`tern resume`", "M2"),
        Some(Command::Daemon) => not_yet("`tern daemon`", "M5"),
        Some(Command::Gateway { .. }) => not_yet("`tern gateway`", "M5"),
        Some(Command::Sessions { .. }) => not_yet("`tern sessions`", "M2"),
        Some(Command::Runs { .. }) => not_yet("`tern runs`", "M2"),
        Some(Command::Connectors { .. }) => not_yet("`tern connectors`", "M4"),
    }
}

fn not_yet(what: &str, milestone: &str) -> anyhow::Result<ExitCode> {
    eprintln!("{what} is not implemented yet (planned for milestone {milestone}).");
    Ok(ExitCode::from(2))
}
