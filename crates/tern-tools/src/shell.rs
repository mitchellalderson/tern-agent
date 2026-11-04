//! The `shell` tool, run through a `SandboxBackend`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult, progress_sink};
use tern_core::types::ToolSpec;
use tern_sandbox::{ExecOpts, OutputSink, SandboxBackend};

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

impl Shell {
    /// Whether this call asks for networking the sandbox doesn't give by
    /// default.
    fn wants_network(&self, args: &Value) -> bool {
        args["network"] == true && !self.backend.network_reachable()
    }
}

#[async_trait]
impl Tool for Shell {
    fn effect(&self) -> Effect {
        Effect::Destructive
    }

    /// With the network reachable a command can fetch anything (`curl`, a
    /// script calling an API), so its output taints the run like
    /// `http_fetch`'s. Without it, it only sees local files. A command that
    /// asked for network (`network: true`) marks its own result.
    fn untrusted(&self) -> bool {
        self.backend.network_reachable()
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "shell",
            "Run a shell command (sh -c) in the workspace directory. Returns the exit code, stdout, and stderr. The environment is minimal; no secrets are available. Commands have no network access unless you pass network: true, which the user must approve every time; use it only for things like git push/pull or installing packages, never to call a service's API (use its tool).",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1 },
                    "network": {
                        "type": "boolean",
                        "description": "Give this command IP networking. Always asks the user."
                    }
                },
                "required": ["command"]
            }),
        )
    }

    fn policy(&self, _ctx: &ToolCtx, _args: &Value) -> ToolPolicy {
        // Destructive outside the allowlist, a granted write inside it
        // (`approval::check`). A call asking for network is never covered
        // by the allowlist (`approval::allowed_by`).
        ToolPolicy::new(Effect::Destructive, false).forced(true)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let cmd = args["command"].as_str()?;
        self.wants_network(args)
            .then(|| format!("with network: {cmd}"))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let network = self.wants_network(&raw);
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
            on_output: progress_sink().map(|sink| OutputSink(Arc::new(move |s| sink(s)))),
            network,
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
            // What a command fetched is third-party text.
            untrusted: network,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use tern_core::tool::{ProgressSink, with_progress};
    use tern_sandbox::HostBackend;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[tokio::test]
    async fn streams_output_while_running_and_still_returns_it() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            workspace: tmp.path().into(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let shell = Shell::new(Arc::new(HostBackend::default()), Duration::from_secs(10));
        let chunks = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink: ProgressSink = {
            let chunks = chunks.clone();
            Arc::new(move |s| chunks.lock().unwrap().push(s.to_owned()))
        };
        let r = with_progress(
            sink,
            shell.call(
                &ctx,
                json!({"command": "echo first; sleep 0.6; echo second >&2"}),
            ),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("first") && r.content.contains("second"));
        let chunks = chunks.lock().unwrap();
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert_eq!(chunks[0], "first\n");
        assert_eq!(chunks[1], "second\n");
    }

    #[tokio::test]
    async fn a_command_given_network_says_so_and_its_output_is_untrusted() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            workspace: tmp.path().into(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let offline = tern_sandbox::HostPolicy {
            network: false,
            ..tern_sandbox::HostPolicy::confined()
        };
        let shell = Shell::new(Arc::new(HostBackend::new(offline)), Duration::from_secs(10));
        let ask = json!({"command": "echo hi", "network": true});
        assert_eq!(
            shell.summary(&ask).as_deref(),
            Some("with network: echo hi")
        );
        assert!(shell.call(&ctx, ask).await.untrusted);
        let plain = json!({"command": "echo hi"});
        assert_eq!(shell.summary(&plain), None);
        assert!(!shell.call(&ctx, plain).await.untrusted);

        // With network on for every command, asking adds nothing: every
        // command's output is already untrusted (`Tool::untrusted`).
        let online = Shell::new(Arc::new(HostBackend::default()), Duration::from_secs(10));
        let ask = json!({"command": "echo hi", "network": true});
        assert_eq!(online.summary(&ask), None);
        assert!(online.untrusted());
    }

    #[test]
    fn output_is_untrusted_when_commands_can_reach_the_network() {
        let shell = |network| {
            let settings = tern_sandbox::DockerSettings {
                network,
                ..tern_sandbox::DockerSettings::default()
            };
            let backend = tern_sandbox::DockerBackend::new("docker", Path::new("/w"), settings);
            Shell::new(Arc::new(backend), Duration::from_secs(1))
        };
        assert!(shell(true).untrusted());
        assert!(!shell(false).untrusted());
        // No OS sandbox: `network = false` isn't enforced, so it counts as on.
        let mut policy = tern_sandbox::HostPolicy::unconfined();
        policy.network = false;
        let host = Shell::new(Arc::new(HostBackend::new(policy)), Duration::from_secs(1));
        assert!(host.untrusted());
    }
}
