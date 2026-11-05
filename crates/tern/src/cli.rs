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
    /// List, create, rename, archive and delete topic channels.
    Channels {
        #[command(subcommand)]
        command: Option<ChannelsCommand>,
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
    /// The "always" grants approvals have recorded, and removing them.
    Grants {
        #[command(subcommand)]
        command: Option<GrantsCommand>,
    },
    /// Security checks.
    Security {
        #[command(subcommand)]
        command: SecurityCommand,
    },
    /// Everything running, queued, scheduled, waiting on you (approvals,
    /// the outbox, secret requests) or failed recently, across agents.
    Tasks {
        #[arg(long)]
        json: bool,
    },
    /// The activity ledger: approvals, grants, writes, connections and
    /// other security-relevant changes, hash-chained (`verify` checks it).
    Ledger {
        #[command(subcommand)]
        command: Option<LedgerCommand>,
    },
    /// Skills, and turning a good run into one: after a long, clean run tern
    /// offers once; `save` writes a draft you review and `publish`.
    Skills {
        #[command(subcommand)]
        command: Option<SkillsCommand>,
    },
    /// Writes waiting for your approval (deferred because nobody answered).
    Outbox {
        #[command(subcommand)]
        command: Option<OutboxCommand>,
    },
    /// Webhook routines: create one (its URL and secret are shown once),
    /// rotate or revoke its secret, or fire it locally.
    #[command(alias = "hook")]
    Hooks {
        #[command(subcommand)]
        command: Option<HooksCommand>,
    },
    /// Manage Luau connectors.
    Connectors {
        #[command(subcommand)]
        command: Option<ConnectorsCommand>,
    },
    /// Bundles of connectors, skills and routines: signed, hash-pinned,
    /// installed only after you've seen everything they can do.
    Bundle {
        #[command(subcommand)]
        command: Option<BundleCommand>,
    },
    /// Bring another agent's setup across.
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
    /// Accounts of connectors and MCP servers: list, add (sign in), and
    /// remove them, or pick the default.
    Connections {
        #[command(subcommand)]
        command: Option<ConnectionsCommand>,
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
        /// `anthropic`, a service (`openai`, `openrouter`, `gemini`, `groq`,
        /// `together`, `ollama`, `lmstudio`), or `openai-compat` for any
        /// other OpenAI-compatible server.
        kind: ProviderKindArg,
        /// Base URL: required for openai-compat (e.g. http://localhost:11434/v1),
        /// optional for a service (it has a default).
        #[arg(long)]
        url: Option<String>,
        /// Name to refer to it by (default: the service's name, or one from the URL).
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
    /// Serve tern to other AI clients (Claude Desktop, Cursor, Claude Code)
    /// as an MCP server over stdio, through the running daemon. Calls run
    /// under tern's approvals, start tainted, and show in tern.
    Serve {
        /// Offer this agent profile's tools (overrides `[mcp.serve] agent`).
        #[arg(long)]
        agent: Option<String>,
        /// Print the `mcpServers` snippet for a client's config and exit.
        #[arg(long)]
        print_config: bool,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ProviderKindArg {
    Anthropic,
    /// Any OpenAI-compatible server; needs --url.
    OpenaiCompat,
    // Friendly presets: the URL is filled in.
    Openai,
    Openrouter,
    Gemini,
    Groq,
    Together,
    Ollama,
    Lmstudio,
}

impl ProviderKindArg {
    /// The preset this names (`tern_core::presets`), if it is one.
    pub fn preset_id(self) -> Option<&'static str> {
        Some(match self {
            Self::Anthropic => "anthropic",
            Self::OpenaiCompat => return None,
            Self::Openai => "openai",
            Self::Openrouter => "openrouter",
            Self::Gemini => "gemini",
            Self::Groq => "groq",
            Self::Together => "together",
            Self::Ollama => "ollama",
            Self::Lmstudio => "lmstudio",
        })
    }
}

#[derive(Debug, Subcommand)]
pub enum GatewayCommand {
    /// Print a one-time pairing code to send to the Telegram bot or a
    /// gateway connector.
    Pair,
    /// Install a built-in gateway connector (`ntfy` or `slack`) and enable
    /// it in config.toml.
    Add {
        /// `ntfy` or `slack`.
        kind: String,
        /// A self-hosted ntfy server, e.g. https://ntfy.example.com.
        #[arg(long)]
        server: Option<String>,
        /// The Slack channel id to talk in (C0123456789).
        #[arg(long)]
        channel: Option<String>,
        /// Replace the connector file if it differs from the built-in one.
        #[arg(long)]
        force: bool,
    },
    /// Gateways in config.toml and whether each can run.
    List,
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
    /// List the starter agents `new --template` takes, by category.
    Templates,
    /// Create an agent and its DM.
    New {
        /// Display name, e.g. "Research".
        name: String,
        /// Its @name (default: from the name).
        #[arg(long)]
        slug: Option<String>,
        /// Start from a starter (`tern agents templates` lists them).
        #[arg(long)]
        template: Option<String>,
        /// `private` (default) or `shared`.
        #[arg(long)]
        memory: Option<String>,
        /// The most autonomy its runs may have: readonly, supervised,
        /// autonomous or yolo (the lower of this and `autonomy` applies).
        #[arg(long)]
        autonomy: Option<String>,
    },
    /// Give an agent a Telegram bot of its own, or take it away. The token
    /// is a secret (`tern secrets set <name>`); restart the daemon to apply.
    Telegram {
        agent: String,
        /// The secret holding the BotFather token, or `none` to remove it.
        secret: String,
    },
    /// Set or clear an agent's autonomy ceiling.
    Autonomy {
        agent: String,
        /// readonly, supervised, autonomous, yolo, or `none` to remove it.
        level: String,
    },
    /// Open an agent's persona in $EDITOR.
    Edit { agent: String },
    /// Rename an agent: its display name, and with --slug its @name (its
    /// DM follows; its id and history don't change).
    Rename {
        agent: String,
        /// The new display name.
        name: Option<String>,
        /// The new @name.
        #[arg(long)]
        slug: Option<String>,
    },
    /// Delete an agent for good. Its conversations stay in History under
    /// its name; its private memories go. Routines must be handed to
    /// another agent or deleted too.
    Delete {
        agent: String,
        /// Hand its routines (and the channels it leads) to this agent.
        #[arg(long)]
        reassign_to: Option<String>,
        /// Delete its routines.
        #[arg(long)]
        delete_routines: bool,
        /// Don't ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
    /// Archive an agent: hide it and pause its routines. Nothing is deleted.
    Archive {
        agent: String,
        /// Unarchive it instead.
        #[arg(long)]
        undo: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ChannelsCommand {
    /// List channels (Home first).
    List {
        /// Include archived channels.
        #[arg(long)]
        all: bool,
    },
    /// Create a topic channel.
    New {
        /// Its #name: lowercase letters, digits and dashes.
        slug: String,
        #[arg(long)]
        title: Option<String>,
        /// Instructions every agent in it follows.
        #[arg(long)]
        instructions: Option<String>,
    },
    /// Rename or re-describe a topic channel.
    Edit {
        channel: String,
        /// The new #name.
        #[arg(long)]
        slug: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// New instructions (an empty string clears them).
        #[arg(long)]
        instructions: Option<String>,
    },
    /// Archive a channel: hide it, keep everything.
    Archive {
        channel: String,
        /// Unarchive it instead.
        #[arg(long)]
        undo: bool,
    },
    /// Delete a topic channel for good: its posts go, its conversations
    /// stay in History.
    Delete {
        channel: String,
        /// Don't ask for confirmation.
        #[arg(long, short)]
        yes: bool,
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
pub enum GrantsCommand {
    /// Every grant: tool and pattern.
    List,
    /// Remove one, e.g. `tern grants remove http_fetch host:example.com`.
    Remove { tool: String, pattern: String },
}

#[derive(Debug, Subcommand)]
pub enum SecurityCommand {
    /// A read-only report: listeners and their exposure, device scopes,
    /// autonomy, routines that start tainted, legacy `ctx.secret`
    /// connectors, the shell's network and sandbox, broad OAuth scopes,
    /// egress. Exits non-zero on a high finding.
    Audit {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum LedgerCommand {
    /// Recent entries, oldest first (the default).
    List(LedgerArgs),
    /// Recompute the hash chain; exits non-zero if the database was changed
    /// outside tern.
    Verify {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Default, Args)]
pub struct LedgerArgs {
    /// A kind (`grant.add`) or a family (`grant`, `outbox`, `approval`).
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub channel: Option<String>,
    /// A routine id.
    #[arg(long)]
    pub routine: Option<String>,
    /// A connector or MCP server name.
    #[arg(long)]
    pub connector: Option<String>,
    #[arg(long)]
    pub run: Option<String>,
    /// How many (default 100).
    #[arg(long, default_value_t = 100)]
    pub limit: u32,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    /// Skills and drafts.
    List,
    /// A skill's SKILL.md.
    Show { name: String },
    /// Runs offered as skills and waiting for your answer (`--all`: decided too).
    Offers {
        #[arg(long)]
        all: bool,
    },
    /// Accept offer ID: one model call writes a draft skill from that run.
    Save {
        id: i64,
        /// Name the skill yourself instead of using the model's.
        #[arg(long)]
        name: Option<String>,
    },
    /// Decline offer ID; that run is not offered again.
    Dismiss { id: i64 },
    /// Make a draft a skill: the next run lists it.
    Publish { name: String },
    /// Delete a draft.
    Discard { name: String },
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
pub enum HooksCommand {
    /// Every webhook routine's URL, auth and state (never secrets).
    List,
    /// A routine that runs `prompt` on each delivery, with the event
    /// attached as untrusted input.
    Create {
        prompt: String,
        /// `hmac` (default: senders sign the body) or `bearer` (senders
        /// send the secret itself).
        #[arg(long)]
        auth: Option<String>,
        /// The agent whose job it is (id or slug).
        #[arg(long)]
        agent: Option<String>,
        /// Where results are posted (channel slug or id).
        #[arg(long)]
        channel: Option<String>,
        /// Luau run on each event first: false drops it with no model
        /// call; a table replaces it (`event.body`, `event.headers`).
        #[arg(long)]
        gate: Option<String>,
        /// The active connector whose hosts and tools the gate gets.
        #[arg(long)]
        gate_connector: Option<String>,
        /// silent, digest, notify or interrupt.
        #[arg(long)]
        initiative: Option<String>,
    },
    /// A new secret, at once; the old one stops working.
    Rotate {
        id: String,
        #[arg(long)]
        auth: Option<String>,
    },
    /// Stop the URL working (until `rotate`).
    Revoke { id: String },
    /// Deliver a body locally, as a sender would (no secret: you are the
    /// owner). Reads `--body`, else `--file`, else `{}`.
    Fire {
        id: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long, conflicts_with = "body")]
        file: Option<std::path::PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConnectorsCommand {
    List,
    /// Validate a connector and replay its recorded fixtures offline (no
    /// network, no secrets). With --record, call the live API and save the
    /// calls as fixtures instead.
    Test {
        name: String,
        /// Call the live API and save the calls, sanitized, as fixtures
        /// beside the connector (<name>.fixtures.json).
        #[arg(long)]
        record: bool,
        /// The tool to record (default: the recorded read calls again, or
        /// every read tool that needs no arguments).
        #[arg(long, requires = "record")]
        tool: Option<String>,
        /// The tool's arguments, as a JSON object.
        #[arg(long, requires = "tool")]
        args: Option<String>,
    },
    /// Generate a draft connector from an OpenAPI 3 or Swagger 2 document
    /// (a URL or a file, JSON or YAML). It is activated like any draft: ask
    /// tern to activate it and approve its capabilities.
    FromOpenapi {
        /// The document's URL or path.
        source: String,
        /// Connector name (default: from the document's title).
        #[arg(long)]
        name: Option<String>,
        /// Operations to include, comma-separated: names from --list,
        /// operationIds, "GET /path", or tag:<tag>.
        #[arg(long, value_delimiter = ',')]
        ops: Vec<String>,
        /// Only list the operations (a page at a time).
        #[arg(long)]
        list: bool,
        /// With --list: only this tag.
        #[arg(long)]
        tag: Option<String>,
        /// With --list: only operations whose name, path or summary has this.
        #[arg(long)]
        search: Option<String>,
        /// With --list: where the page starts.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Where requests go, when the document's servers are missing or
        /// relative (a file has no URL to resolve them against).
        #[arg(long)]
        base_url: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ImportCommand {
    /// OpenClaw: SKILL.md skills, cron automations (as paused routines),
    /// and SOUL.md, USER.md and MEMORY.md. Credentials are never read:
    /// you're told which secrets and accounts to add.
    Openclaw {
        /// OpenClaw's directory (default ~/.openclaw) or its workspace.
        dir: Option<std::path::PathBuf>,
        /// The agent the routines belong to (default: tern).
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum BundleCommand {
    /// What is installed, from where, and how it is signed.
    List,
    /// Show everything a bundle can do, then install it if you approve.
    /// Connectors arrive inactive and routines paused.
    Install {
        /// An https URL (`…#sha256=<hex>` pins it), a .tar.zst, or a directory.
        source: String,
        /// The SHA-256 the bundle must have (as its author published it).
        #[arg(long)]
        sha256: Option<String>,
        /// The agent its routines belong to (default: tern).
        #[arg(long)]
        agent: Option<String>,
        /// Show what it would do and stop.
        #[arg(long)]
        dry_run: bool,
        /// Approve without asking (you've read the plan).
        #[arg(long)]
        yes: bool,
        /// Accept an update signed by a different key than the installed one.
        #[arg(long)]
        accept_new_key: bool,
    },
    /// Fetch an installed bundle again from where it came from; asks again
    /// when the new version can do more.
    Update {
        name: String,
        #[arg(long)]
        sha256: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        accept_new_key: bool,
    },
    /// Remove a bundle and the connectors, skills and routines it installed.
    Remove {
        name: String,
        #[arg(long)]
        yes: bool,
    },
    /// Package your own connectors, skills and routines as a bundle to
    /// share, signed with your key when you have one. Secrets never go in.
    Export {
        /// The bundle's name ([a-z0-9-]).
        name: String,
        #[arg(long, default_value = "0.1.0")]
        version: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "")]
        author: String,
        /// A connector to include (repeat for more).
        #[arg(long = "connector")]
        connectors: Vec<String>,
        /// A skill (its directory under skills/) to include.
        #[arg(long = "skill")]
        skills: Vec<String>,
        /// A routine id (sch_…) to include as a template.
        #[arg(long = "routine")]
        routines: Vec<String>,
        /// An agent (its @name) to include as a template, with its routines.
        #[arg(long = "profile")]
        agents: Vec<String>,
        /// Everything of yours: active connectors, skills, and the routines
        /// of --agent, except what came from installed bundles.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        agent: Option<String>,
        /// Where to write it: a .tar.zst (default <name>-<version>.tar.zst)
        /// or, with --dir, a directory.
        #[arg(long, short)]
        output: Option<std::path::PathBuf>,
        #[arg(long)]
        dir: bool,
        /// Don't sign it, even with a key.
        #[arg(long)]
        unsigned: bool,
    },
    /// Make your signing key (bundles/signing.key) for `tern bundle export`.
    Keygen,
    /// Trust a signing key (minisign format, `RW…`, or a .pub file), or
    /// list the trusted keys.
    Trust {
        /// The key, or a path to a .pub file.
        key: Option<String>,
        /// Whose key it is.
        #[arg(long)]
        name: Option<String>,
    },
    /// Stop trusting a key (by key, key id or name).
    Untrust { key: String },
}

#[derive(Debug, Subcommand)]
pub enum ConnectionsCommand {
    /// Every account.
    List,
    /// Sign in to another account (OAuth in your browser, or a key).
    Add {
        /// The connector or MCP server.
        target: String,
        /// The account's short name ([a-z0-9_-], up to 16); made from the
        /// account's email when left out.
        #[arg(long)]
        connection: Option<String>,
        #[arg(long)]
        label: Option<String>,
        /// `connector` or `mcp`, when both have this name.
        #[arg(long)]
        kind: Option<String>,
        /// Enter a key instead of signing in.
        #[arg(long)]
        key: bool,
        /// Sign in with a code on any device instead of a browser here.
        #[arg(long)]
        device: bool,
    },
    /// Sign in again (after a revoked or expired grant).
    Reconnect {
        target: String,
        connection: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        device: bool,
    },
    /// Make an account the default for reads.
    Default {
        target: String,
        connection: String,
        #[arg(long)]
        kind: Option<String>,
    },
    Rename {
        target: String,
        connection: String,
        label: String,
        #[arg(long)]
        kind: Option<String>,
    },
    /// Revoke at the provider (where it allows), and delete the account's
    /// tokens, keys, cache and grants.
    Remove {
        target: String,
        connection: String,
        #[arg(long)]
        kind: Option<String>,
    },
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
