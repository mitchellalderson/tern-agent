//! Sub-agent tools for the parent side (docs/subagents-design.md §2.2):
//! `agent_spawn`, `agent_wait`, `agent_cancel`, `agent_status`. Each is a
//! thin shell over the [`SubagentHost`] bound into the slot at startup.

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_core::subagent::{CancelRequest, HostSlot, SpawnRequest, SubagentHost, WaitRequest};
use tern_core::tool::{Tool, ToolCtx, ToolPolicy, ToolRegistry, ToolResult, current_call_id};
use tern_core::types::ToolSpec;

use crate::{args, spec};

/// Register the parent-side tools, all bound to `slot`.
pub fn register(registry: &mut ToolRegistry, slot: HostSlot) {
    registry.register(std::sync::Arc::new(AgentSpawn(slot.clone())));
    registry.register(std::sync::Arc::new(AgentWait(slot.clone())));
    registry.register(std::sync::Arc::new(AgentCancel(slot.clone())));
    registry.register(std::sync::Arc::new(AgentStatus(slot)));
}

/// Spawning, waiting and cancelling change nothing outside tern's own runs
/// and are safe to repeat after a crash (spawn is keyed by the call id).
fn policy() -> ToolPolicy {
    ToolPolicy {
        idempotent: true,
        requires_approval: false,
    }
}

fn host(slot: &HostSlot) -> Result<&std::sync::Arc<dyn SubagentHost>, ToolResult> {
    slot.get()
        .ok_or_else(|| ToolResult::error("sub-agents are not available in this process"))
}

fn result(r: Result<Value, String>) -> ToolResult {
    match r {
        Ok(v) => ToolResult::ok(v.to_string()),
        Err(e) => ToolResult::error(e),
    }
}

// ---- agent_spawn ----

pub struct AgentSpawn(pub HostSlot);

#[async_trait]
impl Tool for AgentSpawn {
    fn spec(&self) -> ToolSpec {
        spec(
            "agent_spawn",
            "Start a sub-agent on a task; returns immediately with its handle. The sub-agent is a separate instance of you with a fresh context: it sees only `task`. Collect its report with agent_wait.",
            json!({
                "type": "object",
                "required": ["label", "task"],
                "properties": {
                    "label": { "type": "string", "pattern": "^[a-z0-9][a-z0-9-]{0,31}$",
                        "description": "Short name for this sub-agent, unique among yours, e.g. 'pricing-research'." },
                    "task": { "type": "string",
                        "description": "The complete brief. The sub-agent sees nothing else: goal, what you already know, constraints, and the exact shape of the report you want." },
                    "tools": { "description": "'all' (default), 'read_only', or a list of tool names.",
                        "oneOf": [ { "enum": ["all", "read_only"] }, { "type": "array", "items": { "type": "string" } } ] },
                    "model": { "type": "string", "description": "A model alias from the configuration or provider:model. Default: the configured sub-agent model." },
                    "max_iterations": { "type": "integer", "minimum": 1 },
                    "budget_tokens": { "type": "integer", "minimum": 10000 }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        policy()
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let label = args.get("label").and_then(Value::as_str).unwrap_or("?");
        Some(format!("agent_spawn {label}"))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let req: SpawnRequest = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let host = match host(&self.0) {
            Ok(h) => h,
            Err(e) => return e,
        };
        let Some(call_id) = current_call_id() else {
            return ToolResult::error("agent_spawn must be called by the agent loop");
        };
        result(host.spawn(ctx, &call_id, req).await)
    }
}

// ---- agent_wait ----

pub struct AgentWait(pub HostSlot);

#[async_trait]
impl Tool for AgentWait {
    fn spec(&self) -> ToolSpec {
        spec(
            "agent_wait",
            "Block until your sub-agents finish, and get their reports. Returns early if the timeout passes, with the status of those still running. Each report is delivered once.",
            json!({
                "type": "object",
                "properties": {
                    "agents": { "type": "array", "items": { "type": "string" },
                        "description": "Labels to wait for. Default: all of yours that haven't been reported yet." },
                    "mode": { "enum": ["all", "any"], "default": "all",
                        "description": "'all' waits for every one; 'any' returns when the first finishes." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600, "default": 900 }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        policy()
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let which = match args.get("agents").and_then(Value::as_array) {
            Some(a) if !a.is_empty() => a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            _ => "sub-agents".into(),
        };
        Some(format!("agent_wait {which}"))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let req: WaitRequest = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        match host(&self.0) {
            Ok(h) => result(h.wait(ctx, req).await),
            Err(e) => e,
        }
    }
}

// ---- agent_cancel ----

pub struct AgentCancel(pub HostSlot);

#[async_trait]
impl Tool for AgentCancel {
    fn spec(&self) -> ToolSpec {
        spec(
            "agent_cancel",
            "Stop a sub-agent you no longer need (and any it started).",
            json!({
                "type": "object",
                "required": ["agent"],
                "properties": {
                    "agent": { "type": "string", "description": "The sub-agent's label." },
                    "reason": { "type": "string" }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        policy()
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let label = args.get("agent").and_then(Value::as_str).unwrap_or("?");
        Some(format!("agent_cancel {label}"))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let req: CancelRequest = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        match host(&self.0) {
            Ok(h) => result(h.cancel(ctx, req).await),
            Err(e) => e,
        }
    }
}

// ---- agent_status ----

pub struct AgentStatus(pub HostSlot);

#[async_trait]
impl Tool for AgentStatus {
    fn spec(&self) -> ToolSpec {
        spec(
            "agent_status",
            "List your sub-agents with their status, current activity, elapsed time and tokens, without waiting.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        policy()
    }

    async fn call(&self, ctx: &ToolCtx, _raw: Value) -> ToolResult {
        match host(&self.0) {
            Ok(h) => result(h.status(ctx).await),
            Err(e) => e,
        }
    }
}
