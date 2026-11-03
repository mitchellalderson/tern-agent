//! MCP client (spec §11): configured servers become tools named
//! `mcp__<server>__<tool>`. Servers start on first use and are restarted
//! with backoff after they crash.

use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess};
use serde_json::Value;
use tern_core::Secrets;
use tern_core::config::{McpServerConfig, McpTransport};
use tern_core::tool::{Tool, ToolCtx, ToolPolicy, ToolResult, ToolSource};
use tern_core::types::ToolSpec;
use tokio::io::{AsyncBufReadExt, BufReader};

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
/// Longest tool name every provider accepts.
const MAX_TOOL_NAME: usize = 64;

type Client = RunningService<RoleClient, ()>;

#[derive(Debug, Clone)]
struct RemoteTool {
    remote: String,
    exposed: String,
    description: String,
    schema: Value,
}

#[derive(Default)]
struct Conn {
    client: Option<Client>,
    failures: u32,
    retry_at: Option<Instant>,
    last_error: Option<String>,
}

pub struct McpServer {
    name: String,
    cfg: McpServerConfig,
    secrets: Arc<Secrets>,
    backoff_base: Duration,
    conn: tokio::sync::Mutex<Conn>,
    tools: RwLock<Vec<RemoteTool>>,
    status: RwLock<String>,
}

impl McpServer {
    pub fn new(name: &str, cfg: McpServerConfig, secrets: Arc<Secrets>) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            cfg,
            secrets,
            backoff_base: Duration::from_secs(1),
            conn: tokio::sync::Mutex::new(Conn::default()),
            tools: RwLock::new(Vec::new()),
            status: RwLock::new("not started".into()),
        })
    }

    /// Like [`McpServer::new`] with a custom backoff (for tests).
    pub fn with_backoff(
        name: &str,
        cfg: McpServerConfig,
        secrets: Arc<Secrets>,
        base: Duration,
    ) -> Arc<Self> {
        let mut s = Self::new(name, cfg, secrets);
        if let Some(s) = Arc::get_mut(&mut s) {
            s.backoff_base = base;
        }
        s
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn set_status(&self, s: String) {
        if let Ok(mut g) = self.status.write() {
            *g = s;
        }
    }

    pub fn status(&self) -> String {
        self.status.read().map(|s| s.clone()).unwrap_or_default()
    }

    /// Connect if not connected (respecting backoff) and refresh the tool list.
    pub async fn ensure(&self) -> Result<(), String> {
        let mut conn = self.conn.lock().await;
        self.ensure_locked(&mut conn).await
    }

    async fn ensure_locked(&self, conn: &mut Conn) -> Result<(), String> {
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
        match tokio::time::timeout(CONNECT_TIMEOUT, self.connect())
            .await
            .unwrap_or_else(|_| Err(format!("timed out after {}s", CONNECT_TIMEOUT.as_secs())))
        {
            Ok((client, tools)) => {
                tracing::info!(server = %self.name, tools = tools.len(), "MCP server connected");
                self.set_status(format!("{} tools", tools.len()));
                if let Ok(mut t) = self.tools.write() {
                    *t = tools;
                }
                conn.client = Some(client);
                conn.failures = 0;
                conn.retry_at = None;
                conn.last_error = None;
                Ok(())
            }
            Err(e) => {
                self.fail(conn, e.clone());
                Err(format!("MCP server {} failed to start: {e}", self.name))
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
        self.set_status(format!("unavailable: {error}"));
        conn.last_error = Some(error);
    }

    async fn connect(&self) -> Result<(Client, Vec<RemoteTool>), String> {
        let client = match self.cfg.transport {
            McpTransport::Stdio => {
                let command = self
                    .cfg
                    .command
                    .as_deref()
                    .ok_or("stdio transport needs `command`")?;
                let mut env = Vec::new();
                for key in &self.cfg.env_secrets {
                    let value = self
                        .secrets
                        .get(key)
                        .ok_or_else(|| format!("secret {key} is not set"))?;
                    env.push((key.clone(), value));
                }
                let cmd = tokio::process::Command::new(command).configure(|c| {
                    c.args(&self.cfg.args)
                        .env_clear()
                        .envs(
                            PASSTHROUGH_ENV
                                .iter()
                                .filter_map(|k| std::env::var_os(k).map(|v| (*k, v))),
                        )
                        .envs(env);
                });
                let (transport, stderr) = TokioChildProcess::builder(cmd)
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| format!("spawning `{command}`: {e}"))?;
                if let Some(stderr) = stderr {
                    let (name, secrets) = (self.name.clone(), self.secrets.clone());
                    tokio::spawn(async move {
                        let mut lines = BufReader::new(stderr).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            tracing::debug!(server = %name, "stderr: {}", secrets.redact(&line));
                        }
                    });
                }
                ().serve(transport).await.map_err(|e| e.to_string())?
            }
            McpTransport::Http => {
                let url = self
                    .cfg
                    .url
                    .as_deref()
                    .ok_or("http transport needs `url`")?;
                let client = tern_core::net::http_client()
                    .build()
                    .map_err(|e| e.to_string())?;
                let transport = StreamableHttpClientTransport::with_client(
                    client,
                    StreamableHttpClientTransportConfig::with_uri(url.to_owned()),
                );
                ().serve(transport).await.map_err(|e| e.to_string())?
            }
        };
        let tools = client
            .list_all_tools()
            .await
            .map_err(|e| format!("listing tools: {e}"))?;
        let tools = tools
            .into_iter()
            .map(|t| RemoteTool {
                exposed: exposed_name(&self.name, &t.name),
                remote: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                schema: Value::Object((*t.input_schema).clone()),
            })
            .collect();
        Ok((client, tools))
    }

    pub async fn call(&self, remote: &str, args: Value) -> ToolResult {
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
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.tool.exposed.clone(),
            description: format!(
                "{} (MCP server {})",
                self.tool.description, self.server.name
            ),
            input_schema: self.tool.schema.clone(),
        }
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy {
            idempotent: self.server.cfg.idempotent,
            requires_approval: self.server.cfg.requires_approval,
        }
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        tokio::select! {
            r = self.server.call(&self.tool.remote, args) => r,
            () = ctx.cancel.cancelled() => ToolResult::error("cancelled"),
        }
    }
}

/// All configured MCP servers as one tool source.
pub struct McpServers {
    servers: Vec<Arc<McpServer>>,
}

impl McpServers {
    pub fn new(servers: Vec<Arc<McpServer>>) -> Self {
        Self { servers }
    }

    pub fn from_config(
        cfg: &std::collections::BTreeMap<String, McpServerConfig>,
        secrets: Arc<Secrets>,
    ) -> Self {
        Self::new(
            cfg.iter()
                .map(|(name, c)| McpServer::new(name, c.clone(), secrets.clone()))
                .collect(),
        )
    }

    pub fn servers(&self) -> &[Arc<McpServer>] {
        &self.servers
    }
}

#[async_trait]
impl ToolSource for McpServers {
    async fn prepare(&self) {
        let starts = self.servers.iter().map(|s| async move {
            if let Err(e) = s.ensure().await {
                tracing::warn!("{e}");
            }
        });
        futures::future::join_all(starts).await;
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.servers
            .iter()
            .flat_map(|s| {
                let tools = s.tools.read().map(|t| t.clone()).unwrap_or_default();
                tools.into_iter().map(|tool| {
                    Arc::new(McpTool {
                        server: s.clone(),
                        tool,
                    }) as Arc<dyn Tool>
                })
            })
            .collect()
    }

    fn describe(&self) -> Vec<String> {
        self.servers
            .iter()
            .map(|s| {
                let status = s.status();
                // Servers start on a run's first use, so "not started" is normal here.
                if status == "not started" {
                    format!("MCP server {} (tools mcp__{}__*)", s.name, s.name)
                } else {
                    format!("MCP server {}: {status} (tools mcp__{}__*)", s.name, s.name)
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
