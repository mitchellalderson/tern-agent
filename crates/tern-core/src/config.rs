//! `config.toml` schema and loading.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::home::expand_tilde;

pub mod edit;
mod rate;
pub mod settings;

pub use rate::Rate;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("invalid model reference {0:?}: expected \"provider:model\"")]
    ModelRef(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// `provider:model`, or a fallback chain `["a:x", "b:y"]` (T6a).
    pub default_model: Option<ModelChain>,
    pub workspace: String,
    pub max_iterations: u32,
    /// Default model context window in tokens; providers can override it.
    pub context_window: u64,
    /// How long a `secret_request` prompt waits for the user.
    pub secret_request_timeout_secs: u64,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub sandbox: SandboxConfig,
    pub gateway: GatewayConfig,
    pub mcp: McpConfig,
    pub api: ApiConfig,
    pub webhooks: WebhooksConfig,
    pub subagents: SubagentsConfig,
    pub schedules: SchedulesConfig,
    pub memory: crate::memory::MemoryConfig,
    /// How main channels (Home and each agent's DM) are cut into segments.
    /// `[main]` is accepted too (agent profiles §3).
    #[serde(alias = "main")]
    pub home: crate::channels::HomeConfig,
    /// Agent profiles' model chains (agent profiles §6.3), set by the
    /// runtime from the `agents` table; never read from or written to
    /// config.toml. They join [`Config::model_chains`] after the config's
    /// own, so a config chain with the same head wins.
    #[serde(skip)]
    pub agent_chains: Vec<(String, ModelChain)>,
    /// How much runs may do without asking (tooling spec §5.4): `readonly`,
    /// `supervised` (the default), `autonomous` or `yolo`. A routine can set
    /// its own; it is frozen on each run when the run starts.
    pub autonomy: crate::approval::Autonomy,
    pub outbox: OutboxConfig,
    pub taint: TaintConfig,
    pub connectors: ConnectorsConfig,
    pub auth: AuthConfig,
    pub bundles: BundlesConfig,
}

/// Installing bundles (tooling spec §6.5).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BundlesConfig {
    /// Refuse unsigned bundles instead of installing them with a warning.
    pub require_signed: bool,
    /// Fetch bundle URLs on private or loopback addresses (a server on your
    /// own network). Off: bundles come from the public internet over https.
    pub allow_private_hosts: bool,
}

/// Taint settings (tooling spec §5.3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaintConfig {
    /// In a tainted run, ask before a network read reaches a host the run
    /// hasn't contacted (`ask`), or don't (`allow`). An agent can set its
    /// own.
    pub egress: crate::approval::Egress,
}

/// Luau connectors (tooling spec §6.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectorsConfig {
    /// Requests per connector when its file declares no `rate`.
    pub rate: Rate,
    /// `connector_test` saves a live call as a fixture beside the draft,
    /// and replays it when the same call is tested again (T4c).
    pub record_tests: bool,
    /// Operations `connector_from_openapi` generates when none are named.
    pub openapi_tools: usize,
    /// Most operations one connector generated from OpenAPI may have.
    pub openapi_max_tools: usize,
}

impl Default for ConnectorsConfig {
    fn default() -> Self {
        Self {
            rate: Rate::per_minute(120),
            record_tests: true,
            openapi_tools: 8,
            openapi_max_tools: 40,
        }
    }
}

/// Signing in to services and their connections (tooling spec §6.7–6.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// OAuth apps the user registered, by provider (`google`, or the name
    /// a discovered issuer was given).
    pub clients: BTreeMap<String, OAuthClientConfig>,
    /// OAuth servers tern may reach on private or loopback addresses (a
    /// local or self-hosted identity provider), as `scheme://host[:port]`.
    /// Every other token, metadata or registration endpoint must be public.
    pub local_issuers: Vec<String>,
    /// How long a sign-in waits for the browser (or a device code).
    pub sign_in_timeout_secs: u64,
    /// Accounts per connector or MCP server.
    pub max_connections: u32,
    /// An https URL hosting tern's client ID metadata document, for servers
    /// that accept one instead of registration (empty: register instead).
    pub client_metadata_url: String,
    /// Stop an extra account's stdio MCP server after this long unused.
    pub idle_timeout_secs: u64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            clients: BTreeMap::new(),
            local_issuers: Vec::new(),
            sign_in_timeout_secs: 300,
            max_connections: 10,
            client_metadata_url: String::new(),
            idle_timeout_secs: 600,
        }
    }
}

/// An OAuth app the user registered with a provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthClientConfig {
    pub client_id: String,
    /// The secret holding the app's client secret, for providers that want
    /// one even from desktop apps (Google's "Desktop app" clients).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_secret: Option<String>,
    /// The issuer this app belongs to (`https://login.acme.com`). The app,
    /// and its secret, are only used for sign-ins whose endpoints are on
    /// this origin. Built-in providers (`google`, `slack`…) are bound to
    /// their profile's endpoints without it; any other provider needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
}

/// Deferred approvals (tooling spec §5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutboxConfig {
    /// How long a `write` or `destructive` approval waits for an answer
    /// before it goes to the outbox (at once when no client is attached).
    pub wait_secs: u64,
    /// How long an outbox item waits for the user before it expires.
    pub expire_hours: u64,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            wait_secs: 120,
            expire_hours: 72,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_model: None,
            workspace: "~/tern-workspace".into(),
            max_iterations: 40,
            context_window: 128_000,
            secret_request_timeout_secs: 900,
            providers: BTreeMap::new(),
            sandbox: SandboxConfig::default(),
            gateway: GatewayConfig::default(),
            mcp: McpConfig::default(),
            api: ApiConfig::default(),
            webhooks: WebhooksConfig::default(),
            subagents: SubagentsConfig::default(),
            schedules: SchedulesConfig::default(),
            memory: crate::memory::MemoryConfig::default(),
            home: crate::channels::HomeConfig::default(),
            agent_chains: Vec::new(),
            autonomy: crate::approval::Autonomy::default(),
            outbox: OutboxConfig::default(),
            taint: TaintConfig::default(),
            connectors: ConnectorsConfig::default(),
            auth: AuthConfig::default(),
            bundles: BundlesConfig::default(),
        }
    }
}

impl Config {
    /// Load `path`. A missing file yields the defaults and `false`.
    pub fn load(path: &Path) -> Result<(Self, bool), ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, path).map(|c| (c, true)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((Self::default(), false)),
            Err(source) => Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(e),
        })
    }

    /// Workspace directory with `~` expanded.
    pub fn workspace_dir(&self) -> PathBuf {
        expand_tilde(Path::new(&self.workspace))
    }

    /// Every secret name the config refers to (for redaction).
    pub fn secret_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.secret_uses().into_iter().map(|(n, _)| n).collect();
        names.sort();
        names.dedup();
        names
    }

    /// `(secret, what uses it)` for every secret the config refers to.
    pub fn secret_uses(&self) -> Vec<(String, String)> {
        let mut uses: Vec<(String, String)> = self
            .providers
            .iter()
            .filter_map(|(name, p)| {
                let secret = p.api_key_secret.clone()?;
                Some((secret, format!("provider {name}")))
            })
            .collect();
        if let Some(t) = &self.gateway.telegram {
            uses.push((t.bot_token_secret.clone(), "telegram".into()));
        }
        for (name, s) in &self.mcp.servers {
            uses.extend(
                s.secret_names()
                    .map(|secret| (secret.to_owned(), format!("mcp {name}"))),
            );
        }
        for (name, c) in &self.auth.clients {
            if let Some(secret) = &c.client_secret_secret {
                uses.push((secret.clone(), format!("OAuth app {name}")));
            }
        }
        uses
    }

    pub fn default_model_ref(&self) -> Option<Result<ModelRef, ConfigError>> {
        self.default_model.as_deref().map(str::parse)
    }

    /// Every model setting that may hold a chain, with its config key:
    /// `default_model`, `subagents.default_model`, `subagents.models.<alias>`.
    pub fn model_chains(&self) -> Vec<(String, &ModelChain)> {
        let mut out = Vec::new();
        if let Some(c) = &self.default_model {
            out.push(("default_model".to_owned(), c));
        }
        if let Some(c) = &self.subagents.default_model {
            out.push(("subagents.default_model".to_owned(), c));
        }
        for (alias, c) in &self.subagents.models {
            out.push((format!("subagents.models.{alias}"), c));
        }
        for (slug, c) in &self.agent_chains {
            out.push((format!("agents.{slug}.model"), c));
        }
        out
    }

    /// The fallbacks for `model`: the rest of the first chain (in
    /// [`Config::model_chains`] order) that starts with it. A run on any
    /// model, whatever picked it, gets that model's chain. Entries that
    /// don't parse are skipped here; `tern doctor` reports them.
    pub fn fallbacks_for(&self, model: &ModelRef) -> Vec<ModelRef> {
        let model = model.to_string();
        self.model_chains()
            .into_iter()
            .map(|(_, c)| c)
            .find(|c| c.primary() == model && !c.fallbacks().is_empty())
            .map(|c| {
                c.fallbacks()
                    .iter()
                    .filter_map(|m| m.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One model, or a fallback chain tried in order when a model call fails in
/// a way another provider might not (T6a). Written as `"anthropic:x"` or
/// `["anthropic:x", "openrouter:y"]`; never empty. Derefs to the first
/// entry, the model itself, so code that wants one model reads it as a
/// `&str`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChain(Vec<String>);

impl ModelChain {
    /// `None` for an empty list.
    pub fn new(entries: Vec<String>) -> Option<Self> {
        (!entries.is_empty()).then_some(Self(entries))
    }

    pub fn primary(&self) -> &str {
        &self.0[0]
    }

    pub fn fallbacks(&self) -> &[String] {
        &self.0[1..]
    }

    pub fn entries(&self) -> &[String] {
        &self.0
    }

    /// This chain with `model` first, keeping the other fallbacks: what
    /// picking a new default in `/model` or the app does.
    pub fn with_primary(&self, model: &str) -> Self {
        let mut entries = vec![model.to_owned()];
        entries.extend(self.fallbacks().iter().filter(|m| *m != model).cloned());
        Self(entries)
    }

    /// From `agents.model`: `provider:model`, or a JSON list.
    pub fn from_stored(s: &str) -> Option<Self> {
        if s.trim_start().starts_with('[') {
            serde_json::from_str(s).ok()
        } else {
            Self::new(vec![s.trim().to_owned()]).filter(|c| !c.primary().is_empty())
        }
    }

    /// For `agents.model`: the model, or a JSON list for a chain.
    pub fn to_stored(&self) -> String {
        match self.0.as_slice() {
            [one] => one.clone(),
            many => serde_json::to_string(many).unwrap_or_default(),
        }
    }

    /// The value to write back to `config.toml`: a string for one model.
    pub fn to_toml(&self) -> toml_edit::Value {
        match self.0.as_slice() {
            [one] => one.as_str().into(),
            many => many
                .iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>()
                .into(),
        }
    }
}

impl std::ops::Deref for ModelChain {
    type Target = str;

    fn deref(&self) -> &str {
        self.primary()
    }
}

impl From<&str> for ModelChain {
    fn from(m: &str) -> Self {
        Self(vec![m.to_owned()])
    }
}

impl From<String> for ModelChain {
    fn from(m: String) -> Self {
        Self(vec![m])
    }
}

impl fmt::Display for ModelChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join(" → "))
    }
}

impl Serialize for ModelChain {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => s.serialize_str(one),
            many => many.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for ModelChain {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(String),
            Many(Vec<String>),
        }
        match Raw::deserialize(d)? {
            Raw::One(m) => Ok(Self(vec![m])),
            Raw::Many(v) => Self::new(v).ok_or_else(|| {
                serde::de::Error::custom("a model list needs at least one `provider:model`")
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    pub api_key_secret: Option<String>,
    /// Context window in tokens for this provider's models.
    pub context_window: Option<u64>,
    /// Prices per model id, for budgets. They win over prices the model
    /// list gives.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prices: BTreeMap<String, Pricing>,
}

/// A model's price in USD per million tokens. Cache reads and writes cost
/// the input price unless given.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

impl Pricing {
    /// USD for `usage` tokens: (input, output, cache read, cache write).
    pub fn cost(&self, (input, output, cache_read, cache_write): (u64, u64, u64, u64)) -> f64 {
        let per = |tokens: u64, price: f64| tokens as f64 * price / 1_000_000.0;
        per(input, self.input)
            + per(output, self.output)
            + per(cache_read, self.cache_read.unwrap_or(self.input))
            + per(cache_write, self.cache_write.unwrap_or(self.input))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    OpenaiCompat,
}

impl ProviderConfig {
    pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

    /// Base URL, falling back to the provider kind's public endpoint.
    pub fn effective_base_url(&self) -> Option<&str> {
        match (&self.base_url, self.kind) {
            (Some(url), _) => Some(url.trim_end_matches('/')),
            (None, ProviderKind::Anthropic) => Some(Self::ANTHROPIC_BASE_URL),
            (None, ProviderKind::OpenaiCompat) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxConfig {
    pub backend: SandboxBackendKind,
    pub shell_timeout_secs: u64,
    /// IP networking for every shell command (both backends). Off by
    /// default: a command asks for it with `network: true`, which always
    /// asks the user.
    pub network: bool,
    /// Host backend: what to do when the OS sandbox isn't available.
    pub confine: ConfineMode,
    /// Host backend: extra read-only paths (`~` expands).
    pub read_paths: Vec<String>,
    /// Host backend: extra writable paths (`~` expands).
    pub write_paths: Vec<String>,
    pub docker_image: String,
    /// `docker run --memory`; empty for no limit.
    pub docker_memory: String,
    /// `docker run --cpus`; 0 for no limit.
    pub docker_cpus: f64,
    /// `docker run --pids-limit`; 0 for no limit.
    pub docker_pids: u32,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            backend: SandboxBackendKind::Host,
            shell_timeout_secs: 120,
            network: false,
            confine: ConfineMode::Auto,
            // git refuses to run when it can't read its config.
            read_paths: vec!["~/.gitconfig".into(), "~/.config/git".into()],
            write_paths: Vec::new(),
            docker_image: "debian:stable-slim".into(),
            docker_memory: "2g".into(),
            docker_cpus: 2.0,
            docker_pids: 512,
        }
    }
}

impl SandboxConfig {
    pub fn read_paths(&self) -> Vec<PathBuf> {
        self.read_paths
            .iter()
            .map(|p| expand_tilde(Path::new(p)))
            .collect()
    }

    pub fn write_paths(&self) -> Vec<PathBuf> {
        self.write_paths
            .iter()
            .map(|p| expand_tilde(Path::new(p)))
            .collect()
    }
}

/// `sandbox.confine` (host backend; see `docs/sandbox-design.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfineMode {
    /// Confine when the OS can; otherwise warn and run unconfined.
    #[default]
    Auto,
    /// Refuse to run commands that can't be confined.
    Required,
    /// No OS sandbox.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackendKind {
    Host,
    Docker,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConfig {
    pub telegram: Option<TelegramConfig>,
    /// Gateway connectors by connector name (tooling spec §6.4): Luau
    /// connectors with a `gateway` table, such as ntfy or Slack.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub connectors: BTreeMap<String, GatewayConnectorConfig>,
}

/// `[gateway.connectors.<name>]`: run active connector `<name>` as a chat
/// transport. Delivery targets are `<name>:<chat>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConnectorConfig {
    pub enabled: bool,
    /// How often to poll for messages, in seconds (when the connector can
    /// receive).
    pub poll_secs: u64,
    /// Messages from senders who haven't paired (`tern gateway pair`):
    /// `reply` with a fixed note, `ignore` them, or start a `taint`ed run
    /// that can't write without an approval.
    pub strangers: Strangers,
    /// Handed to the connector (`opts.settings` on send, the third argument
    /// of `poll`): topics, channel ids. Never secret values; those go in
    /// `capabilities.auth`.
    pub settings: BTreeMap<String, toml::Value>,
}

impl Default for GatewayConnectorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_secs: 30,
            strangers: Strangers::Reply,
            settings: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strangers {
    #[default]
    Reply,
    Ignore,
    Taint,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    pub bot_token_secret: String,
    /// Bot API base URL; only changed for tests or self-hosted API servers.
    pub api_url: Option<String>,
}

/// `[api]`: where the daemon's API listens (deployment spec §6.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    /// `"unix"` (always on) plus any `"tcp://host:port"` listeners.
    pub listen: Vec<String>,
    pub tls: TlsMode,
    /// Certificate chain and key (PEM) for `tls = "files"`.
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    /// Honour `X-Forwarded-For` from a reverse proxy on loopback.
    pub trust_proxy: bool,
    /// The URL devices use to reach this core, put in pairing links.
    /// Defaults to the first TCP listener (with this host's name for
    /// `0.0.0.0`).
    pub public_url: Option<String>,
    /// Let paired devices add or test `stdio` MCP servers, which run
    /// commands on this host outside the sandbox.
    pub remote_mcp_stdio: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            listen: vec!["unix".into()],
            tls: TlsMode::Auto,
            tls_cert: None,
            tls_key: None,
            trust_proxy: false,
            public_url: None,
            remote_mcp_stdio: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// A self-signed certificate in `$TERN_HOME/tls/`, pinned by clients.
    Auto,
    /// `tls_cert` and `tls_key`, verified against the usual roots.
    Files,
    /// Plain HTTP: only behind a TLS-terminating proxy on loopback.
    Off,
}

/// `[webhooks]`: the listener webhook routines are reached on (tooling
/// spec §3.1, T1c). Its own port, never the device API's: the two take
/// different credentials and are exposed to different networks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebhooksConfig {
    /// `"tcp://host:port"` listeners. Empty (the default): no webhook can
    /// reach this core; `tern hooks fire` still works locally.
    pub listen: Vec<String>,
    /// TLS on those listeners; unset = `[api] tls`, with its certificate
    /// files. `off` needs loopback listeners and `[api] trust_proxy`.
    pub tls: Option<TlsMode>,
    /// The URL senders use, shown with each hook's path. Defaults to the
    /// first listener (this host's name for `0.0.0.0`).
    pub public_url: Option<String>,
    /// Largest body accepted, in KB.
    pub max_body_kb: u64,
    /// Deliveries one routine accepts per minute; more get 429.
    pub per_minute: u32,
    /// How far a signed timestamp (`X-Tern-Signature: t=`) may be from now.
    pub tolerance_secs: u64,
    /// How long a sender has to send a request's headers (and the TLS
    /// handshake before them), timed from when the connection is waiting
    /// for them: a client that dribbles or idles longer is cut off.
    pub header_timeout_secs: u64,
    /// How long a sender has to send a body once its headers are in; a
    /// slower one gets 408.
    pub body_timeout_secs: u64,
    /// Open connections the listener holds at once, handshakes included;
    /// more are refused (503 without TLS, closed with it).
    pub max_connections: u32,
}

impl Default for WebhooksConfig {
    fn default() -> Self {
        Self {
            listen: Vec::new(),
            tls: None,
            public_url: None,
            max_body_kb: 256,
            per_minute: 30,
            tolerance_secs: 300,
            header_timeout_secs: 10,
            body_timeout_secs: 30,
            max_connections: 64,
        }
    }
}

/// `[subagents]`: delegation to child runs (docs/subagents-design.md §1.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubagentsConfig {
    /// Registers the `agent_*` tools and tells the agent to delegate.
    pub enabled: bool,
    /// Model for sub-agents when `agent_spawn` doesn't name one, as
    /// `provider:model`, or a fallback chain. Unset = the parent's model.
    pub default_model: Option<ModelChain>,
    /// Aliases the parent can pass as `model`, e.g. `fast = "anthropic:…"`;
    /// each may be a fallback chain.
    pub models: BTreeMap<String, ModelChain>,
    /// 1 = children can't spawn; 2 = one level of grandchildren.
    pub max_depth: u32,
    /// Running children per parent run; more are queued.
    pub max_concurrent: u32,
    /// Children per parent run, in total.
    pub max_children: u32,
    /// Running sub-agents across the process; more are queued.
    pub max_concurrent_global: u32,
    /// Per child, unless `agent_spawn` asks for a different number.
    pub max_iterations: u32,
    /// The most `agent_spawn` may ask for (at least `max_iterations`).
    pub max_iterations_ceiling: u32,
    /// Per child (input + output + cache tokens, counted on every model
    /// call), unless `agent_spawn` asks for a different number. 0 = none.
    pub budget_tokens: u64,
    /// The most `agent_spawn` may ask for (at least `budget_tokens`).
    /// 0 = no ceiling.
    pub budget_tokens_ceiling: u64,
    /// How long `agent_ask_parent` waits for an answer.
    pub ask_parent_timeout_secs: u64,
}

impl Default for SubagentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_model: None,
            models: BTreeMap::new(),
            max_depth: 1,
            max_concurrent: 4,
            max_children: 12,
            max_concurrent_global: 8,
            max_iterations: 30,
            max_iterations_ceiling: 100,
            budget_tokens: 400_000,
            budget_tokens_ceiling: 2_000_000,
            ask_parent_timeout_secs: 900,
        }
    }
}

/// `[schedules]`: defaults for every schedule; each can override them
/// (docs/personal-agent-tooling-spec.md §4.1, §8.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulesConfig {
    /// Deliveries finishing inside this window wait for its end.
    pub quiet_hours: crate::quiet::QuietHours,
    /// Runs a schedule may start per local day; then it pauses.
    pub runs_per_day: Option<u32>,
    /// USD a schedule may spend per local day (where prices are known).
    pub usd_per_day: Option<f64>,
    /// Tokens (input + output + cache) one run may use before it is stopped.
    pub tokens_per_run: Option<u64>,
    /// New events one watch poll may start a run each for; the rest share
    /// one batched run.
    pub max_events_per_poll: usize,
    /// How loudly a routine's result reaches its chat, unless the routine
    /// sets its own: `silent`, `digest`, `notify` or `interrupt` (T1b).
    pub initiative: crate::initiative::Initiative,
    /// Every routine one level `quieter` or `louder`, or `vacation` (all
    /// silent); `normal` leaves them as they are.
    pub initiative_dial: crate::initiative::Dial,
    /// Local times when held digest results are sent, as one message.
    pub digest_times: crate::initiative::DigestTimes,
    /// The model that decides whether a `notify` result is worth a push;
    /// unset = the cheapest configured model with a known price.
    pub gate_model: Option<ModelChain>,
}

impl Default for SchedulesConfig {
    fn default() -> Self {
        Self {
            quiet_hours: crate::quiet::QuietHours::default(),
            runs_per_day: None,
            usd_per_day: None,
            tokens_per_run: None,
            max_events_per_poll: 20,
            initiative: crate::initiative::Initiative::default(),
            initiative_dial: crate::initiative::Dial::default(),
            digest_times: crate::initiative::DigestTimes::default(),
            gate_model: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub servers: BTreeMap<String, McpServerConfig>,
    /// `tern mcp serve`: what tern shows other MCP clients (T6d).
    #[serde(skip_serializing_if = "McpServeConfig::is_default")]
    pub serve: McpServeConfig,
}

/// `[mcp.serve]`: which of tern's tools `tern mcp serve` offers MCP
/// clients, and how freely they run. Every call still goes through the
/// effect policy, and starts tainted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpServeConfig {
    /// The agent profile (slug) whose tool scope applies. Unset: only
    /// `memory_search`, `channel_read`, `schedule_list` and `skill_read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Caps the global `autonomy` for MCP calls (it can't raise it):
    /// `readonly` makes the server read-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<crate::approval::Autonomy>,
}

impl McpServeConfig {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub transport: McpTransport,
    /// A disabled server isn't started and adds no tools.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Executable for the `stdio` transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Plain, non-secret environment for `stdio` servers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Working directory for `stdio` servers, `~` expanded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Endpoint for the `http` (streamable HTTP) transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Secret names exported to the server process environment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_secrets: Vec<String>,
    /// `http` only: a secret sent as `Authorization: Bearer <value>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_secret: Option<String>,
    /// `http` only: header name → secret name, e.g. `X-API-Key = "FOO_KEY"`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub header_secrets: BTreeMap<String, String>,
    /// The server's own tool names to hide from the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub idempotent: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub requires_approval: bool,
    /// Per-tool effect overrides, keyed by the server's own tool name (`"*"`
    /// for the rest), e.g. `effects = { search = "read", "*" = "write" }`.
    /// They win over the server's `readOnlyHint` / `destructiveHint`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effects: BTreeMap<String, crate::tool::Effect>,
    /// The server's tools return third-party text (a browser, mail, chat):
    /// their results taint the run (tooling spec §5.3).
    #[serde(default, skip_serializing_if = "is_false")]
    pub untrusted: bool,
}

impl McpServerConfig {
    /// Every secret the server needs: `env_secrets`, then the bearer token,
    /// then header secrets.
    pub fn secret_names(&self) -> impl Iterator<Item = &str> {
        self.env_secrets
            .iter()
            .chain(&self.bearer_token_secret)
            .chain(self.header_secrets.values())
            .map(String::as_str)
    }
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    #[default]
    Stdio,
    Http,
}

/// A `provider:model` reference, e.g. `openrouter:qwen/qwen3-coder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl FromStr for ModelRef {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once(':') {
            Some((p, m)) if !p.is_empty() && !m.is_empty() => Ok(Self {
                provider: p.to_owned(),
                model: m.to_owned(),
            }),
            _ => Err(ConfigError::ModelRef(s.to_owned())),
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = r#"
default_model = "anthropic:claude-sonnet-4-5"
workspace = "~/tern-workspace"
max_iterations = 40

[providers.anthropic]
kind = "anthropic"
api_key_secret = "ANTHROPIC_API_KEY"

[providers.openrouter]
kind = "openai_compat"
base_url = "https://openrouter.ai/api/v1"
api_key_secret = "OPENROUTER_API_KEY"

[providers.ollama]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"

[sandbox]
backend = "host"
shell_timeout_secs = 120
docker_image = "debian:stable-slim"

[gateway.telegram]
enabled = true
bot_token_secret = "TELEGRAM_BOT_TOKEN"

[gateway.connectors.ntfy]
poll_secs = 15
strangers = "taint"
settings = { topic = "tern-abc", reply_topic = "tern-abc-reply" }

[mcp.servers.github]
transport = "stdio"
command = "github-mcp-server"
args = ["stdio"]
env_secrets = ["GITHUB_TOKEN"]
"#;

    #[test]
    fn parses_spec_example() {
        let c = Config::parse(SPEC_EXAMPLE, Path::new("config.toml")).unwrap();
        assert_eq!(c.providers.len(), 3);
        assert_eq!(c.providers["ollama"].kind, ProviderKind::OpenaiCompat);
        assert_eq!(
            c.providers["anthropic"].effective_base_url(),
            Some(ProviderConfig::ANTHROPIC_BASE_URL)
        );
        assert!(c.gateway.telegram.as_ref().unwrap().enabled);
        let ntfy = &c.gateway.connectors["ntfy"];
        assert!(ntfy.enabled);
        assert_eq!((ntfy.poll_secs, ntfy.strangers), (15, Strangers::Taint));
        assert_eq!(ntfy.settings["topic"].as_str(), Some("tern-abc"));
        assert_eq!(c.mcp.servers["github"].env_secrets, ["GITHUB_TOKEN"]);
        let m = c.default_model_ref().unwrap().unwrap();
        assert_eq!(m.provider, "anthropic");
        assert_eq!(m.to_string(), "anthropic:claude-sonnet-4-5");
    }

    #[test]
    fn autonomy_and_outbox_settings() {
        let c = Config::parse(
            "autonomy = \"autonomous\"\n[outbox]\nwait_secs = 30\n",
            Path::new("c"),
        )
        .unwrap();
        assert_eq!(c.autonomy, crate::approval::Autonomy::Autonomous);
        assert_eq!((c.outbox.wait_secs, c.outbox.expire_hours), (30, 72));
        let c = Config::parse("[connectors]\nrate = \"10/sec\"\n", Path::new("x")).unwrap();
        assert_eq!(c.connectors.rate.to_string(), "10/sec");
        assert!(c.connectors.record_tests);
        assert_eq!(
            (c.connectors.openapi_tools, c.connectors.openapi_max_tools),
            (8, 40)
        );
        let c = Config::parse(
            "[connectors]\nrecord_tests = false\nopenapi_tools = 3\nopenapi_max_tools = 12\n",
            Path::new("x"),
        )
        .unwrap();
        assert!(!c.connectors.record_tests);
        assert_eq!(
            (c.connectors.openapi_tools, c.connectors.openapi_max_tools),
            (3, 12)
        );
        assert!(Config::parse("[connectors]\nrate = \"lots\"\n", Path::new("x")).is_err());
        assert_eq!(
            Config::default().autonomy,
            crate::approval::Autonomy::Supervised
        );
        assert!(Config::parse("autonomy = \"reckless\"", Path::new("c")).is_err());
        let c = Config::parse("[taint]\negress = \"allow\"\n", Path::new("c")).unwrap();
        assert_eq!(c.taint.egress, crate::approval::Egress::Allow);
        assert_eq!(Config::default().taint.egress, crate::approval::Egress::Ask);
        assert!(Config::parse("[taint]\negress = \"maybe\"\n", Path::new("c")).is_err());
    }

    #[test]
    fn sandbox_settings() {
        let c = Config::parse(
            r#"
[sandbox]
network = true
confine = "required"
read_paths = ["~/.cargo", "/opt/sdk"]
docker_cpus = 1.5
"#,
            Path::new("config.toml"),
        )
        .unwrap();
        assert!(c.sandbox.network);
        assert_eq!(c.sandbox.confine, ConfineMode::Required);
        assert_eq!(c.sandbox.docker_cpus, 1.5);
        assert_eq!(c.sandbox.docker_pids, 512);
        assert_eq!(c.sandbox.read_paths()[1], PathBuf::from("/opt/sdk"));
        assert!(!c.sandbox.read_paths()[0].starts_with("~"));
        let d = SandboxConfig::default();
        assert!(!d.network, "shell commands get no network unless asked");
        assert_eq!(d.confine, ConfineMode::Auto);
        assert_eq!(d.read_paths, ["~/.gitconfig", "~/.config/git"]);
    }

    #[test]
    fn shipped_example_parses() {
        let c = Config::parse(crate::defaults::CONFIG_TOML, Path::new("x")).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn subagents_table_parses_and_defaults_on() {
        assert!(Config::default().subagents.enabled);
        let c = Config::parse(
            r#"
[subagents]
enabled = false
default_model = "anthropic:claude-sonnet-4-5"
models = { fast = "anthropic:claude-haiku-4-5", strong = "anthropic:claude-opus-4-1" }
max_depth = 1
max_concurrent = 4
max_children = 12
max_concurrent_global = 8
max_iterations = 30
max_iterations_ceiling = 100
budget_tokens = 400000
budget_tokens_ceiling = 2000000
ask_parent_timeout_secs = 900
"#,
            Path::new("c.toml"),
        )
        .unwrap();
        assert!(!c.subagents.enabled);
        assert_eq!(&*c.subagents.models["fast"], "anthropic:claude-haiku-4-5");
        let err = Config::parse("[subagents]\nmax_dept = 2", Path::new("c.toml")).unwrap_err();
        assert!(err.to_string().contains("max_dept"), "{err}");
    }

    #[test]
    fn missing_file_is_default() {
        let tmp = tempfile::tempdir().unwrap();
        let (c, loaded) = Config::load(&tmp.path().join("nope.toml")).unwrap();
        assert!(!loaded);
        assert_eq!(c.max_iterations, 40);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = Config::parse("max_iteratons = 3", Path::new("c.toml")).unwrap_err();
        assert!(err.to_string().contains("max_iteratons"), "{err}");
    }

    #[test]
    fn model_ref_keeps_slashes_and_colons_in_model() {
        let m: ModelRef = "openrouter:qwen/qwen3-coder:free".parse().unwrap();
        assert_eq!(m.provider, "openrouter");
        assert_eq!(m.model, "qwen/qwen3-coder:free");
        assert!("nocolon".parse::<ModelRef>().is_err());
        assert!(":m".parse::<ModelRef>().is_err());
    }

    #[test]
    fn model_chains_accept_a_string_or_a_list() {
        let c = Config::parse(
            r#"
default_model = ["anthropic:a", "openrouter:b", "bad"]
[subagents]
default_model = "anthropic:c"
models = { fast = ["anthropic:h", "openrouter:h"] }
"#,
            Path::new("t"),
        )
        .unwrap();
        let chain = c.default_model.as_ref().unwrap();
        assert_eq!(&**chain, "anthropic:a");
        assert_eq!(chain.fallbacks(), ["openrouter:b", "bad"]);
        let refs = |m: &str| -> Vec<String> {
            c.fallbacks_for(&m.parse().unwrap())
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(refs("anthropic:a"), ["openrouter:b"]);
        assert_eq!(refs("anthropic:h"), ["openrouter:h"]);
        assert!(refs("anthropic:c").is_empty());
        assert!(refs("openrouter:b").is_empty());
        assert_eq!(c.subagents.default_model.as_deref(), Some("anthropic:c"));

        // A single model round-trips as a string, a chain as a list.
        assert_eq!(ModelChain::from("a:x").to_toml().to_string(), "\"a:x\"");
        let moved = chain.with_primary("openrouter:b");
        assert_eq!(moved.entries(), ["openrouter:b", "bad"]);
        assert_eq!(moved.to_toml().to_string(), "[\"openrouter:b\", \"bad\"]");

        let empty = Config::parse("default_model = []", Path::new("t"));
        assert!(empty.is_err());
    }
}
