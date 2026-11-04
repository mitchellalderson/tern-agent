//! MCP server management (docs/mcp-management-spec.md): testing, adding,
//! changing and removing servers without a restart. Every write goes
//! through `config.toml` and then [`Runtime::reload_mcp`], which starts
//! added and changed servers and leaves the rest connected.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use anyhow::Context;
use tern_core::config::McpServerConfig;
use tern_core::config::edit::ConfigFile;
use tern_core::mcp::{
    ImportedServer, McpErrorKind, McpPatch, McpStatus, McpVerified, McpVerifyError, check_config,
    check_name,
};
use tern_core::{Config, Secrets, TernHome};

use crate::runtime::Runtime;

/// A write the caller got wrong, as opposed to an I/O failure.
#[derive(Debug)]
pub enum McpError {
    NotFound(String),
    Exists(String),
    Invalid(String),
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "MCP server {name:?} is not configured"),
            Self::Exists(name) => write!(f, "MCP server {name:?} already exists"),
            Self::Invalid(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for McpError {}

/// What MCP writes need, shared by the runtime (API, TUI, CLI) and the
/// agent's `mcp_*` tools.
pub struct McpAdmin {
    pub(crate) home: TernHome,
    pub(crate) live: Arc<RwLock<Config>>,
    pub(crate) secrets: Secrets,
    pub(crate) mcp: Arc<tern_mcp::McpServers>,
    pub(crate) connectors: Arc<tern_lua::Connectors>,
}

impl McpAdmin {
    fn live(&self) -> Config {
        match self.live.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn set_live(&self, config: Config) {
        match self.live.write() {
            Ok(mut g) => *g = config,
            Err(poisoned) => *poisoned.into_inner() = config,
        }
    }

    fn connector_secret_uses(&self) -> Vec<(String, String)> {
        self.connectors.secret_uses()
    }

    /// Whether paired devices may approve or add `stdio` servers.
    pub fn remote_stdio_allowed(&self) -> bool {
        self.live().api.remote_mcp_stdio
    }

    pub fn mcp_statuses(&self) -> Vec<McpStatus> {
        self.mcp.statuses()
    }

    pub fn mcp_status(&self, name: &str) -> Option<McpStatus> {
        self.mcp.status(name)
    }

    /// Start `cfg` as a throwaway server and list its tools. `secrets` are
    /// values typed in a form but not saved yet. Saves nothing.
    pub async fn verify_mcp(
        &self,
        name: &str,
        cfg: &McpServerConfig,
        secrets: BTreeMap<String, String>,
    ) -> Result<McpVerified, McpVerifyError> {
        let invalid = |message: String| McpVerifyError {
            kind: McpErrorKind::Invalid,
            message,
            stderr_tail: Vec::new(),
        };
        check_name(name).map_err(invalid)?;
        check_config(cfg).map_err(invalid)?;
        let secrets = secrets
            .into_iter()
            .map(|(k, v)| (k, v.trim().to_owned()))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        tern_mcp::verify(name, cfg, Arc::new(self.secrets.clone()), secrets).await
    }

    pub fn import_mcp(&self, text: &str) -> Result<Vec<ImportedServer>, String> {
        tern_mcp::import(text)
    }

    /// Add a server (`old` is `None`) or replace `old`, renaming it when
    /// `name` differs. Stores `secrets` first, so the server starts with
    /// them.
    pub fn save_mcp(
        &self,
        old: Option<&str>,
        name: &str,
        cfg: &McpServerConfig,
        secrets: &BTreeMap<String, String>,
    ) -> anyhow::Result<McpStatus> {
        check_name(name).map_err(McpError::Invalid)?;
        check_config(cfg).map_err(McpError::Invalid)?;
        let servers = self.live().mcp.servers;
        match old {
            None if servers.contains_key(name) => return Err(McpError::Exists(name.into()).into()),
            Some(o) if !servers.contains_key(o) => return Err(McpError::NotFound(o.into()).into()),
            Some(o) if o != name && servers.contains_key(name) => {
                return Err(McpError::Exists(name.into()).into());
            }
            _ => {}
        }
        let mut values = Vec::new();
        for (secret, value) in secrets {
            if !cfg.secret_names().any(|n| n == secret) {
                return Err(McpError::Invalid(format!("this server doesn't use {secret}")).into());
            }
            let value = value.trim();
            if !value.is_empty() {
                values.push((secret, value));
            }
        }
        let mut file = ConfigFile::open(&self.home.config_path())?;
        if let Some(o) = old.filter(|o| *o != name) {
            file.remove_mcp_server(o);
        }
        file.upsert_mcp_server(name, cfg)?;
        file.parsed()?;
        for (secret, value) in values {
            self.secrets
                .set(secret, value)
                .with_context(|| format!("storing {secret}"))?;
        }
        file.save()?;
        self.reload_mcp()?;
        tracing::info!(server = %name, "MCP server saved");
        self.mcp_status(name)
            .context("MCP server vanished after saving it")
    }

    /// The quick toggles: enable or disable, hide tools, approval and retry.
    pub fn patch_mcp(&self, name: &str, patch: &McpPatch) -> anyhow::Result<McpStatus> {
        let mut cfg = self
            .live()
            .mcp
            .servers
            .remove(name)
            .ok_or_else(|| McpError::NotFound(name.into()))?;
        if let Some(v) = patch.enabled {
            cfg.enabled = v;
        }
        if let Some(v) = &patch.disabled_tools {
            cfg.disabled_tools = v.clone();
        }
        if let Some(v) = patch.requires_approval {
            cfg.requires_approval = v;
        }
        if let Some(v) = patch.idempotent {
            cfg.idempotent = v;
        }
        let mut file = ConfigFile::open(&self.home.config_path())?;
        file.upsert_mcp_server(name, &cfg)?;
        file.save()?;
        self.reload_mcp()?;
        self.mcp_status(name)
            .context("MCP server vanished after saving it")
    }

    /// Remove a server. With `remove_secrets`, its secrets go too unless a
    /// provider, another server or a connector still uses them.
    pub fn remove_mcp(&self, name: &str, remove_secrets: bool) -> anyhow::Result<()> {
        let cfg = self
            .live()
            .mcp
            .servers
            .remove(name)
            .ok_or_else(|| McpError::NotFound(name.into()))?;
        let mut file = ConfigFile::open(&self.home.config_path())?;
        file.remove_mcp_server(name);
        let saved = file.save()?;
        if remove_secrets {
            let mut used = saved.secret_uses();
            used.extend(self.connector_secret_uses());
            for secret in cfg.secret_names() {
                if !used.iter().any(|(s, _)| s == secret) {
                    self.secrets.remove(secret)?;
                }
            }
        }
        self.reload_mcp()?;
        tracing::info!(server = %name, "MCP server removed");
        Ok(())
    }

    /// Reconnect now, ignoring backoff.
    pub async fn restart_mcp(&self, name: &str) -> anyhow::Result<McpStatus> {
        if self.mcp_status(name).is_none() {
            return Err(McpError::NotFound(name.into()).into());
        }
        self.mcp
            .restart(name)
            .await
            .map_err(|e| McpError::Invalid(e).into())
    }

    /// Re-read `config.toml` and apply its MCP servers: removed ones stop,
    /// added and changed ones start, the rest keep their connection.
    pub fn reload_mcp(&self) -> anyhow::Result<()> {
        let (config, _) = Config::load(&self.home.config_path())?;
        self.mcp.apply(&config.mcp.servers);
        self.set_live(config);
        Ok(())
    }
}

/// The runtime's view, for the API, the TUI and the CLI.
impl Runtime {
    pub fn mcp_statuses(&self) -> Vec<McpStatus> {
        self.mcp_admin.mcp_statuses()
    }

    pub fn mcp_status(&self, name: &str) -> Option<McpStatus> {
        self.mcp_admin.mcp_status(name)
    }

    pub async fn verify_mcp(
        &self,
        name: &str,
        cfg: &McpServerConfig,
        secrets: BTreeMap<String, String>,
    ) -> Result<McpVerified, McpVerifyError> {
        self.mcp_admin.verify_mcp(name, cfg, secrets).await
    }

    pub fn import_mcp(&self, text: &str) -> Result<Vec<ImportedServer>, String> {
        self.mcp_admin.import_mcp(text)
    }

    pub fn save_mcp(
        &self,
        old: Option<&str>,
        name: &str,
        cfg: &McpServerConfig,
        secrets: &BTreeMap<String, String>,
    ) -> anyhow::Result<McpStatus> {
        self.mcp_admin.save_mcp(old, name, cfg, secrets)
    }

    pub fn patch_mcp(&self, name: &str, patch: &McpPatch) -> anyhow::Result<McpStatus> {
        self.mcp_admin.patch_mcp(name, patch)
    }

    pub fn remove_mcp(&self, name: &str, remove_secrets: bool) -> anyhow::Result<()> {
        self.mcp_admin.remove_mcp(name, remove_secrets)
    }

    pub async fn restart_mcp(&self, name: &str) -> anyhow::Result<McpStatus> {
        self.mcp_admin.restart_mcp(name).await
    }

    pub fn reload_mcp(&self) -> anyhow::Result<()> {
        self.mcp_admin.reload_mcp()
    }
}
