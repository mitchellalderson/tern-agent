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
    /// Show tern in the menu bar: its state, Open, Start/Quit and Start at
    /// login (macOS).
    Tray,
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
    /// List, create, edit and archive your agents: each has its own
    /// persona, model, tools, memory, routines and DM.
    Agents {
        #[command(subcommand)]
        command: Option<AgentsCommand>,
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
    /// Writes waiting for your approval (deferred because nobody answered).
    Outbox {
        #[command(subcommand)]
        command: Option<OutboxCommand>,
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
    /// List, add, test and manage MCP servers. Changes apply to a running
    /// daemon at once.
    Mcp {
        #[command(subcommand)]
        command: Option<McpCommand>,
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
    /// Delete all local data (conversations, runs, memory, skills,
    /// connectors, logs, config, secrets), stopping tern first.
    Reset {
        /// Keep config.toml and secrets.toml (providers and API keys).
        #[arg(long)]
        keep_config: bool,
        /// Don't ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Install the service. A crash restarts tern; `tern stop` doesn't.
    Install {
        /// Install without starting tern at login.
        #[arg(long, conflicts_with = "tray")]
        no_login: bool,
        /// Also start the menu-bar icon (`tern tray`) at login.
        #[arg(long)]
        tray: bool,
    },
    /// Remove the service and the menu-bar icon's (tern keeps running
    /// until stopped).
    Uninstall,
    /// Whether the service is installed, loaded and running.
    Status {
        /// Print JSON (for the desktop app).
        #[arg(long)]
        json: bool,
    },
    /// Print the daemon plist bundled in `tern Tray.app` (for packaging).
    #[command(hide = true)]
    BundlePlist,
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

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// Configured servers and their state.
    List {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// One server: config, state, tools and its recent stderr.
    Show { name: String },
    /// Add a server: `tern mcp add github -- github-mcp-server stdio`, or
    /// `tern mcp add linear --url https://mcp.linear.app/mcp`. It's tested
    /// first, and missing secrets are asked for (hidden).
    Add {
        name: String,
        /// Endpoint of a streamable HTTP server (instead of a command).
        #[arg(long, conflicts_with = "command")]
        url: Option<String>,
        /// Plain environment for the command: `--env LOG_LEVEL=warn`.
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        /// A secret passed to the command's environment: `--secret GITHUB_TOKEN`.
        #[arg(long = "secret", value_name = "NAME")]
        secrets: Vec<String>,
        /// Working directory for the command.
        #[arg(long)]
        cwd: Option<String>,
        /// A secret sent as `Authorization: Bearer …` (with --url).
        #[arg(long, value_name = "NAME", requires = "url")]
        bearer_secret: Option<String>,
        /// A header filled from a secret: `--header-secret X-API-Key=FOO_KEY`.
        #[arg(long = "header-secret", value_name = "H=NAME", requires = "url")]
        header_secrets: Vec<String>,
        /// Ask before each of its tool calls.
        #[arg(long)]
        approve: bool,
        /// Save without starting it first.
        #[arg(long)]
        no_test: bool,
        /// The command and its arguments, after `--`.
        #[arg(last = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// Add the servers in a pasted `mcpServers` JSON snippet (from FILE, or
    /// stdin when FILE is omitted).
    Import {
        file: Option<std::path::PathBuf>,
        /// Save without starting them first.
        #[arg(long)]
        no_test: bool,
    },
    /// Start a configured server and list its tools, without changing it.
    Test { name: String },
    /// Turn a server on.
    Enable { name: String },
    /// Turn a server off without removing it.
    Disable { name: String },
    /// Reconnect now.
    Restart { name: String },
    /// Remove a server and, unless --keep-secrets, the secrets only it uses.
    Rm {
        name: String,
        #[arg(long)]
        keep_secrets: bool,
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
pub enum AgentsCommand {
    /// List agents (the default, `tern`, first).
    List {
        /// Include archived agents.
        #[arg(long)]
        all: bool,
    },
    /// Create an agent and its DM.
    New {
        /// Display name, e.g. "Research".
        name: String,
        /// Its @name (default: from the name).
        #[arg(long)]
        slug: Option<String>,
        /// Start from a template: research, inbox, marketing or blank.
        #[arg(long)]
        template: Option<String>,
        /// `private` (default) or `shared`.
        #[arg(long)]
        memory: Option<String>,
    },
    /// Open an agent's persona in $EDITOR.
    Edit { agent: String },
    /// Archive an agent: hide it and pause its routines. Nothing is deleted.
    Archive {
        agent: String,
        /// Unarchive it instead.
        #[arg(long)]
        undo: bool,
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
pub enum OutboxCommand {
    /// What is waiting (`--all`: decided items too).
    List {
        #[arg(long)]
        all: bool,
    },
    /// One item, with its arguments and result.
    Show { id: i64 },
    /// Run it now, exactly once.
    Approve {
        id: i64,
        /// JSON arguments to run it with instead of the agent's.
        #[arg(long)]
        input: Option<String>,
    },
    /// Don't run it; the run that queued it is told if still working.
    Reject {
        id: i64,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConnectorsCommand {
    List,
    Test { name: String },
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Skip network checks (provider reachability) and starting MCP servers.
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
