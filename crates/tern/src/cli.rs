use clap::{Args, Parser, Subcommand};

/// A personal AI agent in one static binary.
#[derive(Debug, Parser)]
#[command(name = "tern", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a one-shot prompt non-interactively and print the final answer.
    Run {
        prompt: String,
        #[arg(long)]
        session: Option<String>,
        /// Model as provider:model.
        #[arg(long)]
        model: Option<String>,
        /// Auto-approve every tool call (for scripting and tests).
        #[arg(long)]
        yes: bool,
    },
    /// Resume an interrupted run.
    Resume {
        run_id: String,
        /// Auto-approve every tool call.
        #[arg(long)]
        yes: bool,
    },
    /// Run the gateway, scheduler, and local API in the foreground.
    Daemon,
    /// Messaging gateways.
    Gateway {
        #[command(subcommand)]
        command: GatewayCommand,
    },
    /// List or show conversation sessions.
    Sessions {
        #[command(subcommand)]
        command: Option<SessionsCommand>,
    },
    /// Inspect runs.
    Runs {
        #[command(subcommand)]
        command: RunsCommand,
    },
    /// Manage Luau connectors.
    Connectors {
        #[command(subcommand)]
        command: Option<ConnectorsCommand>,
    },
    /// Check configuration, providers, database, and integrations.
    Doctor(DoctorArgs),
    /// Read and write config.toml.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum GatewayCommand {
    /// Print a one-time pairing code to send to the Telegram bot.
    Pair,
}

#[derive(Debug, Subcommand)]
pub enum SessionsCommand {
    List,
    Show { id: String },
}

#[derive(Debug, Subcommand)]
pub enum RunsCommand {
    /// Print a run's step journal.
    Show { run_id: String },
}

#[derive(Debug, Subcommand)]
pub enum ConnectorsCommand {
    List,
    Test { name: String },
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Skip network checks (provider reachability).
    #[arg(long)]
    pub offline: bool,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Create the data directory, an example config.toml, and default persona/user notes.
    Init {
        /// Overwrite config.toml if it exists.
        #[arg(long)]
        force: bool,
    },
    /// Print a config value by dotted key (or the whole effective config).
    Get { key: Option<String> },
    /// Set a config value by dotted key. Values are parsed as TOML, else stored as strings.
    Set { key: String, value: String },
}
