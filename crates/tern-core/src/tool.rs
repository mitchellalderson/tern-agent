//! The `Tool` trait and the registry the agent loop calls through.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::types::{ToolCall, ToolSpec};

/// Tool output beyond this is cut in the middle (spec §6).
pub const MAX_TOOL_OUTPUT: usize = 32 * 1024;

/// What a tool call does to the world (tooling spec §5.1). The host decides
/// whether to ask from this, never the model: see [`crate::approval::check`].
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Only looks. Never asks.
    Read,
    /// Changes something. Asks unless an "always" grant covers it. The
    /// default for a tool that declares nothing (tooling spec §13.4).
    #[default]
    Write,
    /// Hard or impossible to undo. Asks every time.
    Destructive,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Read => "read",
            Effect::Write => "write",
            Effect::Destructive => "destructive",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Effect::Read),
            "write" => Some(Effect::Write),
            "destructive" => Some(Effect::Destructive),
            _ => None,
        }
    }
}

impl std::fmt::Display for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolPolicy {
    /// Safe to re-run after a crash interrupted it.
    pub idempotent: bool,
    /// The effect of this call; usually [`Tool::effect`], but a tool may
    /// raise it for particular arguments.
    pub effect: Effect,
    /// Ask even where the effect alone wouldn't (a `read` outside the
    /// workspace, config `requires_approval`). Grants still apply.
    pub requires_approval: bool,
    /// The host grants this `write` without a stored grant: writes to the
    /// workspace and tern's own state, and grandfathered M4 connectors.
    /// Ignored for `destructive` and when `requires_approval` is set.
    pub granted: bool,
    /// Set for tools flagged `money_movement` or `trade`: refused, and
    /// nothing can approve them.
    pub forbidden: Option<&'static str>,
    /// Only a front end on this host may approve the call (the TUI or the
    /// desktop app over the local socket), not a chat gateway or a paired
    /// device: it would run a command here outside the sandbox.
    pub local_approval: bool,
}

impl ToolPolicy {
    pub const fn new(effect: Effect, idempotent: bool) -> Self {
        Self {
            idempotent,
            effect,
            requires_approval: false,
            granted: false,
            forbidden: None,
            local_approval: false,
        }
    }

    /// A `write` the host grants by itself (see [`ToolPolicy::granted`]).
    pub const fn granted_write(idempotent: bool) -> Self {
        Self {
            granted: true,
            ..Self::new(Effect::Write, idempotent)
        }
    }

    pub const fn forced(self, requires_approval: bool) -> Self {
        Self {
            requires_approval,
            ..self
        }
    }
}

pub struct ToolCtx {
    pub workspace: PathBuf,
    pub run_id: String,
    pub session_id: String,
    pub cancel: CancellationToken,
}

/// Where a running tool sends output as it is produced.
pub type ProgressSink = Arc<dyn Fn(&str) + Send + Sync>;

tokio::task_local! {
    static PROGRESS: ProgressSink;
}

/// Run `fut` (a tool call) with `sink` as its live-output sink. The agent
/// loop wraps each call with the sink for that call's `tool_output` events.
pub async fn with_progress<F: std::future::Future>(sink: ProgressSink, fut: F) -> F::Output {
    PROGRESS.scope(sink, fut).await
}

/// The live-output sink of the tool call being run, if anyone is listening.
/// Informational only: the model sees the tool's `ToolResult`, not this.
pub fn progress_sink() -> Option<ProgressSink> {
    PROGRESS.try_with(Arc::clone).ok()
}

/// What the agent loop knows about the call being run, beyond `ToolCtx`.
struct CallScope {
    id: String,
    consumes: std::sync::Mutex<Vec<i64>>,
}

tokio::task_local! {
    static CALL: Arc<CallScope>;
}

/// Run `fut` (a tool call) as call `id`. Returns its output and the agent
/// mail it asked to consume (see [`consume_mail`]).
pub async fn with_call<F: std::future::Future>(id: &str, fut: F) -> (F::Output, Vec<i64>) {
    let scope = Arc::new(CallScope {
        id: id.to_owned(),
        consumes: std::sync::Mutex::new(Vec::new()),
    });
    let out = CALL.scope(scope.clone(), fut).await;
    let consumes = scope
        .consumes
        .lock()
        .map(|mut c| std::mem::take(&mut *c))
        .unwrap_or_default();
    (out, consumes)
}

/// The id of the tool call being run (the model's `tool_use` id).
pub fn current_call_id() -> Option<String> {
    CALL.try_with(|c| c.id.clone()).ok()
}

/// Mark agent mail as consumed by the running tool call. Takes effect when
/// its journal step completes, in the same transaction.
pub fn consume_mail(ids: &[i64]) {
    let _ = CALL.try_with(|c| {
        if let Ok(mut v) = c.consumes.lock() {
            v.extend_from_slice(ids);
        }
    });
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    /// Third-party text: from an [`Tool::untrusted`] tool (set by the agent
    /// loop), or marked by the tool itself for this result (`channel_read`
    /// returning posts from tainted runs). Journaled with the step, so a
    /// replayed run is tainted at the same step (tooling spec §5.3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untrusted: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            untrusted: false,
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            untrusted: false,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    /// The declared effect, shown in tool lists. Per-call policy is
    /// [`Tool::policy`]'s `effect`.
    fn effect(&self) -> Effect;
    /// Returns third-party text (web pages, mail, chat messages) that may
    /// carry instructions. Its results taint the run and reach the model
    /// wrapped in `<untrusted>` (tooling spec §5.3).
    fn untrusted(&self) -> bool {
        false
    }
    /// The host a call reaches, for tools that fetch from the network. In a
    /// tainted run, a call to a host the run hasn't contacted yet asks
    /// (tooling spec §5.3), so injected text can't send data somewhere new.
    fn egress_host(&self, _args: &Value) -> Option<String> {
        None
    }
    /// Policy for this particular call; may depend on the arguments (e.g.
    /// writes outside the workspace need approval, writes inside don't).
    fn policy(&self, ctx: &ToolCtx, args: &Value) -> ToolPolicy;
    async fn call(&self, ctx: &ToolCtx, args: Value) -> ToolResult;
    /// One-line description of a call, shown in the UI and approval prompts.
    /// `None` uses a generic summary.
    fn summary(&self, _args: &Value) -> Option<String> {
        None
    }
    /// Whether "always allow" may be offered. Tools whose every call deserves
    /// a fresh look (activating agent-written code) return false.
    fn allow_always(&self) -> bool {
        true
    }
}

/// Tools that come from somewhere that has to be started first (an MCP
/// server). `prepare` runs before each run; `tools` returns what is
/// currently available.
#[async_trait]
pub trait ToolSource: Send + Sync {
    async fn prepare(&self);
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    /// Short human-readable status lines for the system prompt and `/tools`.
    fn describe(&self) -> Vec<String> {
        Vec::new()
    }
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    sources: Vec<Arc<dyn ToolSource>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    pub fn add_source(&mut self, source: Arc<dyn ToolSource>) {
        self.sources.push(source);
    }

    /// Start lazy sources (e.g. MCP servers) so their tools are listed.
    pub async fn prepare(&self) {
        for s in &self.sources {
            s.prepare().await;
        }
    }

    pub fn describe_sources(&self) -> Vec<String> {
        self.sources.iter().flat_map(|s| s.describe()).collect()
    }

    /// Every tool by name; built-ins win over source tools with the same name.
    fn all(&self) -> BTreeMap<String, Arc<dyn Tool>> {
        let mut all = BTreeMap::new();
        for s in &self.sources {
            for t in s.tools() {
                all.insert(t.spec().name, t);
            }
        }
        for (name, t) in &self.tools {
            all.insert(name.clone(), t.clone());
        }
        all
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        match self.tools.get(name) {
            Some(t) => Some(t.clone()),
            None => self
                .sources
                .iter()
                .flat_map(|s| s.tools())
                .find(|t| t.spec().name == name),
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.all().into_keys().collect()
    }

    /// Names of the tools registered directly (not from a source such as
    /// an MCP server or the connectors).
    pub fn builtin_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Specs sorted by name, so requests (and their journal hashes) are stable.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.all().values().map(|t| t.spec()).collect()
    }

    /// [`ToolRegistry::specs`] limited to `scope` (tool names; `None` = all
    /// but the sub-agent-only ones, see [`in_scope`]).
    pub fn specs_for(&self, scope: Option<&[String]>) -> Vec<ToolSpec> {
        self.all()
            .into_iter()
            .filter(|(name, _)| in_scope(scope, name))
            .map(|(_, t)| t.spec())
            .collect()
    }

    pub fn allow_always(&self, call: &ToolCall) -> bool {
        self.get(&call.name).is_none_or(|t| t.allow_always())
    }

    /// Declared effect and untrusted flag of every tool, by name.
    pub fn effects(&self) -> BTreeMap<String, (Effect, bool)> {
        self.all()
            .into_iter()
            .map(|(name, t)| (name, (t.effect(), t.untrusted())))
            .collect()
    }

    /// The host `call` reaches ([`Tool::egress_host`]).
    pub fn egress_host(&self, call: &ToolCall) -> Option<String> {
        self.get(&call.name)
            .and_then(|t| t.egress_host(&call.input))
    }

    /// Whether `name` returns third-party text ([`Tool::untrusted`]).
    pub fn untrusted(&self, name: &str) -> bool {
        self.get(name).is_some_and(|t| t.untrusted())
    }

    pub fn summarize(&self, call: &ToolCall) -> String {
        self.get(&call.name)
            .and_then(|t| t.summary(&call.input))
            .unwrap_or_else(|| crate::agent::summarize(call))
    }

    /// Unknown tools need no approval: calling them just returns an error.
    pub fn policy(&self, ctx: &ToolCtx, call: &ToolCall) -> ToolPolicy {
        match self.get(&call.name) {
            Some(t) => t.policy(ctx, &call.input),
            None => ToolPolicy::new(Effect::Read, true),
        }
    }

    pub async fn call(&self, ctx: &ToolCtx, call: &ToolCall) -> ToolResult {
        self.call_in(None, ctx, call).await
    }

    /// Call a tool for a run limited to `scope`. A name outside it is an
    /// error result, so a hallucinated call can't escape the scope.
    pub async fn call_in(
        &self,
        scope: Option<&[String]>,
        ctx: &ToolCtx,
        call: &ToolCall,
    ) -> ToolResult {
        if !in_scope(scope, &call.name) {
            return ToolResult::error(format!(
                "tool {:?} is not available to this agent",
                call.name
            ));
        }
        let Some(tool) = self.get(&call.name) else {
            return ToolResult::error(format!("unknown tool {:?}", call.name));
        };
        let mut result = tool.call(ctx, call.input.clone()).await;
        result.content = truncate_middle(&result.content, MAX_TOOL_OUTPUT);
        result
    }
}

/// Whether `name` is allowed by `scope` (`None` = every tool except the
/// ones only sub-agents get, such as `agent_ask_parent`). An entry ending
/// in `*` allows names it is a prefix of: an agent profile's scope names a
/// connector's or MCP server's tools that way (`mcp__browser__*`).
pub fn in_scope(scope: Option<&[String]>, name: &str) -> bool {
    match scope {
        None => !crate::subagent::CHILD_ONLY_TOOLS.contains(&name),
        Some(s) => s.iter().any(|n| match n.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => n == name,
        }),
    }
}

/// Keep the head and tail of `s` within `max` bytes, marking the cut.
pub fn truncate_middle(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let keep = max.saturating_sub(64) / 2;
    let mut head = keep;
    while !s.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = s.len() - keep;
    while !s.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n\n[... truncated {} bytes ...]\n\n{}",
        &s[..head],
        tail - head,
        &s[tail..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_head_and_tail_on_char_boundaries() {
        let s = format!("{}{}{}", "a".repeat(100), "é".repeat(100), "z".repeat(100));
        let t = truncate_middle(&s, 200);
        assert!(t.len() <= 200 + 40);
        assert!(t.starts_with("aaa"));
        assert!(t.ends_with("zzz"));
        assert!(t.contains("truncated"));
        assert_eq!(truncate_middle("short", 200), "short");
    }
}
