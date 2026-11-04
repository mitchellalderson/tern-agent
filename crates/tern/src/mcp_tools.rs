//! The agent's MCP tools (docs/mcp-management-spec.md §6.2): list, add,
//! update and remove servers from the chat. Every write asks the user each
//! time, never "always". A `stdio` server runs a command on this host
//! outside the sandbox, so adding one can only be approved from the TUI or
//! desktop app on the host unless `api.remote_mcp_stdio` is set. Secrets
//! never pass through these tools: the agent asks with `secret_request`.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::config::{McpServerConfig, McpTransport};
use tern_core::mcp::{McpPatch, McpStatus, check_config, check_name};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolRegistry, ToolResult};
use tern_core::types::ToolSpec;

use crate::mcp::McpAdmin;

pub fn register(registry: &mut ToolRegistry, admin: Arc<McpAdmin>) {
    registry.register(Arc::new(McpList(admin.clone())));
    registry.register(Arc::new(McpAdd(admin.clone())));
    registry.register(Arc::new(McpUpdate(admin.clone())));
    registry.register(Arc::new(McpRemove(admin)));
}

fn spec(name: &str, description: &str, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

fn parse<T: serde::de::DeserializeOwned>(raw: Value) -> Result<T, ToolResult> {
    serde_json::from_value(raw).map_err(|e| ToolResult::error(format!("invalid arguments: {e}")))
}

fn name_of(raw: &Value) -> &str {
    raw.get("name").and_then(Value::as_str).unwrap_or("?")
}

/// What the agent sees of a server: config and state, never secret values.
fn view(s: &McpStatus) -> Value {
    let c = &s.config;
    json!({
        "name": s.name,
        "state": s.state,
        "config": c,
        "server_info": s.server_info,
        "tools": s.tools.iter().filter(|t| !t.disabled).map(|t| &t.exposed).collect::<Vec<_>>(),
        "hidden_tools": c.disabled_tools,
        "missing_secrets": s.missing_secrets,
        "stderr_tail": s.stderr_tail.iter().rev().take(10).rev().collect::<Vec<_>>(),
    })
}

struct McpList(Arc<McpAdmin>);

#[async_trait]
impl Tool for McpList {
    fn spec(&self) -> ToolSpec {
        spec(
            "mcp_list",
            "List the configured MCP servers: config (secret names only), state, tools, missing secrets and the last lines of a stdio server's stderr. Use it before changing a server or to find out why one isn't working.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn effect(&self) -> Effect {
        Effect::Read
    }

    /// Server stderr and tool descriptions come from outside.
    fn untrusted(&self) -> bool {
        true
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    async fn call(&self, _: &ToolCtx, _: Value) -> ToolResult {
        let all: Vec<Value> = self.0.mcp_statuses().iter().map(view).collect();
        ToolResult::ok(serde_json::to_string_pretty(&all).unwrap_or_default())
    }
}

struct McpAdd(Arc<McpAdmin>);

/// Everything but `name` and `replace` is the server's config, which
/// refuses unknown fields.
fn add_args(raw: &Value) -> Result<(String, bool, McpServerConfig), String> {
    let mut v = raw.clone();
    let m = v.as_object_mut().ok_or("arguments must be an object")?;
    let name = match m.remove("name") {
        Some(Value::String(n)) => n,
        _ => return Err("`name` is required".into()),
    };
    let replace = m
        .remove("replace")
        .and_then(|r| r.as_bool())
        .unwrap_or(false);
    m.remove("oauth");
    let cfg = serde_json::from_value(v).map_err(|e| format!("invalid config: {e}"))?;
    Ok((name, replace, cfg))
}

impl McpAdd {
    fn config(&self, raw: &Value) -> Option<McpServerConfig> {
        add_args(raw).ok().map(|(_, _, c)| c)
    }
}

/// `oauth: true`: an http server that signs in with OAuth. It answers 401
/// until an account is connected, so that answer saves it.
fn signs_in(raw: &Value, cfg: &McpServerConfig) -> bool {
    raw.get("oauth").and_then(Value::as_bool) == Some(true)
        && cfg.transport == McpTransport::Http
        && cfg.secret_names().next().is_none()
}

#[async_trait]
impl Tool for McpAdd {
    fn spec(&self) -> ToolSpec {
        spec(
            "mcp_add",
            "Add an MCP server, or replace one with `replace: true`. The user approves every call, seeing the exact command or URL. Before calling: work out the config from the server's docs (a pasted `mcpServers` JSON maps directly: command/args/env, or url/headers). For a remote (http) server, sign-in comes first: call auth_discover with its URL and mcp: true; if it signs in with OAuth, call auth_propose, and when the user chooses signing in, add it with `oauth: true` and no secret fields, then call connection_add so they sign in. Otherwise put every API key or token in `env_secrets` (stdio), `bearer_token_secret` or `header_secrets` (http) by name, and get missing values with `secret_request` first. Never put a secret value in `env`, `args` or the URL. The server is started and its tools listed before saving; if that fails you get its error and stderr and nothing is saved. Its tools can be used from the user's next message. A command (stdio) server can only be approved in the TUI or desktop app on the host, not from Telegram or another device: if the user is elsewhere, tell them that.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "1-24 characters, a-z 0-9 _ -; tools appear as mcp__<name>__<tool>" },
                    "transport": { "type": "string", "enum": ["stdio", "http"], "description": "stdio: a local command; http: a streamable HTTP URL (SSE isn't supported)" },
                    "command": { "type": "string", "description": "stdio: the executable, e.g. npx, uvx, docker" },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "stdio: plain, non-secret settings only" },
                    "cwd": { "type": "string" },
                    "env_secrets": { "type": "array", "items": { "type": "string" }, "description": "stdio: names of secrets passed as environment variables" },
                    "url": { "type": "string" },
                    "bearer_token_secret": { "type": "string", "description": "http: secret sent as Authorization: Bearer" },
                    "header_secrets": { "type": "object", "additionalProperties": { "type": "string" }, "description": "http: header name → secret name" },
                    "disabled_tools": { "type": "array", "items": { "type": "string" }, "description": "the server's tool names to hide" },
                    "requires_approval": { "type": "boolean", "description": "ask before each of its tool calls" },
                    "idempotent": { "type": "boolean", "description": "its tools are safe to re-run" },
                    "replace": { "type": "boolean", "description": "replace an existing server of this name" },
                    "oauth": { "type": "boolean", "description": "http: the server signs in with OAuth (from auth_discover); it is saved even though it refuses requests until connection_add signs an account in" }
                },
                "required": ["name", "transport"]
            }),
        )
    }

    fn effect(&self) -> Effect {
        Effect::Destructive
    }

    fn allow_always(&self) -> bool {
        false
    }

    /// Always asked. A command is `destructive` (it runs outside the
    /// sandbox) and only the host may approve it; a URL is a `write`.
    fn policy(&self, _: &ToolCtx, raw: &Value) -> ToolPolicy {
        let stdio = self
            .config(raw)
            .is_none_or(|c| c.transport == McpTransport::Stdio);
        let effect = if stdio {
            Effect::Destructive
        } else {
            Effect::Write
        };
        ToolPolicy {
            local_approval: stdio && !self.0.remote_stdio_allowed(),
            ..ToolPolicy::new(effect, false).forced(true)
        }
    }

    fn summary(&self, raw: &Value) -> Option<String> {
        let name = name_of(raw);
        let c = self.config(raw)?;
        let verb = if raw.get("replace").and_then(Value::as_bool) == Some(true) {
            "replace"
        } else {
            "add"
        };
        let what = match c.transport {
            McpTransport::Stdio => {
                let mut cmd = c.command.clone().unwrap_or_default();
                for a in &c.args {
                    cmd.push(' ');
                    cmd.push_str(a);
                }
                format!("run `{cmd}` on this machine, outside the sandbox")
            }
            McpTransport::Http => format!("connect to {}", c.url.as_deref().unwrap_or("?")),
        };
        let mut extra = Vec::new();
        if !c.env.is_empty() {
            let env: Vec<String> = c.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            extra.push(format!("env {}", env.join(" ")));
        }
        let secrets: Vec<&str> = c.secret_names().collect();
        if !secrets.is_empty() {
            extra.push(format!("secrets {}", secrets.join(", ")));
        }
        let extra = if extra.is_empty() {
            String::new()
        } else {
            format!(" ({})", extra.join("; "))
        };
        Some(format!("{verb} MCP server {name}: {what}{extra}"))
    }

    async fn call(&self, _: &ToolCtx, raw: Value) -> ToolResult {
        let (name, replace, mut cfg) = match add_args(&raw) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(e),
        };
        cfg.enabled = true;
        if let Err(e) = check_name(&name).and_then(|()| check_config(&cfg)) {
            return ToolResult::error(format!("not saved: {e}"));
        }
        let exists = self.0.mcp_status(&name).is_some();
        if exists && !replace {
            return ToolResult::error(format!(
                "not saved: MCP server {} already exists; call mcp_list to see it, and pass replace: true to change it",
                name
            ));
        }
        let missing: Vec<&str> = cfg
            .secret_names()
            .filter(|n| self.0.secrets.get(n).is_none())
            .collect();
        if !missing.is_empty() {
            return ToolResult::error(format!(
                "not saved: {} not set. Call secret_request for {} (the user types the values; you never see them), then call mcp_add again.",
                missing.join(", "),
                if missing.len() == 1 { "it" } else { "them" }
            ));
        }
        let verified = match self.0.verify_mcp(&name, &cfg, Default::default()).await {
            Ok(v) => v,
            Err(e)
                if e.kind == tern_core::mcp::McpErrorKind::Unauthorized && signs_in(&raw, &cfg) =>
            {
                let old = exists.then_some(name.as_str());
                return match self.0.save_mcp(old, &name, &cfg, &Default::default()) {
                    Ok(_) => ToolResult::ok(
                        json!({
                            "saved": true,
                            "name": name,
                            "signed_in": false,
                            "next": format!("It needs an account: call connection_add with target {name:?} (and the plan_id from auth_propose) so the user signs in."),
                        })
                        .to_string(),
                    ),
                    Err(e) => ToolResult::error(format!("not saved: {e:#}")),
                };
            }
            Err(e) => {
                return ToolResult::error(
                    json!({
                        "saved": false,
                        "error": e.kind,
                        "message": e.message,
                        "stderr_tail": e.stderr_tail,
                    })
                    .to_string(),
                );
            }
        };
        let old = exists.then_some(name.as_str());
        match self.0.save_mcp(old, &name, &cfg, &Default::default()) {
            Ok(_) => ToolResult::ok(
                json!({
                    "saved": true,
                    "name": name,
                    "server_info": verified.server_info,
                    "tools": verified.tools.iter().filter(|t| !t.disabled).map(|t| &t.exposed).collect::<Vec<_>>(),
                    "note": "Its tools can be used from the user's next message.",
                })
                .to_string(),
            ),
            Err(e) => ToolResult::error(format!("not saved: {e:#}")),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    name: String,
    enabled: Option<bool>,
    disabled_tools: Option<Vec<String>>,
    requires_approval: Option<bool>,
    idempotent: Option<bool>,
}

struct McpUpdate(Arc<McpAdmin>);

#[async_trait]
impl Tool for McpUpdate {
    fn spec(&self) -> ToolSpec {
        spec(
            "mcp_update",
            "Turn an MCP server on or off, hide some of its tools, or change whether its calls need approval. The user approves every call. To change its command, URL or secrets, use mcp_add with replace: true.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "enabled": { "type": "boolean" },
                    "disabled_tools": { "type": "array", "items": { "type": "string" }, "description": "the full list of the server's tool names to hide (replaces the old list)" },
                    "requires_approval": { "type": "boolean" },
                    "idempotent": { "type": "boolean" }
                },
                "required": ["name"]
            }),
        )
    }

    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn allow_always(&self) -> bool {
        false
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Write, true).forced(true)
    }

    fn summary(&self, raw: &Value) -> Option<String> {
        let mut changes = Vec::new();
        let flag = |k: &str| raw.get(k).and_then(Value::as_bool);
        if let Some(e) = flag("enabled") {
            changes.push(if e { "turn on" } else { "turn off" }.to_owned());
        }
        if let Some(t) = raw.get("disabled_tools").and_then(Value::as_array) {
            let t: Vec<&str> = t.iter().filter_map(Value::as_str).collect();
            changes.push(if t.is_empty() {
                "show all tools".to_owned()
            } else {
                format!("hide {}", t.join(", "))
            });
        }
        if let Some(a) = flag("requires_approval") {
            changes.push(
                if a {
                    "ask before each call"
                } else {
                    "stop asking before each call"
                }
                .to_owned(),
            );
        }
        if let Some(i) = flag("idempotent") {
            changes.push(format!("safe to retry: {i}"));
        }
        Some(format!(
            "update MCP server {}: {}",
            name_of(raw),
            changes.join(", ")
        ))
    }

    async fn call(&self, _: &ToolCtx, raw: Value) -> ToolResult {
        let a: UpdateArgs = match parse(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let patch = McpPatch {
            enabled: a.enabled,
            disabled_tools: a.disabled_tools,
            requires_approval: a.requires_approval,
            idempotent: a.idempotent,
        };
        match self.0.patch_mcp(&a.name, &patch) {
            Ok(s) => ToolResult::ok(view(&s).to_string()),
            Err(e) => ToolResult::error(format!("{e:#}")),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveArgs {
    name: String,
    #[serde(default)]
    remove_secrets: bool,
}

struct McpRemove(Arc<McpAdmin>);

#[async_trait]
impl Tool for McpRemove {
    fn spec(&self) -> ToolSpec {
        spec(
            "mcp_remove",
            "Remove an MCP server. With remove_secrets, its secrets are deleted too unless something else uses them. The user approves every call.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "remove_secrets": { "type": "boolean" }
                },
                "required": ["name"]
            }),
        )
    }

    fn effect(&self) -> Effect {
        Effect::Destructive
    }

    fn allow_always(&self) -> bool {
        false
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Destructive, false).forced(true)
    }

    fn summary(&self, raw: &Value) -> Option<String> {
        let secrets = if raw.get("remove_secrets").and_then(Value::as_bool) == Some(true) {
            " and its unused secrets"
        } else {
            ""
        };
        Some(format!("remove MCP server {}{secrets}", name_of(raw)))
    }

    async fn call(&self, _: &ToolCtx, raw: Value) -> ToolResult {
        let a: RemoveArgs = match parse(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        match self.0.remove_mcp(&a.name, a.remove_secrets) {
            Ok(()) => ToolResult::ok(format!("removed MCP server {}", a.name)),
            Err(e) => ToolResult::error(format!("{e:#}")),
        }
    }
}
