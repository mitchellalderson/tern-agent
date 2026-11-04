//! MCP client (spec §11, docs/mcp-management-spec.md): configured servers
//! become tools named `mcp__<server>__<tool>`. Servers start on first use
//! (or straight away after a config write) and are restarted with backoff
//! after they crash. The set is live: [`McpServers::apply`] swaps in a new
//! config without touching servers that didn't change.

use std::collections::{BTreeMap, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess};
use serde_json::Value;
use tern_auth::{Conn as Account, Connections, Kind, Resolved};
use tern_core::Secrets;
use tern_core::config::{McpServerConfig, McpTransport};
use tern_core::mcp::{McpErrorKind, McpState, McpStatus, McpToolInfo, McpVerified, McpVerifyError};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult, ToolSource};
use tern_core::types::ToolSpec;
use tokio::io::{AsyncBufReadExt, BufReader};

mod import;
pub use import::import;

/// Environment passed to stdio servers, besides their declared `env_secrets`.
const PASSTHROUGH_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "TERM",
    "TMPDIR",
    "TZ",
    "SYSTEMROOT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PATHEXT",
    "COMSPEC",
];
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// An extra account's client stops after this long unused (`[auth]
/// idle_timeout_secs`).
const DEFAULT_IDLE: Duration = Duration::from_secs(600);
/// Longest tool name every provider accepts.
const MAX_TOOL_NAME: usize = 64;
/// Lines of a stdio server's stderr kept for clients.
const STDERR_LINES: usize = 50;

type Client = RunningService<RoleClient, ()>;

#[derive(Debug, Clone)]
struct RemoteTool {
    remote: String,
    exposed: String,
    description: String,
    schema: Value,
    /// From the server's annotations, before config overrides.
    hinted: Effect,
}

impl RemoteTool {
    fn info(&self, cfg: &McpServerConfig) -> McpToolInfo {
        McpToolInfo {
            exposed: self.exposed.clone(),
            remote: self.remote.clone(),
            description: self.description.clone(),
            disabled: cfg.disabled_tools.contains(&self.remote),
        }
    }
}

/// The effect the server's annotations suggest: `readOnlyHint` → read,
/// else `destructiveHint` → destructive, else write. MCP defaults
/// `destructiveHint` to true when absent; tern only trusts what's stated,
/// so a tool without annotations is `write` (tooling spec §5.1).
pub fn hinted_effect(annotations: Option<&rmcp::model::ToolAnnotations>) -> Effect {
    let hint = |f: fn(&rmcp::model::ToolAnnotations) -> Option<bool>| {
        annotations.and_then(f).unwrap_or(false)
    };
    if hint(|a| a.read_only_hint) {
        Effect::Read
    } else if hint(|a| a.destructive_hint) {
        Effect::Destructive
    } else {
        Effect::Write
    }
}

/// The effect of server tool `remote`: a config override (by name, then
/// `"*"`) wins over the server's hint.
pub fn effect_for(cfg: &McpServerConfig, remote: &str, hinted: Effect) -> Effect {
    cfg.effects
        .get(remote)
        .or_else(|| cfg.effects.get("*"))
        .copied()
        .unwrap_or(hinted)
}

/// Called with a server's name and new state (`None` once it's removed).
pub type Notify = Arc<dyn Fn(&str, Option<McpState>) + Send + Sync>;

#[derive(Default)]
struct Conn {
    client: Option<Client>,
    failures: u32,
    retry_at: Option<Instant>,
    last_error: Option<String>,
}

/// Why a connection attempt failed.
struct ConnErr {
    kind: McpErrorKind,
    message: String,
}

impl ConnErr {
    fn new(kind: McpErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

type Connected = (Client, Vec<RemoteTool>, Option<String>);

pub struct McpServer {
    name: String,
    cfg: McpServerConfig,
    secrets: Arc<Secrets>,
    /// Secret values typed in a form but not saved yet (verify only).
    overrides: BTreeMap<String, String>,
    backoff_base: Duration,
    notify: Option<Notify>,
    conn: tokio::sync::Mutex<Conn>,
    tools: RwLock<Vec<RemoteTool>>,
    state: RwLock<McpState>,
    server_info: RwLock<Option<String>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// The account this instance signs in as (tooling spec §6.8): `None`
    /// for the server's own config with plain secret names.
    account: Option<(Arc<Connections>, Resolved)>,
    last_used: Mutex<Instant>,
}

impl McpServer {
    fn build(name: &str, cfg: McpServerConfig, secrets: Arc<Secrets>) -> Self {
        let state = if cfg.enabled {
            McpState::NotStarted
        } else {
            McpState::Disabled
        };
        Self {
            name: name.to_owned(),
            cfg,
            secrets,
            overrides: BTreeMap::new(),
            backoff_base: Duration::from_secs(1),
            notify: None,
            conn: tokio::sync::Mutex::new(Conn::default()),
            tools: RwLock::new(Vec::new()),
            state: RwLock::new(state),
            server_info: RwLock::new(None),
            stderr: Arc::default(),
            account: None,
            last_used: Mutex::new(Instant::now()),
        }
    }

    pub fn new(name: &str, cfg: McpServerConfig, secrets: Arc<Secrets>) -> Arc<Self> {
        Arc::new(Self::build(name, cfg, secrets))
    }

    /// Like [`McpServer::new`] with a custom backoff (for tests).
    pub fn with_backoff(
        name: &str,
        cfg: McpServerConfig,
        secrets: Arc<Secrets>,
        base: Duration,
    ) -> Arc<Self> {
        let mut s = Self::build(name, cfg, secrets);
        s.backoff_base = base;
        Arc::new(s)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn config(&self) -> &McpServerConfig {
        &self.cfg
    }

    fn set_state(&self, s: McpState) {
        if let Ok(mut g) = self.state.write() {
            if *g == s {
                return;
            }
            *g = s.clone();
        }
        if let Some(n) = &self.notify {
            n(&self.name, Some(s));
        }
    }

    pub fn state(&self) -> McpState {
        self.state
            .read()
            .map(|s| s.clone())
            .unwrap_or(McpState::NotStarted)
    }

    /// The state as one line, as the system prompt and `/tools` show it.
    pub fn status(&self) -> String {
        match self.state() {
            McpState::Disabled => "disabled".into(),
            McpState::NotStarted => "not started".into(),
            McpState::Connecting => "connecting".into(),
            McpState::Running { tools, .. } => format!("{tools} tools"),
            McpState::Unavailable { error, .. } => format!("unavailable: {error}"),
        }
    }

    /// A declared secret, for this instance's account
    /// (`conn/mcp:<server>/<slug>/<NAME>`, the plain name for the default).
    fn secret(&self, name: &str) -> Option<String> {
        self.overrides.get(name).cloned().or_else(|| {
            self.secrets.get(&Connections::secret_name(
                self.account.as_ref().map(|(_, r)| r),
                name,
            ))
        })
    }

    pub fn missing_secrets(&self) -> Vec<String> {
        self.cfg
            .secret_names()
            .filter(|n| self.secret(n).is_none())
            .map(str::to_owned)
            .collect()
    }

    fn stderr_tail(&self) -> Vec<String> {
        self.stderr
            .lock()
            .map(|l| l.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn tool_infos(&self) -> Vec<McpToolInfo> {
        self.tools
            .read()
            .map(|t| t.iter().map(|t| t.info(&self.cfg)).collect())
            .unwrap_or_default()
    }

    /// Everything clients show about this server.
    pub fn snapshot(&self) -> McpStatus {
        McpStatus {
            name: self.name.clone(),
            config: self.cfg.clone(),
            state: self.state(),
            tools: self.tool_infos(),
            missing_secrets: self.missing_secrets(),
            server_info: self.server_info.read().ok().and_then(|s| s.clone()),
            stderr_tail: self.stderr_tail(),
        }
    }

    /// Connect if not connected (respecting backoff) and refresh the tool list.
    pub async fn ensure(&self) -> Result<(), String> {
        let mut conn = self.conn.lock().await;
        self.ensure_locked(&mut conn).await
    }

    /// Drop the connection, forget the backoff and connect again now.
    pub async fn restart(&self) -> McpStatus {
        let mut conn = self.conn.lock().await;
        *conn = Conn::default();
        if let Err(e) = self.ensure_locked(&mut conn).await {
            tracing::warn!("{e}");
        }
        drop(conn);
        self.snapshot()
    }

    async fn ensure_locked(&self, conn: &mut Conn) -> Result<(), String> {
        if !self.cfg.enabled {
            return Err(format!("MCP server {} is disabled", self.name));
        }
        if conn
            .client
            .as_ref()
            .is_some_and(|c| !c.is_closed() && !c.is_transport_closed())
        {
            return Ok(());
        }
        conn.client = None;
        if let Some(at) = conn.retry_at
            && Instant::now() < at
        {
            let wait = at.saturating_duration_since(Instant::now()).as_secs_f64();
            return Err(format!(
                "MCP server {} is unavailable ({}); retrying in {wait:.0}s",
                self.name,
                conn.last_error.as_deref().unwrap_or("not connected")
            ));
        }
        self.set_state(McpState::Connecting);
        match self.connect_timed().await {
            Ok((client, tools, info)) => {
                let visible = tools
                    .iter()
                    .filter(|t| !self.cfg.disabled_tools.contains(&t.remote))
                    .count();
                tracing::info!(server = %self.name, tools = visible, "MCP server connected");
                if let Ok(mut t) = self.tools.write() {
                    *t = tools;
                }
                if let Ok(mut s) = self.server_info.write() {
                    *s = info;
                }
                conn.client = Some(client);
                conn.failures = 0;
                conn.retry_at = None;
                conn.last_error = None;
                self.set_state(McpState::Running {
                    tools: visible,
                    since: chrono::Utc::now().timestamp(),
                });
                Ok(())
            }
            Err(e) => {
                self.fail(conn, e.message.clone());
                Err(format!(
                    "MCP server {} failed to start: {}",
                    self.name, e.message
                ))
            }
        }
    }

    fn fail(&self, conn: &mut Conn, error: String) {
        conn.client = None;
        conn.failures += 1;
        let delay = self
            .backoff_base
            .saturating_mul(1u32 << (conn.failures - 1).min(10))
            .min(MAX_BACKOFF);
        conn.retry_at = Some(Instant::now() + delay);
        tracing::warn!(server = %self.name, %error, retry_in = ?delay, "MCP server unavailable");
        let retry_at = chrono::Utc::now()
            + chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::zero());
        self.set_state(McpState::Unavailable {
            error: error.clone(),
            retry_at: Some(retry_at.timestamp()),
            failures: conn.failures,
        });
        conn.last_error = Some(error);
    }

    async fn connect_timed(&self) -> Result<Connected, ConnErr> {
        tokio::time::timeout(CONNECT_TIMEOUT, self.connect())
            .await
            .unwrap_or_else(|_| {
                Err(ConnErr::new(
                    McpErrorKind::Timeout,
                    format!("timed out after {}s", CONNECT_TIMEOUT.as_secs()),
                ))
            })
    }

    async fn connect(&self) -> Result<Connected, ConnErr> {
        let client = match self.cfg.transport {
            McpTransport::Stdio => self.connect_stdio().await?,
            McpTransport::Http => self.connect_http().await?,
        };
        let info = client
            .peer_info()
            .and_then(|i| i.server_info.clone())
            .map(|i| format!("{} {}", i.name, i.version));
        let tools = client
            .list_all_tools()
            .await
            .map_err(|e| ConnErr::new(McpErrorKind::Protocol, format!("listing tools: {e}")))?;
        let tools = tools
            .into_iter()
            .map(|t| RemoteTool {
                exposed: exposed_name(&self.name, &t.name),
                remote: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                schema: Value::Object((*t.input_schema).clone()),
                hinted: hinted_effect(t.annotations.as_ref()),
            })
            .collect();
        Ok((client, tools, info))
    }

    async fn connect_stdio(&self) -> Result<Client, ConnErr> {
        let command = self.cfg.command.as_deref().ok_or_else(|| {
            ConnErr::new(McpErrorKind::Invalid, "stdio transport needs `command`")
        })?;
        let mut env = Vec::new();
        for key in &self.cfg.env_secrets {
            let value = self.secret(key).ok_or_else(|| {
                ConnErr::new(
                    McpErrorKind::MissingSecret,
                    format!("secret {key} is not set"),
                )
            })?;
            env.push((key.clone(), value));
        }
        let cwd = self.cfg.cwd.as_deref().map(expand_tilde);
        let cmd = tokio::process::Command::new(command).configure(|c| {
            c.args(&self.cfg.args)
                .env_clear()
                .envs(
                    PASSTHROUGH_ENV
                        .iter()
                        .filter_map(|k| std::env::var_os(k).map(|v| (*k, v))),
                )
                .envs(&self.cfg.env)
                .envs(env);
            if let Some(dir) = &cwd {
                c.current_dir(dir);
            }
        });
        let (transport, stderr) = TokioChildProcess::builder(cmd)
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                let kind = if e.kind() == std::io::ErrorKind::NotFound {
                    McpErrorKind::CommandNotFound
                } else {
                    McpErrorKind::SpawnFailed
                };
                ConnErr::new(kind, format!("spawning `{command}`: {e}"))
            })?;
        if let Ok(mut l) = self.stderr.lock() {
            l.clear();
        }
        let reader = stderr.map(|stderr| {
            let (name, secrets, ring) =
                (self.name.clone(), self.secrets.clone(), self.stderr.clone());
            let overrides: Vec<String> = self.overrides.values().cloned().collect();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let line = tern_core::secrets::redact_with(&overrides, &secrets.redact(&line));
                    tracing::debug!(server = %name, "stderr: {line}");
                    if let Ok(mut ring) = ring.lock() {
                        if ring.len() == STDERR_LINES {
                            ring.pop_front();
                        }
                        ring.push_back(line);
                    }
                }
            })
        });
        match ().serve(transport).await {
            Ok(c) => Ok(c),
            Err(e) => {
                // The child closing stdout usually means it exited; give its
                // stderr a moment to drain so the reason is in the message.
                let exited = match reader {
                    Some(r) => tokio::time::timeout(Duration::from_secs(1), r)
                        .await
                        .is_ok(),
                    None => false,
                };
                let last = self
                    .stderr_tail()
                    .into_iter()
                    .rev()
                    .find(|l| !l.trim().is_empty());
                let message = match last {
                    Some(l) => format!("{e} ({l})"),
                    None => e.to_string(),
                };
                let kind = if exited {
                    McpErrorKind::ExitedEarly
                } else {
                    McpErrorKind::Protocol
                };
                Err(ConnErr::new(kind, message))
            }
        }
    }

    async fn connect_http(&self) -> Result<Client, ConnErr> {
        let url = self
            .cfg
            .url
            .as_deref()
            .ok_or_else(|| ConnErr::new(McpErrorKind::Invalid, "http transport needs `url`"))?;
        let mut headers = reqwest::header::HeaderMap::new();
        let secret_header = |header: &str, secret: &str, prefix: &str| {
            let value = self.secret(secret).ok_or_else(|| {
                ConnErr::new(
                    McpErrorKind::MissingSecret,
                    format!("secret {secret} is not set"),
                )
            })?;
            let name = reqwest::header::HeaderName::from_bytes(header.as_bytes()).map_err(|e| {
                ConnErr::new(McpErrorKind::Invalid, format!("header {header:?}: {e}"))
            })?;
            let mut value = reqwest::header::HeaderValue::from_str(&format!("{prefix}{value}"))
                .map_err(|_| {
                    ConnErr::new(
                        McpErrorKind::Invalid,
                        format!("secret {secret} isn't a valid header value"),
                    )
                })?;
            value.set_sensitive(true);
            Ok::<_, ConnErr>((name, value))
        };
        if let Some(secret) = &self.cfg.bearer_token_secret {
            let (k, v) = secret_header("authorization", secret, "Bearer ")?;
            headers.insert(k, v);
        }
        for (header, secret) in &self.cfg.header_secrets {
            let (k, v) = secret_header(header, secret, "")?;
            headers.insert(k, v);
        }
        // An OAuth account: its access token, refreshed once if the server
        // refuses it.
        let oauth = self
            .account
            .as_ref()
            .filter(|(_, r)| r.conn.auth.oauth().is_some());
        let Some((conns, r)) = oauth else {
            return self.serve_http(url, headers).await;
        };
        let token = conns
            .access_token(r, None)
            .await
            .map_err(|e| ConnErr::new(McpErrorKind::Unauthorized, e))?;
        let bearer = |t: &str| {
            let mut v =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {t}")).map_err(|_| {
                    ConnErr::new(
                        McpErrorKind::Invalid,
                        "the access token isn't a valid header value",
                    )
                })?;
            v.set_sensitive(true);
            Ok::<_, ConnErr>(v)
        };
        let mut first = headers.clone();
        first.insert(reqwest::header::AUTHORIZATION, bearer(&token)?);
        match self.serve_http(url, first).await {
            Err(e) if e.kind == McpErrorKind::Unauthorized => {
                let fresh = conns
                    .access_token(r, Some(&token))
                    .await
                    .map_err(|e| ConnErr::new(McpErrorKind::Unauthorized, e))?;
                headers.insert(reqwest::header::AUTHORIZATION, bearer(&fresh)?);
                self.serve_http(url, headers).await
            }
            other => other,
        }
    }

    async fn serve_http(
        &self,
        url: &str,
        headers: reqwest::header::HeaderMap,
    ) -> Result<Client, ConnErr> {
        let client = tern_core::net::http_client()
            .default_headers(headers)
            .build()
            .map_err(|e| ConnErr::new(McpErrorKind::Invalid, e.to_string()))?;
        let transport = StreamableHttpClientTransport::with_client(
            client,
            StreamableHttpClientTransportConfig::with_uri(url.to_owned()),
        );
        ().serve(transport).await.map_err(|e| {
            let message = e.to_string();
            let lower = message.to_ascii_lowercase();
            let kind = if ["401", "403", "unauthorized", "forbidden"]
                .iter()
                .any(|w| lower.contains(w))
            {
                McpErrorKind::Unauthorized
            } else if [
                "error sending request",
                "connect",
                "dns",
                "refused",
                "resolve",
            ]
            .iter()
            .any(|w| lower.contains(w))
            {
                McpErrorKind::Unreachable
            } else {
                McpErrorKind::Protocol
            };
            ConnErr::new(kind, message)
        })
    }

    pub async fn call(&self, remote: &str, args: Value) -> ToolResult {
        if let Ok(mut t) = self.last_used.lock() {
            *t = Instant::now();
        }
        let mut conn = self.conn.lock().await;
        if let Err(e) = self.ensure_locked(&mut conn).await {
            return ToolResult::error(e);
        }
        let Some(client) = conn.client.as_ref() else {
            return ToolResult::error(format!("MCP server {} is not connected", self.name));
        };
        let mut params = CallToolRequestParams::new(remote.to_owned());
        params.arguments = match args {
            Value::Object(m) => Some(m),
            Value::Null => None,
            other => return ToolResult::error(format!("arguments must be an object, got {other}")),
        };
        match tokio::time::timeout(CALL_TIMEOUT, client.call_tool(params)).await {
            Err(_) => ToolResult::error(format!(
                "MCP call timed out after {}s",
                CALL_TIMEOUT.as_secs()
            )),
            Ok(Ok(result)) => convert(result),
            Ok(Err(e)) => {
                let lost = matches!(
                    e,
                    ServiceError::TransportClosed | ServiceError::TransportSend(_)
                ) || client.is_closed()
                    || client.is_transport_closed();
                if lost {
                    self.fail(&mut conn, format!("connection lost: {e}"));
                    return ToolResult::error(format!(
                        "MCP server {} disconnected during the call ({e}); it will be restarted. \
                         The call may or may not have taken effect.",
                        self.name
                    ));
                }
                ToolResult::error(format!("MCP error: {e}"))
            }
        }
    }
}

/// Start a throwaway server from `cfg`, list its tools and shut it down.
/// `overrides` are secret values typed in a form but not saved yet. Never
/// touches the live set or the config.
pub async fn verify(
    name: &str,
    cfg: &McpServerConfig,
    secrets: Arc<Secrets>,
    overrides: BTreeMap<String, String>,
) -> Result<McpVerified, McpVerifyError> {
    let mut cfg = cfg.clone();
    cfg.enabled = true;
    let mut server = McpServer::build(name, cfg, secrets);
    server.overrides = overrides;
    if let Some(missing) = server.missing_secrets().first() {
        return Err(McpVerifyError {
            kind: McpErrorKind::MissingSecret,
            message: format!("secret {missing} is not set"),
            stderr_tail: Vec::new(),
        });
    }
    match server.connect_timed().await {
        Ok((client, tools, server_info)) => {
            if let Err(e) = client.cancel().await {
                tracing::debug!(server = %name, "closing test connection: {e}");
            }
            Ok(McpVerified {
                tools: tools.iter().map(|t| t.info(&server.cfg)).collect(),
                server_info,
            })
        }
        Err(e) => Err(McpVerifyError {
            kind: e.kind,
            message: e.message,
            stderr_tail: server.stderr_tail(),
        }),
    }
}

fn expand_tilde(path: &str) -> std::path::PathBuf {
    match (
        path.strip_prefix("~/").or((path == "~").then_some("")),
        std::env::var_os("HOME"),
    ) {
        (Some(rest), Some(home)) => std::path::PathBuf::from(home).join(rest),
        _ => path.into(),
    }
}

fn convert(result: CallToolResult) -> ToolResult {
    let mut parts: Vec<String> = result
        .content
        .iter()
        .map(|c| match c {
            ContentBlock::Text(t) => t.text.clone(),
            other => {
                let v = serde_json::to_value(other).unwrap_or(Value::Null);
                let kind = v.get("type").and_then(Value::as_str).unwrap_or("content");
                match kind {
                    "image" | "audio" => format!(
                        "[{kind} content: {}]",
                        v.get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown type")
                    ),
                    _ => v.to_string(),
                }
            }
        })
        .collect();
    if parts.is_empty()
        && let Some(s) = &result.structured_content
    {
        parts.push(s.to_string());
    }
    ToolResult {
        content: parts.join("\n"),
        is_error: result.is_error.unwrap_or(false),
        untrusted: false,
    }
}

/// `mcp__<server>__<tool>`, restricted to `[A-Za-z0-9_-]` and at most 64
/// characters (long names keep a stable hash suffix so they stay unique).
pub fn exposed_name(server: &str, tool: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let full = format!("mcp__{}__{}", clean(server), clean(tool));
    if full.len() <= MAX_TOOL_NAME {
        return full;
    }
    // FNV-1a: stable across Rust versions, unlike DefaultHasher.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in format!("{server}/{tool}").bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{}_{:08x}", &full[..MAX_TOOL_NAME - 9], h as u32)
}

struct McpTool {
    server: Arc<McpServer>,
    tool: RemoteTool,
    accounts: Arc<Accounts>,
}

impl McpTool {
    fn routed(&self) -> Option<Vec<Account>> {
        self.accounts
            .conns
            .as_ref()?
            .routed(Kind::Mcp, &self.server.name)
    }

    /// The account a call runs under, with the parameter taken out.
    fn route(&self, args: &mut Value) -> Result<Option<Resolved>, String> {
        let Some(conns) = &self.accounts.conns else {
            return Ok(None);
        };
        let asked = tern_auth::connections::take_param(&self.tool.schema, args);
        conns.resolve(
            Kind::Mcp,
            &self.server.name,
            asked.as_deref(),
            self.effect(),
        )
    }
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        let input_schema = match self.routed() {
            Some(conns) => tern_auth::connections::with_param(&self.tool.schema, &conns),
            None => self.tool.schema.clone(),
        };
        ToolSpec {
            name: self.tool.exposed.clone(),
            description: format!(
                "{} (MCP server {})",
                self.tool.description, self.server.name
            ),
            input_schema,
        }
    }

    fn effect(&self) -> Effect {
        effect_for(&self.server.cfg, &self.tool.remote, self.tool.hinted)
    }

    fn untrusted(&self) -> bool {
        self.server.cfg.untrusted
    }

    fn policy(&self, _: &ToolCtx, args: &Value) -> ToolPolicy {
        // Refused for want of an account: nothing is sent, nobody asked.
        if self.route(&mut args.clone()).is_err() {
            return ToolPolicy::new(Effect::Read, true);
        }
        ToolPolicy::new(self.effect(), self.server.cfg.idempotent)
            .forced(self.server.cfg.requires_approval)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        self.routed()?;
        let r = self.route(&mut args.clone()).ok()??;
        Some(format!("{} as {}", self.tool.exposed, r.conn.title()))
    }

    async fn call(&self, ctx: &ToolCtx, mut args: Value) -> ToolResult {
        let server = match self.route(&mut args) {
            Ok(r) => self.accounts.instance(&self.server, r.as_ref()),
            Err(e) => return ToolResult::error(format!("{}: {e}", self.tool.exposed)),
        };
        tokio::select! {
            r = server.call(&self.tool.remote, args) => r,
            () = ctx.cancel.cancelled() => ToolResult::error("cancelled"),
        }
    }
}

/// Clients by (server, slug), with the connection each was made for.
type Instances = BTreeMap<(String, String), (Account, Arc<McpServer>)>;

/// Every account's client of every server (tooling spec §6.8.1): one MCP
/// session per connection, started on first use; a stdio server is one
/// process per connection, stopped when idle.
struct Accounts {
    conns: Option<Arc<Connections>>,
    secrets: Arc<Secrets>,
    /// By (server, slug), with the connection they were made for.
    instances: Mutex<Instances>,
    idle: Duration,
}

impl Accounts {
    /// The client for `r` of `base` (`None`: the server's own config).
    fn instance(&self, base: &Arc<McpServer>, r: Option<&Resolved>) -> Arc<McpServer> {
        let (Some(conns), Some(r)) = (&self.conns, r) else {
            return base.clone();
        };
        let mut m = self.instances.lock().unwrap_or_else(|e| e.into_inner());
        let key = (base.name.clone(), r.conn.slug.clone());
        if let Some((made_for, s)) = m.get(&key)
            && same_account(made_for, &r.conn)
            && s.cfg == base.cfg
        {
            return s.clone();
        }
        let mut s = McpServer::build(&base.name, base.cfg.clone(), self.secrets.clone());
        s.account = Some((conns.clone(), r.clone()));
        s.notify = None;
        let s = Arc::new(s);
        m.insert(key, (r.conn.clone(), s.clone()));
        s
    }

    /// The instance whose tools are listed: the default account's, once a
    /// server has accounts.
    fn listing(&self, base: &Arc<McpServer>) -> Arc<McpServer> {
        let Some(conns) = &self.conns else {
            return base.clone();
        };
        let list = conns.list(Kind::Mcp, &base.name);
        match list.iter().find(|c| c.is_default).or(list.first()) {
            Some(c) => self.instance(base, Some(&Resolved { conn: c.clone() })),
            None => base.clone(),
        }
    }

    /// Stop accounts' clients unused for the idle timeout (the listing one
    /// stays), and forget instances of removed connections.
    fn reap(&self) {
        let Some(conns) = &self.conns else { return };
        let mut m = self.instances.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|(server, slug), (made_for, _)| {
            conns
                .get(Kind::Mcp, server, slug)
                .is_some_and(|c| same_account(made_for, &c))
        });
        for (made_for, s) in m.values() {
            let idle = s.last_used.lock().map(|t| t.elapsed()).unwrap_or_default();
            if !made_for.is_default
                && idle >= self.idle
                && let Ok(mut c) = s.conn.try_lock()
                && c.client.is_some()
            {
                tracing::info!(server = %s.name, connection = %made_for.slug, "stopping an idle MCP connection");
                *c = Conn::default();
                s.set_state(McpState::NotStarted);
            }
        }
    }
}

/// Same connection, same credentials: a re-sign-in or new status makes a
/// fresh client.
fn same_account(a: &Account, b: &Account) -> bool {
    a.id == b.id && a.auth == b.auth && a.status == b.status && a.account == b.account
}

/// All configured MCP servers as one tool source.
pub struct McpServers {
    servers: RwLock<BTreeMap<String, Arc<McpServer>>>,
    secrets: Arc<Secrets>,
    notify: Option<Notify>,
    accounts: Arc<Accounts>,
}

impl McpServers {
    pub fn new(servers: Vec<Arc<McpServer>>) -> Self {
        let secrets = servers
            .first()
            .map(|s| s.secrets.clone())
            .unwrap_or_default();
        Self {
            servers: RwLock::new(servers.into_iter().map(|s| (s.name.clone(), s)).collect()),
            accounts: Arc::new(Accounts {
                conns: None,
                secrets: secrets.clone(),
                instances: Mutex::default(),
                idle: DEFAULT_IDLE,
            }),
            secrets,
            notify: None,
        }
    }

    /// Route calls to accounts (tooling spec §6.8). Extra accounts' clients
    /// stop after `idle` unused.
    pub fn with_connections(mut self, conns: Arc<Connections>, idle: Duration) -> Self {
        self.accounts = Arc::new(Accounts {
            conns: Some(conns),
            secrets: self.secrets.clone(),
            instances: Mutex::default(),
            idle,
        });
        self
    }

    /// Servers for `cfg`, none started. `notify` hears every state change.
    pub fn from_config(
        cfg: &BTreeMap<String, McpServerConfig>,
        secrets: Arc<Secrets>,
        notify: Option<Notify>,
    ) -> Self {
        let set = Self {
            servers: RwLock::default(),
            accounts: Arc::new(Accounts {
                conns: None,
                secrets: secrets.clone(),
                instances: Mutex::default(),
                idle: DEFAULT_IDLE,
            }),
            secrets,
            notify,
        };
        let servers = cfg
            .iter()
            .map(|(name, c)| (name.clone(), set.make(name, c)))
            .collect();
        if let Ok(mut s) = set.servers.write() {
            *s = servers;
        }
        set
    }

    fn make(&self, name: &str, cfg: &McpServerConfig) -> Arc<McpServer> {
        let mut s = McpServer::build(name, cfg.clone(), self.secrets.clone());
        s.notify = self.notify.clone();
        Arc::new(s)
    }

    pub fn servers(&self) -> Vec<Arc<McpServer>> {
        self.servers
            .read()
            .map(|s| s.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get(&self, name: &str) -> Option<Arc<McpServer>> {
        self.servers.read().ok()?.get(name).cloned()
    }

    /// Swap in a new config. Removed servers leave the set (their process
    /// is killed once calls in flight finish), changed ones are replaced,
    /// unchanged ones keep their connection. Added and changed servers that
    /// are enabled start in the background straight away.
    pub fn apply(&self, cfg: &BTreeMap<String, McpServerConfig>) {
        let mut started = Vec::new();
        let mut removed = Vec::new();
        {
            let Ok(mut servers) = self.servers.write() else {
                return;
            };
            servers.retain(|name, _| {
                let keep = cfg.contains_key(name);
                if !keep {
                    removed.push(name.clone());
                }
                keep
            });
            for (name, c) in cfg {
                if servers.get(name).is_some_and(|s| s.cfg == *c) {
                    continue;
                }
                let s = self.make(name, c);
                if let Some(n) = &self.notify {
                    n(name, Some(s.state()));
                }
                if c.enabled {
                    started.push(s.clone());
                }
                servers.insert(name.clone(), s);
            }
        }
        if let Some(n) = &self.notify {
            for name in &removed {
                n(name, None);
            }
        }
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            for s in started {
                rt.spawn(async move {
                    if let Err(e) = s.ensure().await {
                        tracing::warn!("{e}");
                    }
                });
            }
        }
    }

    pub fn statuses(&self) -> Vec<McpStatus> {
        self.servers().iter().map(|s| s.snapshot()).collect()
    }

    pub fn status(&self, name: &str) -> Option<McpStatus> {
        self.get(name).map(|s| s.snapshot())
    }

    /// Reconnect `name` now, ignoring backoff.
    pub async fn restart(&self, name: &str) -> Result<McpStatus, String> {
        let s = self
            .get(name)
            .ok_or_else(|| format!("MCP server {name:?} is not configured"))?;
        if !s.cfg.enabled {
            return Err(format!("MCP server {name} is disabled"));
        }
        Ok(s.restart().await)
    }
}

#[async_trait]
impl ToolSource for McpServers {
    async fn prepare(&self) {
        if let Some(c) = &self.accounts.conns {
            c.reload();
        }
        self.accounts.reap();
        let servers: Vec<Arc<McpServer>> = self
            .servers()
            .iter()
            .filter(|s| s.cfg.enabled)
            .map(|s| self.accounts.listing(s))
            .collect();
        let starts = servers.iter().map(|s| async move {
            if let Err(e) = s.ensure().await {
                tracing::warn!("{e}");
            }
        });
        futures::future::join_all(starts).await;
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.servers()
            .into_iter()
            .filter(|s| s.cfg.enabled)
            .flat_map(|s| {
                let listing = self.accounts.listing(&s);
                let tools = listing.tools.read().map(|t| t.clone()).unwrap_or_default();
                tools
                    .into_iter()
                    .filter(|t| !s.cfg.disabled_tools.contains(&t.remote))
                    .map(|tool| {
                        Arc::new(McpTool {
                            server: s.clone(),
                            tool,
                            accounts: self.accounts.clone(),
                        }) as Arc<dyn Tool>
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn describe(&self) -> Vec<String> {
        self.servers()
            .iter()
            .map(|s| self.accounts.listing(s))
            .filter_map(|s| {
                let accounts = self
                    .accounts
                    .conns
                    .as_ref()
                    .and_then(|c| c.describe(Kind::Mcp, &s.name))
                    .map(|a| format!("; accounts: {a}"))
                    .unwrap_or_default();
                Some(format!("{}{accounts}", describe_one(&s)?))
            })
            .collect()
    }
}

fn describe_one(s: &McpServer) -> Option<String> {
    match s.state() {
        McpState::Disabled => None,
        // Servers start on a run's first use, so "not started" is normal here.
        McpState::NotStarted => Some(format!("MCP server {} (tools mcp__{}__*)", s.name, s.name)),
        _ => Some(format!(
            "MCP server {}: {} (tools mcp__{}__*)",
            s.name,
            s.status(),
            s.name
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_map_to_effects() {
        use rmcp::model::ToolAnnotations;
        let a = |ro: Option<bool>, d: Option<bool>| {
            let mut a = ToolAnnotations::default();
            a.read_only_hint = ro;
            a.destructive_hint = d;
            a
        };
        assert_eq!(hinted_effect(None), Effect::Write);
        assert_eq!(hinted_effect(Some(&a(None, None))), Effect::Write);
        assert_eq!(hinted_effect(Some(&a(Some(true), None))), Effect::Read);
        // readOnly wins: destructiveHint only means something for writers.
        assert_eq!(
            hinted_effect(Some(&a(Some(true), Some(true)))),
            Effect::Read
        );
        assert_eq!(
            hinted_effect(Some(&a(Some(false), Some(true)))),
            Effect::Destructive
        );
        assert_eq!(hinted_effect(Some(&a(None, Some(false)))), Effect::Write);
    }

    #[test]
    fn config_overrides_hints_by_name_then_star() {
        let mut cfg: McpServerConfig = serde_json::from_value(serde_json::json!({
            "transport": "stdio", "command": "x", "effects": { "search": "read" }
        }))
        .unwrap();
        assert_eq!(
            effect_for(&cfg, "search", Effect::Destructive),
            Effect::Read
        );
        assert_eq!(
            effect_for(&cfg, "send", Effect::Destructive),
            Effect::Destructive
        );
        cfg.effects.insert("*".into(), Effect::Write);
        assert_eq!(effect_for(&cfg, "send", Effect::Destructive), Effect::Write);
        assert_eq!(effect_for(&cfg, "search", Effect::Write), Effect::Read);
        let bad = serde_json::json!({ "transport": "stdio", "effects": { "x": "delete" } });
        assert!(serde_json::from_value::<McpServerConfig>(bad).is_err());
    }

    #[test]
    fn untrusted_servers_taint_with_every_tool() {
        let tool = |untrusted: bool| McpTool {
            server: McpServer::new(
                "browser",
                McpServerConfig {
                    untrusted,
                    ..McpServerConfig::default()
                },
                Arc::default(),
            ),
            tool: RemoteTool {
                remote: "browser_snapshot".into(),
                exposed: "mcp__browser__browser_snapshot".into(),
                description: String::new(),
                schema: Value::Null,
                hinted: Effect::Read,
            },
            accounts: Arc::new(Accounts {
                conns: None,
                secrets: Arc::default(),
                instances: Mutex::default(),
                idle: DEFAULT_IDLE,
            }),
        };
        assert!(tool(true).untrusted());
        assert!(!tool(false).untrusted());
        let cfg: McpServerConfig = serde_json::from_value(serde_json::json!({
            "transport": "stdio", "command": "npx", "untrusted": true
        }))
        .unwrap();
        assert!(cfg.untrusted);
    }

    #[test]
    fn exposed_names_are_safe_and_bounded() {
        assert_eq!(
            exposed_name("github", "create_issue"),
            "mcp__github__create_issue"
        );
        assert_eq!(
            exposed_name("my.server", "do thing/now"),
            "mcp__my_server__do_thing_now"
        );
        let long = exposed_name("server", &"x".repeat(100));
        assert_eq!(long.len(), 64);
        assert_eq!(long, exposed_name("server", &"x".repeat(100)), "stable");
        assert_ne!(long, exposed_name("server", &"x".repeat(101)));
    }
}
