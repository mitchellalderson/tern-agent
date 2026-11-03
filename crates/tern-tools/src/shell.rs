//! The `shell` tool, run through a `SandboxBackend`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::tool::{Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;
use tern_sandbox::{ExecOpts, SandboxBackend};

use crate::{args, spec};

pub struct Shell {
    backend: Arc<dyn SandboxBackend>,
    default_timeout: Duration,
}

impl Shell {
    pub fn new(backend: Arc<dyn SandboxBackend>, default_timeout: Duration) -> Self {
        Self {
            backend,
            default_timeout,
        }
    }
}

#[derive(Deserialize)]
struct ShellArgs {
    command: String,
    timeout_secs: Option<u64>,
}

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        spec(
            "shell",
            "Run a shell command (sh -c) in the workspace directory. Returns the exit code, stdout, and stderr. The environment is minimal; no secrets are available.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1 }
                },
                "required": ["command"]
            }),
        )
    }

    fn policy(&self, _ctx: &ToolCtx, _args: &Value) -> ToolPolicy {
        ToolPolicy {
            idempotent: false,
            requires_approval: true,
        }
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: ShellArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        if let Err(e) = tokio::fs::create_dir_all(&ctx.workspace).await {
            return ToolResult::error(format!("creating workspace: {e}"));
        }
        let timeout = a
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);
        let opts = ExecOpts {
            cwd: ctx.workspace.clone(),
            timeout,
            max_output: 256 * 1024,
        };
        let out = match self
            .backend
            .exec(&a.command, opts, ctx.cancel.clone())
            .await
        {
            Ok(o) => o,
            Err(e) => return ToolResult::error(e.to_string()),
        };
        let status = if out.timed_out {
            format!("timed out after {}s", timeout.as_secs())
        } else if out.cancelled {
            "cancelled".to_owned()
        } else {
            match out.exit_code {
                Some(c) => format!("exit code {c}"),
                None => "killed by signal".to_owned(),
            }
        };
        let mut text = status;
        if !out.stdout.is_empty() {
            text.push_str("\n--- stdout ---\n");
            text.push_str(&out.stdout);
        }
        if !out.stderr.is_empty() {
            text.push_str("\n--- stderr ---\n");
            text.push_str(&out.stderr);
        }
        if out.truncated {
            text.push_str("\n[output truncated]");
        }
        ToolResult {
            content: text,
            is_error: out.exit_code != Some(0),
        }
    }
}
