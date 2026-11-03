mod cli;
mod config_cmd;
mod connectors_cmd;
mod daemon;
mod devices_cmd;
mod doctor;
mod gateway_cmd;
mod inspect;
mod logging;
mod remote;
mod run_cmd;
mod runtime;
mod tui_backend;

use std::process::ExitCode;

use clap::Parser;
use tern_core::TernHome;

use cli::{Cli, Command, ConfigCommand, ConnectorsCommand, RunsCommand, SessionsCommand};

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
    // Commands that run the agent own the data directory; create it before
    // logging starts so their first invocation is logged too.
    if matches!(
        cli.command,
        Some(Command::Run { .. } | Command::Resume { .. })
    ) {
        home.ensure_layout()?;
    }
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
        None => {
            let remote = match cli.remote {
                Some(url) => Some(tui_backend::RemoteTarget {
                    url,
                    token: std::env::var("TERN_REMOTE_TOKEN")
                        .ok()
                        .filter(|t| !t.is_empty())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "--remote needs a device token in $TERN_REMOTE_TOKEN (get one with `tern devices join <link>`)"
                            )
                        })?,
                    pin: cli
                        .pin
                        .or_else(|| std::env::var("TERN_REMOTE_PIN").ok().filter(|p| !p.is_empty())),
                }),
                None => None,
            };
            tui_backend::run(&home, remote)
        }
        Some(Command::Devices { command }) => devices_cmd::run(&home, command),
        Some(Command::Run {
            prompt,
            session,
            model,
            yes,
        }) => run_cmd::run(
            &home,
            run_cmd::RunArgs {
                prompt: &prompt,
                session: session.as_deref(),
                model: model.as_deref(),
                yes,
            },
        ),
        Some(Command::Resume { run_id, yes }) => run_cmd::resume(&home, &run_id, yes),
        Some(Command::Daemon) => daemon::run(&home),
        Some(Command::Gateway {
            command: cli::GatewayCommand::Pair,
        }) => gateway_cmd::pair(&home),
        Some(Command::Sessions { command }) => {
            match command.unwrap_or(SessionsCommand::List) {
                SessionsCommand::List => inspect::sessions_list(&home)?,
                SessionsCommand::Show { id } => inspect::sessions_show(&home, &id)?,
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Runs { command }) => {
            match command {
                RunsCommand::List => inspect::runs_list(&home)?,
                RunsCommand::Show { run_id } => inspect::runs_show(&home, &run_id)?,
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Connectors { command }) => match command.unwrap_or(ConnectorsCommand::List) {
            ConnectorsCommand::List => connectors_cmd::list(&home),
            ConnectorsCommand::Test { name } => connectors_cmd::test(&home, &name),
        },
    }
}
