//! `config.toml` schema and loading.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::home::expand_tilde;

pub mod edit;

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
            subagents: SubagentsConfig::default(),
            schedules: SchedulesConfig::default(),
            memory: crate::memory::MemoryConfig::default(),
            home: crate::channels::HomeConfig::default(),
            agent_chains: Vec::new(),
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
    /// IP networking for shell commands (both backends).
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
            network: true,
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub servers: BTreeMap<String, McpServerConfig>,
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
        assert_eq!(c.mcp.servers["github"].env_secrets, ["GITHUB_TOKEN"]);
        let m = c.default_model_ref().unwrap().unwrap();
        assert_eq!(m.provider, "anthropic");
        assert_eq!(m.to_string(), "anthropic:claude-sonnet-4-5");
    }

    #[test]
    fn sandbox_settings() {
        let c = Config::parse(
            r#"
[sandbox]
network = false
confine = "required"
read_paths = ["~/.cargo", "/opt/sdk"]
docker_cpus = 1.5
"#,
            Path::new("config.toml"),
        )
        .unwrap();
        assert!(!c.sandbox.network);
        assert_eq!(c.sandbox.confine, ConfineMode::Required);
        assert_eq!(c.sandbox.docker_cpus, 1.5);
        assert_eq!(c.sandbox.docker_pids, 512);
        assert_eq!(c.sandbox.read_paths()[1], PathBuf::from("/opt/sdk"));
        assert!(!c.sandbox.read_paths()[0].starts_with("~"));
        let d = SandboxConfig::default();
        assert!(d.network);
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
