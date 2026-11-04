//! Sub-agent tools (docs/subagents-design.md §2.2–§2.3): `agent_spawn`,
//! `agent_wait`, `agent_send`, `agent_cancel`, `agent_status` for the parent
//! side and `agent_ask_parent` for children. Each is a thin shell over the
//! [`SubagentHost`] bound into the slot at startup.

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_core::subagent::{
    AskRequest, CancelRequest, HostSlot, SendRequest, SpawnRequest, SubagentHost, WaitRequest,
};
use tern_core::tool::{
    Effect, Tool, ToolCtx, ToolPolicy, ToolRegistry, ToolResult, current_call_id,
};
use tern_core::types::ToolSpec;

use crate::{args, spec};

/// Register the sub-agent tools, all bound to `slot`. Which runs see which
/// is decided by tool scope (`agent_ask_parent` only for children).
pub fn register(registry: &mut ToolRegistry, slot: HostSlot) {
    registry.register(std::sync::Arc::new(AgentSpawn(slot.clone())));
    registry.register(std::sync::Arc::new(AgentWait(slot.clone())));
    registry.register(std::sync::Arc::new(AgentSend(slot.clone())));
    registry.register(std::sync::Arc::new(AgentCancel(slot.clone())));
    registry.register(std::sync::Arc::new(AgentStatus(slot.clone())));
    registry.register(std::sync::Arc::new(AgentAskParent(slot)));
}

/// Spawning, waiting and cancelling change nothing outside tern's own runs
/// and are safe to repeat after a crash (spawn is keyed by the call id).
/// Writes are granted by the host: sub-agents' own calls go through the
/// same policy.
fn policy(effect: Effect) -> ToolPolicy {
    ToolPolicy {
        granted: true,
        ..ToolPolicy::new(effect, true)
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
    fn effect(&self) -> Effect {
        Effect::Write
    }

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
                    "max_iterations": { "type": "integer", "minimum": 1,
                        "description": "Steps for this sub-agent. Default and ceiling are configured; ask for more when it must read or try a lot." },
                    "budget_tokens": { "type": "integer", "minimum": 10000,
                        "description": "Tokens for this sub-agent (input + output + cache, counted on every step). Ask for more when it must read a lot, or when retrying one that ran out." },
                    "post": { "type": "boolean",
                        "description": "Let it post progress to this conversation's channel with channel_post, and post its report there when it finishes. For long background work the user should see as it goes. Default false: it reports only to you." }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        policy(self.effect())
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
    fn effect(&self) -> Effect {
        Effect::Read
    }

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
        policy(self.effect())
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
    fn effect(&self) -> Effect {
        Effect::Write
    }

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
        policy(self.effect())
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
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "agent_status",
            "List your sub-agents with their status, current activity, elapsed time and tokens, without waiting.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        policy(self.effect())
    }

    async fn call(&self, ctx: &ToolCtx, _raw: Value) -> ToolResult {
        match host(&self.0) {
            Ok(h) => result(h.status(ctx).await),
            Err(e) => e,
        }
    }
}

// ---- agent_send ----

pub struct AgentSend(pub HostSlot);

#[async_trait]
impl Tool for AgentSend {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "agent_send",
            "Answer a sub-agent's question, or redirect a running sub-agent. A sub-agent waiting on a question gets the message as the answer; otherwise it gets it before its next step.",
            json!({
                "type": "object",
                "required": ["agent", "message"],
                "properties": {
                    "agent": { "type": "string", "description": "The sub-agent's label." },
                    "message": { "type": "string" },
                    "question_id": { "type": "string",
                        "description": "The id of the question you are answering (e.g. 'q_17'), from agent_wait or the sub-agent's message." }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        policy(self.effect())
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let label = args.get("agent").and_then(Value::as_str).unwrap_or("?");
        Some(format!("agent_send {label}"))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let req: SendRequest = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let host = match host(&self.0) {
            Ok(h) => h,
            Err(e) => return e,
        };
        let Some(call_id) = current_call_id() else {
            return ToolResult::error("agent_send must be called by the agent loop");
        };
        result(host.send(ctx, &call_id, req).await)
    }
}

// ---- agent_ask_parent ----

pub struct AgentAskParent(pub HostSlot);

#[async_trait]
impl Tool for AgentAskParent {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "agent_ask_parent",
            "Ask the agent that started you a question and wait for its answer. Only for decisions that are really its call; otherwise make a sensible assumption and say so in your report.",
            json!({
                "type": "object",
                "required": ["question"],
                "properties": {
                    "question": { "type": "string", "description": "One clear question." },
                    "recommendation": { "type": "string",
                        "description": "The answer you would choose, used if no answer comes in time." }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        policy(self.effect())
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let q = args.get("question").and_then(Value::as_str).unwrap_or("?");
        let q: String = q.chars().take(80).collect();
        Some(format!("agent_ask_parent {q}"))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let req: AskRequest = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let host = match host(&self.0) {
            Ok(h) => h,
            Err(e) => return e,
        };
        let Some(call_id) = current_call_id() else {
            return ToolResult::error("agent_ask_parent must be called by the agent loop");
        };
        result(host.ask_parent(ctx, &call_id, req).await)
    }
}
