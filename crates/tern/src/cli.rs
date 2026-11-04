use clap::{Args, Parser, Subcommand};

/// A personal AI agent in one static binary.
#[derive(Debug, Parser)]
#[command(name = "tern", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Attach the chat UI to a core over HTTPS (e.g. https://host:7420)
    /// instead of the local daemon. The device token comes from
    /// $TERN_REMOTE_TOKEN (get one with `tern devices join`).
    #[arg(long, value_name = "URL")]
    pub remote: Option<String>,
    /// Certificate pin for --remote (`sha256:…`, from the pairing link);
    /// also read from $TERN_REMOTE_PIN.
    #[arg(long, requires = "remote")]
    pub pin: Option<String>,
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
    /// Start tern in the background (through the service if installed).
    Start,
    /// Stop tern. Tasks in progress pick up where they left off next start.
    Stop,
    /// Start tern at login through launchd (macOS) or systemd (Linux).
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Pair and manage devices that reach this core over TCP.
    Devices {
        #[command(subcommand)]
        command: Option<DevicesCommand>,
    },
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
    /// Write a redacted bug report: environment, doctor, config, the last
    /// problem run, crashes, and recent logs.
    Report {
        /// Where to write it (`-` for stdout). Default: logs/report-<time>.md.
        #[arg(long, short)]
        output: Option<std::path::PathBuf>,
        /// Include this run's journal instead of the latest unfinished one.
        #[arg(long)]
        run: Option<String>,
    },
    /// Connect a model provider: choose one, paste an API key, pick a model.
    Setup,
    /// List, add or remove model providers (for scripts; `tern setup` is
    /// the guided way).
    Providers {
        #[command(subcommand)]
        command: Option<ProvidersCommand>,
    },
    /// List the models each provider offers.
    Models {
        /// Only this provider.
        provider: Option<String>,
        /// Ask the providers again instead of using the cached lists.
        #[arg(long)]
        refresh: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
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
pub enum ServiceCommand {
    /// Install the service. A crash restarts tern; `tern stop` doesn't.
    Install {
        /// Install without starting tern at login.
        #[arg(long)]
        no_login: bool,
    },
    /// Remove the service (tern keeps running until stopped).
    Uninstall,
    /// Whether the service is installed, loaded and running.
    Status,
}

#[derive(Debug, Subcommand)]
pub enum ProvidersCommand {
    /// Configured providers and their keys (never the values).
    List,
    /// Add a provider. The key is read from stdin when it isn't a terminal
    /// (`pbpaste | tern providers add anthropic`), else asked for hidden.
    Add {
        /// `anthropic` or `openai-compat`.
        kind: ProviderKindArg,
        /// Base URL, required for openai-compat (e.g. http://localhost:11434/v1).
        #[arg(long)]
        url: Option<String>,
        /// Name to refer to it by (default: anthropic, or one from the URL).
        #[arg(long)]
        name: Option<String>,
        /// Model to make the default (default: the first listed, when there
        /// is no usable default yet).
        #[arg(long)]
        default_model: Option<String>,
        /// Use the key already in the environment.
        #[arg(long)]
        env_key: bool,
        /// No key: for local servers that don't need one.
        #[arg(long, conflicts_with = "env_key")]
        no_key: bool,
    },
    /// Remove a provider and, unless --keep-key, its API key.
    Rm {
        name: String,
        #[arg(long)]
        keep_key: bool,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ProviderKindArg {
    Anthropic,
    OpenaiCompat,
}

#[derive(Debug, Subcommand)]
pub enum GatewayCommand {
    /// Print a one-time pairing code to send to the Telegram bot.
    Pair,
}

#[derive(Debug, Subcommand)]
pub enum DevicesCommand {
    /// Print a one-time pairing link (and QR code) for a phone or another
    /// computer. Valid for 10 minutes.
    Pair {
        /// owner (everything), chat (conversations and approvals, no
        /// config or secrets), or read (view only).
        #[arg(long, default_value = "chat")]
        scope: String,
        /// Name for the device; otherwise the device names itself.
        #[arg(long)]
        name: Option<String>,
        /// URL devices reach this core at (default: `api.public_url`, else
        /// the first TCP listener).
        #[arg(long)]
        url: Option<String>,
    },
    /// List paired devices.
    List,
    /// Revoke a device's token, signing it out at once.
    Revoke { id: String },
    /// Rename a device.
    Rename { id: String, name: String },
    /// Pair this machine with a remote core from its pairing link, and
    /// print the device token.
    Join {
        link: String,
        /// Name to show on the core (default: this host's name).
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SessionsCommand {
    List,
    Show { id: String },
}

#[derive(Debug, Subcommand)]
pub enum RunsCommand {
    /// List the 20 most recent runs.
    List,
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
