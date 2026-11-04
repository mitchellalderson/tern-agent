mod agents_cmd;
mod bundle_cmd;
mod cli;
mod config_cmd;
mod connections_cmd;
mod connectors_cmd;
mod crash;
mod daemon;
mod devices_cmd;
mod doctor;
mod gateway_cmd;
mod grants_cmd;
mod hooks_cmd;
mod inspect;
mod ledger_cmd;
mod lifecycle;
mod logging;
mod mcp;
mod mcp_cmd;
mod mcp_serve;
mod mcp_tools;
mod outbox_cmd;
mod providers;
mod providers_cmd;
mod remote;
mod report_cmd;
mod reset_cmd;
mod routines;
mod run_cmd;
mod runtime;
mod security_cmd;
mod service;
mod settings;
mod subagents;
mod tasks_cmd;
mod tray;
mod tui_backend;

use std::process::ExitCode;

use clap::Parser;
use tern_core::TernHome;

use cli::{
    Cli, Command, ConfigCommand, ConnectorsCommand, RunsCommand, ServiceCommand, SessionsCommand,
};

/// Counts live bytes so a `python` helper can cap its memory
/// (docs/python-tool-spec.md §5.2). Elsewhere it only counts: the ceiling is
/// armed by the helper alone.
#[cfg(feature = "python")]
#[global_allocator]
static ALLOC: monty_alloc::LimitedAllocator = monty_alloc::LimitedAllocator;

fn main() -> ExitCode {
    // `tern __sandbox-exec …`: confine this process, then exec a shell command.
    if let Some(code) = tern_sandbox::run_helper_if_requested() {
        return code;
    }
    // `tern __monty-exec`: interpret one `python` script for the daemon.
    #[cfg(feature = "python")]
    if let Some(code) = tern_python::helper::run_if_requested() {
        return code;
    }
    let cli = Cli::parse_from(tray::rewrite_args(std::env::args_os().collect()));
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
        None | Some(
            Command::Run { .. }
                | Command::Resume { .. }
                | Command::Daemon
                | Command::Gateway { .. }
        )
    ) {
        home.ensure_layout()?;
    }
    // Only log to file when the data directory already exists, so read-only
    // commands like `doctor` don't create ~/.tern as a side effect.
    logging::init(&home);
    crash::install(&home);
    if std::env::var_os(crash::TEST_PANIC_ENV).is_some() {
        panic!("{} is set", crash::TEST_PANIC_ENV);
    }
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
            tui_backend::run(&home, remote, None)
        }
        Some(Command::Setup) => {
            home.ensure_layout()?;
            tui_backend::run(&home, None, Some("/setup"))
        }
        Some(Command::Providers { command }) => {
            home.ensure_layout()?;
            providers_cmd::run(&home, command)
        }
        Some(Command::Mcp { command }) => {
            home.ensure_layout()?;
            mcp_cmd::run(&home, command)
        }
        Some(Command::Models {
            provider,
            refresh,
            json,
        }) => providers_cmd::models(&home, provider, refresh, json),
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
        Some(Command::Start) => lifecycle::start(&home),
        Some(Command::Reset { keep_config, yes }) => reset_cmd::run(&home, keep_config, yes),
        Some(Command::Stop) => lifecycle::stop(&home),
        Some(Command::Service { command }) => match command {
            ServiceCommand::Install { no_login, tray } => service::install(&home, !no_login, tray),
            ServiceCommand::Uninstall => service::uninstall(&home),
            ServiceCommand::Status { json } => service::status(&home, json),
            ServiceCommand::BundlePlist => {
                print!("{}", service::bundled_plist());
                Ok(ExitCode::SUCCESS)
            }
        },
        Some(Command::Tray) => tray::run(&home),
        Some(Command::Report { output, run }) => {
            report_cmd::run(&home, output.as_deref(), run.as_deref())
        }
        Some(Command::Gateway { command }) => match command {
            cli::GatewayCommand::Pair => gateway_cmd::pair(&home),
            cli::GatewayCommand::Add {
                kind,
                server,
                channel,
                force,
            } => gateway_cmd::add(&home, &kind, server.as_deref(), channel.as_deref(), force),
            cli::GatewayCommand::List => gateway_cmd::list(&home),
        },
        Some(Command::Agents { command }) => agents_cmd::run(&home, command),
        Some(Command::Sessions { command }) => {
            match command.unwrap_or(SessionsCommand::List) {
                SessionsCommand::List => inspect::sessions_list(&home)?,
                SessionsCommand::Show { id } => inspect::sessions_show(&home, &id)?,
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Outbox { command }) => outbox_cmd::run(&home, command),
        Some(Command::Hooks { command }) => hooks_cmd::run(&home, command),
        Some(Command::Grants { command }) => grants_cmd::run(&home, command),
        Some(Command::Ledger { command }) => ledger_cmd::run(&home, command),
        Some(Command::Tasks { json }) => tasks_cmd::run(&home, json),
        Some(Command::Security { command }) => match command {
            cli::SecurityCommand::Audit { json } => security_cmd::run(&home, json),
        },
        Some(Command::Runs { command }) => {
            match command {
                RunsCommand::List => inspect::runs_list(&home)?,
                RunsCommand::Show { run_id } => inspect::runs_show(&home, &run_id)?,
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Bundle { command }) => bundle_cmd::run(&home, command),
        Some(Command::Import { command }) => bundle_cmd::import(&home, command),
        Some(Command::Connections { command }) => {
            home.ensure_layout()?;
            connections_cmd::run(&home, command)
        }
        Some(Command::Connectors { command }) => match command.unwrap_or(ConnectorsCommand::List) {
            ConnectorsCommand::List => connectors_cmd::list(&home),
            ConnectorsCommand::Test {
                name,
                record,
                tool,
                args,
            } => connectors_cmd::test(&home, &name, record, tool.as_deref(), args.as_deref()),
            ConnectorsCommand::FromOpenapi {
                source,
                name,
                ops,
                list,
                tag,
                search,
                offset,
                base_url,
            } => connectors_cmd::from_openapi(
                &home,
                &connectors_cmd::FromOpenapi {
                    source,
                    name,
                    ops,
                    list,
                    tag,
                    search,
                    offset,
                    base_url,
                },
            ),
        },
    }
}
