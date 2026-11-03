//! Agent-authored connectors (spec §10.4): `connector_write` drafts and
//! validates, `connector_test` runs a draft's tool, `connector_activate`
//! promotes a draft after the user approves its capabilities.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_core::tool::{Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;

use crate::{EXTENSION, Host, Vm, load_file, tool_name, valid_ident};

#[derive(Clone)]
pub struct Dirs {
    pub active: PathBuf,
    pub drafts: PathBuf,
}

impl Dirs {
    pub fn draft_path(&self, name: &str) -> PathBuf {
        self.draft(name)
    }
    fn draft(&self, name: &str) -> PathBuf {
        self.drafts.join(format!("{name}.{EXTENSION}"))
    }
    fn active(&self, name: &str) -> PathBuf {
        self.active.join(format!("{name}.{EXTENSION}"))
    }
}

fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolResult> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolResult::error(format!("invalid arguments: `{key}` must be a string")))
}

fn name_arg(args: &Value) -> Result<&str, ToolResult> {
    let name = arg(args, "name")?;
    if !valid_ident(name) {
        return Err(ToolResult::error(format!(
            "name {name:?} must match [a-z][a-z0-9_]* (max 32 chars)"
        )));
    }
    Ok(name)
}

fn load_draft(host: &Host, dirs: &Dirs, name: &str) -> Result<Vm, String> {
    let path = dirs.draft(name);
    if !path.exists() {
        return Err(format!(
            "no draft named {name:?}; write one with connector_write"
        ));
    }
    load_file(host, &path).map_err(|e| e.to_string())
}

fn describe(vm: &Vm) -> String {
    let s = &vm.spec;
    let mut out = format!("{} — {}\n{}", s.name, s.description, s.capability_summary());
    for t in &s.tools {
        out.push_str(&format!(
            "\n- {}: {}",
            tool_name(&s.name, &t.name),
            t.description
        ));
    }
    out
}

const NOT_IDEMPOTENT: ToolPolicy = ToolPolicy {
    idempotent: false,
    requires_approval: false,
};

// ---- connector_write ----

pub struct ConnectorWrite {
    pub host: Host,
    pub dirs: Dirs,
}

#[async_trait]
impl Tool for ConnectorWrite {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "connector_write".into(),
            description: CONNECTOR_DOCS.into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "lowercase identifier; must equal the table's `name`" },
                    "source": { "type": "string", "description": "complete Luau source of the connector file" }
                },
                "required": ["name", "source"]
            }),
        }
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        NOT_IDEMPOTENT
    }

    async fn call(&self, _: &ToolCtx, args: Value) -> ToolResult {
        let (name, source) = match (name_arg(&args), arg(&args, "source")) {
            (Ok(n), Ok(s)) => (n, s),
            (Err(e), _) | (_, Err(e)) => return e,
        };
        if let Err(e) = std::fs::create_dir_all(&self.dirs.drafts) {
            return ToolResult::error(format!("creating drafts directory: {e}"));
        }
        if let Err(e) = std::fs::write(self.dirs.draft(name), source) {
            return ToolResult::error(format!("writing draft: {e}"));
        }
        match load_draft(&self.host, &self.dirs, name) {
            Ok(vm) => ToolResult::ok(format!(
                "draft saved and valid:\n{}\n\nTry it with connector_test, then ask to activate it with connector_activate.",
                describe(&vm)
            )),
            Err(e) => ToolResult::error(format!("draft saved but invalid: {e}")),
        }
    }
}

// ---- connector_test ----

pub struct ConnectorTest {
    pub host: Host,
    pub dirs: Dirs,
}

#[async_trait]
impl Tool for ConnectorTest {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "connector_test".into(),
            description:
                "Run one tool of a draft connector with the given arguments and return its result."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "tool": { "type": "string" },
                    "args": { "type": "object" }
                },
                "required": ["name", "tool"]
            }),
        }
    }

    /// A draft that can reach the network or read secrets is run only with
    /// approval: it is code the model just wrote.
    fn policy(&self, _: &ToolCtx, args: &Value) -> ToolPolicy {
        let risky = name_arg(args)
            .ok()
            .and_then(|n| load_draft(&self.host, &self.dirs, n).ok())
            .is_some_and(|vm| {
                !vm.spec.capabilities.http.is_empty() || !vm.spec.capabilities.secrets.is_empty()
            });
        ToolPolicy {
            idempotent: false,
            requires_approval: risky,
        }
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let name = name_arg(args).ok()?;
        let tool = args.get("tool").and_then(Value::as_str).unwrap_or("?");
        let caps = load_draft(&self.host, &self.dirs, name)
            .map(|vm| vm.spec.capability_summary())
            .unwrap_or_else(|e| format!("invalid draft: {e}"));
        Some(format!("connector_test draft {name}.{tool} ({caps})"))
    }

    async fn call(&self, _: &ToolCtx, args: Value) -> ToolResult {
        let (name, tool) = match (name_arg(&args), arg(&args, "tool")) {
            (Ok(n), Ok(t)) => (n, t),
            (Err(e), _) | (_, Err(e)) => return e,
        };
        let vm = match load_draft(&self.host, &self.dirs, name) {
            Ok(vm) => vm,
            Err(e) => return ToolResult::error(e),
        };
        let tool_args = args.get("args").cloned().unwrap_or_else(|| json!({}));
        match vm.call(tool, &tool_args).await {
            Ok(out) => ToolResult::ok(out),
            Err(e) => ToolResult::error(format!("{name}.{tool}: {e}")),
        }
    }
}

// ---- connector_activate ----

pub struct ConnectorActivate {
    pub host: Host,
    pub dirs: Dirs,
}

#[async_trait]
impl Tool for ConnectorActivate {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "connector_activate".into(),
            description: "Promote a tested draft connector so its tools are available in future messages. The user must approve its capabilities.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        }
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy {
            idempotent: false,
            requires_approval: true,
        }
    }

    /// The approval prompt shows exactly what the connector may access.
    fn summary(&self, args: &Value) -> Option<String> {
        let name = name_arg(args).ok()?;
        let replacing = if self.dirs.active(name).exists() {
            " (replacing the active one)"
        } else {
            ""
        };
        Some(match load_draft(&self.host, &self.dirs, name) {
            Ok(vm) => format!(
                "activate Luau connector {name}{replacing} — {}",
                vm.spec.capability_summary()
            ),
            Err(e) => format!("activate Luau connector {name} — invalid draft: {e}"),
        })
    }

    async fn call(&self, _: &ToolCtx, args: Value) -> ToolResult {
        let name = match name_arg(&args) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let vm = match load_draft(&self.host, &self.dirs, name) {
            Ok(vm) => vm,
            Err(e) => return ToolResult::error(e),
        };
        if let Err(e) = std::fs::create_dir_all(&self.dirs.active) {
            return ToolResult::error(format!("creating connectors directory: {e}"));
        }
        if let Err(e) = std::fs::rename(self.dirs.draft(name), self.dirs.active(name)) {
            return ToolResult::error(format!("activating: {e}"));
        }
        let tools: Vec<String> = vm
            .spec
            .tools
            .iter()
            .map(|t| tool_name(name, &t.name))
            .collect();
        ToolResult::ok(format!(
            "activated {name}; from the next message its tools are available: {}",
            tools.join(", ")
        ))
    }
}

const CONNECTOR_DOCS: &str = r#"Write (or overwrite) a draft Luau connector in ~/.tern/connectors/drafts/ and validate it. A connector file returns a table:

return {
  name = "weather",                       -- must equal the `name` argument
  description = "Look up forecasts",
  capabilities = {
    http = { "api.weather.example" },     -- hosts ctx.http may call ("*.example.com" allowed)
    secrets = { "WEATHER_API_KEY" },      -- secrets ctx.secret may read
  },
  tools = {
    forecast = {
      description = "Forecast for a city",
      idempotent = true,                  -- safe to re-run after a crash (default false)
      params = { type = "object", properties = { city = { type = "string" } }, required = { "city" } },
      run = function(ctx, args)
        local res = ctx.http.get("https://api.weather.example/v1?q=" .. args.city, {
          headers = { Authorization = "Bearer " .. ctx.secret("WEATHER_API_KEY") },
        })
        if not res.ok then error("HTTP " .. res.status) end
        return res.json                   -- tables are returned as JSON, strings as-is
      end,
    },
  },
}

ctx API: ctx.http.request{method, url, headers, json, body, timeout} and ctx.http.get/post/put/patch/delete(url, opts) return {status, ok, headers, body, json}; ctx.secret(name); ctx.kv.get(key) / ctx.kv.set(key, value) (per-connector storage; set nil to delete); ctx.log(level, msg); ctx.now() (unix ms). Globals: json.encode, json.decode, string, table, math, os.time/os.date. No file or process access, no require. Each call has a 5 second limit."#;
