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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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
    "connector_from_openapi",
    "connector_test",
    "connector_activate",
    "schedule_create",
    "schedule_delete",
    "mcp_add",
    "mcp_update",
    "mcp_remove",
    // Signing in: each call can reach a host the agent chose and show the
    // user a card.
    "auth_discover",
    "auth_propose",
    "connection_add",
];

/// Whether a tool with this declared effect may be offered to scripts:
/// `read` tools, and `write` tools when `[python] allow_writes` is on
/// (PY2). `destructive` tools never are. The per-call policy is checked
/// again at each call.
pub fn callable_in_scripts(name: &str, effect: Effect, allow_writes: bool) -> bool {
    !NOT_IN_SCRIPTS.contains(&name)
        && match effect {
            Effect::Read => true,
            Effect::Write => allow_writes,
            Effect::Destructive => false,
        }
}

/// What the loop lets one script do (`[python]`, docs/python-tool-spec.md
/// §3.4, §3.5, §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptLimits {
    /// `write` tools are callable from scripts.
    pub allow_writes: bool,
    /// Approval prompts one script may cause; the next call that would ask
    /// is refused.
    pub max_asks: u32,
    /// Write calls one script may make.
    pub max_writes: u32,
    /// Tool calls run at once when a script awaits several (`gather`).
    pub max_parallel: usize,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            allow_writes: false,
            max_asks: 3,
            max_writes: 10,
            max_parallel: 4,
        }
    }
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
    /// The script can't go on: it ends with this error, uncatchable. Used
    /// when a re-run diverged after writes had already run (spec §4.2).
    Abort(String),
}

pub enum HostRequest {
    Env(oneshot::Sender<ScriptEnv>),
    /// One or more calls the script made without waiting for each result
    /// (`gather`): planned and journaled in order, run up to `max_parallel`
    /// at a time, answered together in the same order. A plain call is a
    /// batch of one.
    Calls {
        calls: Vec<(String, Value)>,
        reply: oneshot::Sender<Vec<HostReply>>,
    },
}

/// What a script tool sees of the loop during one call.
pub struct ScriptHost {
    callable: Vec<ToolSpec>,
    /// Tools the run has that scripts can't call, and why.
    blocked: BTreeMap<String, String>,
    tx: mpsc::Sender<HostRequest>,
    /// Milliseconds the loop spent waiting for the user, which don't count
    /// against the script's wall time.
    waited_ms: AtomicU64,
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
                waited_ms: AtomicU64::new(0),
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

    /// One call; see [`ScriptHost::calls`].
    pub async fn call(&self, name: &str, input: Value) -> HostReply {
        self.calls(vec![(name.to_owned(), input)])
            .await
            .pop()
            .unwrap_or_else(|| HostReply::Refused("the run stopped".into()))
    }

    /// Several calls at once. The answers come back in the order asked.
    pub async fn calls(&self, calls: Vec<(String, Value)>) -> Vec<HostReply> {
        let n = calls.len();
        let stopped = || vec![HostReply::Refused("the run stopped".into()); n];
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(HostRequest::Calls { calls, reply })
            .await
            .is_err()
        {
            return stopped();
        }
        match rx.await {
            Ok(r) if r.len() == n => r,
            _ => stopped(),
        }
    }

    /// Time the loop spent waiting for the user so far; the script's wall
    /// limit doesn't count it.
    pub fn waited(&self) -> Duration {
        Duration::from_millis(self.waited_ms.load(Ordering::Relaxed))
    }

    /// Record time spent waiting for the user.
    pub fn add_wait(&self, d: Duration) {
        self.waited_ms
            .fetch_add(d.as_millis() as u64, Ordering::Relaxed);
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
