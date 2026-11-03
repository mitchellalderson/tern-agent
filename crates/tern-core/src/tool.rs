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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolPolicy {
    /// Safe to re-run after a crash interrupted it.
    pub idempotent: bool,
    pub requires_approval: bool,
}

pub struct ToolCtx {
    pub workspace: PathBuf,
    pub run_id: String,
    pub session_id: String,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
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

    /// Specs sorted by name, so requests (and their journal hashes) are stable.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.all().values().map(|t| t.spec()).collect()
    }

    pub fn allow_always(&self, call: &ToolCall) -> bool {
        self.get(&call.name).is_none_or(|t| t.allow_always())
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
            None => ToolPolicy {
                idempotent: true,
                requires_approval: false,
            },
        }
    }

    pub async fn call(&self, ctx: &ToolCtx, call: &ToolCall) -> ToolResult {
        let Some(tool) = self.get(&call.name) else {
            return ToolResult::error(format!("unknown tool {:?}", call.name));
        };
        let mut result = tool.call(ctx, call.input.clone()).await;
        result.content = truncate_middle(&result.content, MAX_TOOL_OUTPUT);
        result
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
