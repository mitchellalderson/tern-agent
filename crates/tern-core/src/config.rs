//! `config.toml` schema and loading.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::home::expand_tilde;

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
    pub default_model: Option<String>,
    pub workspace: String,
    pub max_iterations: u32,
    /// Default model context window in tokens; providers can override it.
    pub context_window: u64,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub sandbox: SandboxConfig,
    pub gateway: GatewayConfig,
    pub mcp: McpConfig,
    pub api: ApiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_model: None,
            workspace: "~/tern-workspace".into(),
            max_iterations: 40,
            context_window: 128_000,
            providers: BTreeMap::new(),
            sandbox: SandboxConfig::default(),
            gateway: GatewayConfig::default(),
            mcp: McpConfig::default(),
            api: ApiConfig::default(),
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
        let mut names: Vec<String> = self
            .providers
            .values()
            .filter_map(|p| p.api_key_secret.clone())
            .collect();
        if let Some(t) = &self.gateway.telegram {
            names.push(t.bot_token_secret.clone());
        }
        for s in self.mcp.servers.values() {
            names.extend(s.env_secrets.iter().cloned());
        }
        names.sort();
        names.dedup();
        names
    }

    pub fn default_model_ref(&self) -> Option<Result<ModelRef, ConfigError>> {
        self.default_model.as_deref().map(str::parse)
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
    pub docker_image: String,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            backend: SandboxBackendKind::Host,
            shell_timeout_secs: 120,
            docker_image: "debian:stable-slim".into(),
        }
    }
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub transport: McpTransport,
    /// Executable for the `stdio` transport.
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Endpoint for the `http` (streamable HTTP) transport.
    pub url: Option<String>,
    /// Secret names exported to the server process environment.
    #[serde(default)]
    pub env_secrets: Vec<String>,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default)]
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
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
    fn shipped_example_parses() {
        let c = Config::parse(crate::defaults::CONFIG_TOML, Path::new("x")).unwrap();
        assert_eq!(c, Config::default());
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
}
