//! The host side of scripts that call tools (the `python` tool,
//! docs/python-tool-spec.md §3).
//!
//! A script tool doesn't run tools itself. It asks the agent loop through the
//! [`ScriptHost`] installed around its call, and the loop runs each call the
//! way it runs a top-level one: scope, `approval::check`, a journaled
//! `py_call` sub-step, taint. Replies come back over a channel so the loop
//! keeps every bit of state (step index, taint) to itself.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::tool::{Effect, ToolResult};
use crate::types::ToolSpec;

/// The tool name the loop serves host calls for.
pub const PYTHON: &str = "python";

/// Tools a script never calls, whatever their effect: the script tool itself,
/// and tools that start runs, talk to the user or change tern's own
/// configuration. Those should be visible top-level steps.
pub const NOT_IN_SCRIPTS: &[&str] = &[
    PYTHON,
    "shell",
    "agent_spawn",
    "agent_send",
    "agent_cancel",
    "agent_wait",
    "agent_ask_parent",
    "channel_post",
    "channel_suggest",
    "secret_request",
    "connector_write",
    "connector_test",
    "connector_activate",
    "schedule_create",
    "schedule_delete",
    "mcp_add",
    "mcp_update",
    "mcp_remove",
];

/// Whether a tool with this declared effect may be offered to scripts (PY0:
/// `read` tools only). The per-call policy is checked again at each call.
pub fn callable_in_scripts(name: &str, effect: Effect) -> bool {
    effect == Effect::Read && !NOT_IN_SCRIPTS.contains(&name)
}

/// The clock and random seed a script runs with, journaled as its first
/// sub-step so a re-run after a crash sees the same values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptEnv {
    /// Unix seconds that `time.time()` and `datetime.now()` return.
    pub clock_secs: i64,
    pub seed: u64,
}

/// The loop's answer to one host call.
#[derive(Debug, Clone)]
pub enum HostReply {
    /// The call ran (or was replayed): its result, errors included.
    Done(ToolResult),
    /// The call wasn't made; the script raises `PermissionError` with this.
    Refused(String),
}

pub enum HostRequest {
    Env(oneshot::Sender<ScriptEnv>),
    Call {
        name: String,
        input: Value,
        reply: oneshot::Sender<HostReply>,
    },
}

/// What a script tool sees of the loop during one call.
pub struct ScriptHost {
    callable: Vec<ToolSpec>,
    /// Tools the run has that scripts can't call, and why.
    blocked: BTreeMap<String, String>,
    tx: mpsc::Sender<HostRequest>,
}

impl ScriptHost {
    /// A host offering `callable`, and the receiver the loop serves.
    /// `blocked` says why other tools the run knows about can't be called.
    pub fn channel(
        callable: Vec<ToolSpec>,
        blocked: BTreeMap<String, String>,
    ) -> (Arc<Self>, mpsc::Receiver<HostRequest>) {
        let (tx, rx) = mpsc::channel(1);
        (
            Arc::new(Self {
                callable,
                blocked,
                tx,
            }),
            rx,
        )
    }

    /// The tools a script may call, sorted by name.
    pub fn callable(&self) -> &[ToolSpec] {
        &self.callable
    }

    /// Why `name`, a tool the run knows about, can't be called from a script.
    pub fn blocked(&self) -> &BTreeMap<String, String> {
        &self.blocked
    }

    /// The journaled clock and seed. `None` if the loop has gone away.
    pub async fn env(&self) -> Option<ScriptEnv> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(HostRequest::Env(reply)).await.ok()?;
        rx.await.ok()
    }

    pub async fn call(&self, name: &str, input: Value) -> HostReply {
        let (reply, rx) = oneshot::channel();
        let req = HostRequest::Call {
            name: name.to_owned(),
            input,
            reply,
        };
        if self.tx.send(req).await.is_err() {
            return HostReply::Refused("the run stopped".into());
        }
        rx.await
            .unwrap_or_else(|_| HostReply::Refused("the run stopped".into()))
    }
}

tokio::task_local! {
    static HOST: Arc<ScriptHost>;
}

/// Run `fut` (a script tool's call) with `host` answering its tool calls.
pub async fn with_host<F: std::future::Future>(host: Arc<ScriptHost>, fut: F) -> F::Output {
    HOST.scope(host, fut).await
}

/// The host of the script call being run, if the agent loop installed one.
pub fn current_host() -> Option<Arc<ScriptHost>> {
    HOST.try_with(Arc::clone).ok()
}
