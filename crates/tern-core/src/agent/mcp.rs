//! Calls from MCP clients (`tern mcp serve`, T6d). Each call is a small
//! system run in the client's session, trigger [`TRIGGER`], with one
//! `mcp_call` step: the effect policy and the user's answer
//! ([`Agent::decide`], so prompts land in tern and an unanswered write goes
//! to the outbox), then the call. No model runs.
//!
//! The run starts tainted ([`TAINT`]): the arguments come from another
//! model, which may have read anything. So every `write` and `destructive`
//! call asks, no standing grant covers it, and "always" isn't offered.

use serde::{Deserialize, Serialize};
use tern_store::RunRow;
use tokio_util::sync::CancellationToken;

use super::{Agent, AgentError, INTERRUPTED_TOOL_MESSAGE, RunOutcome, RunStatus, preview, taint};
use crate::approval::{self, ApprovalOutcome, Autonomy, Gate};
use crate::events::AgentEvent;
use crate::journal::{InFlight, StepError, StepKind, StepOutput};
use crate::tool::{ToolCtx, ToolResult, with_call};
use crate::types::{ContentBlock, Message, ToolCall};

/// `runs.trigger` of a call from an MCP client.
pub const TRIGGER: &str = "mcp";

/// `sessions.source` of an MCP client's session.
pub const SOURCE: &str = "mcp";

/// The taint every MCP call starts with.
pub const TAINT: &str = "a call from an MCP client";

/// What an `mcp_call` step records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpCallOutput {
    /// How the approval went, when the policy asked or refused; `None`
    /// when the call ran without asking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalOutcome>,
    pub result: ToolResult,
}

/// An `mcp` run's input message: a line for people, then the call.
pub fn input_text(client: &str, call: &ToolCall) -> String {
    let call = serde_json::to_string(call).unwrap_or_default();
    format!("[{client} called a tool through tern mcp serve]\n{call}")
}

/// The call in an `mcp` run's input message ([`input_text`]).
pub fn call_of(text: &str) -> Option<ToolCall> {
    let (_, json) = text.split_once('\n')?;
    serde_json::from_str(json).ok()
}

impl Agent {
    /// Drive an `mcp` run: decide, then call, in one journaled step.
    pub(super) async fn mcp_exec(
        &self,
        run: &RunRow,
        ctx: &ToolCtx,
        cancel: &CancellationToken,
        outcome: &mut RunOutcome,
    ) -> Result<RunStatus, AgentError> {
        // The input is the last block: earlier calls' messages merge into
        // the same user message.
        let call = self
            .context(run, 0)?
            .last()
            .and_then(|m| match m.content.last() {
                Some(ContentBlock::Text { text }) => call_of(text),
                _ => None,
            })
            .ok_or_else(|| AgentError::Corrupt(format!("run {} carries no MCP call", run.id)))?;
        let scope: Option<Vec<String>> = run
            .tool_scope
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| AgentError::Corrupt(format!("run {} tool_scope: {e}", run.id)))?;
        let scope = scope.as_deref();
        let mut taint = taint::Taint::at_start(run);
        let gate = Gate {
            tainted: taint.is_set(),
            autonomy: Autonomy::of_run(run.autonomy.as_deref()),
        };
        let runs = crate::tool::in_scope(scope, &call.name) && self.tools.get(&call.name).is_some();
        let mut policy = if runs {
            self.tools.policy(ctx, &call)
        } else {
            crate::tool::ToolPolicy::new(crate::tool::Effect::Read, true)
        };
        let host = runs.then(|| self.tools.egress_host(&call)).flatten();
        let limited = taint.limits(host.as_deref());
        let egress = (limited && !approval::needs_check(&call.name, &policy, gate))
            .then_some(host.as_deref())
            .flatten();
        if limited {
            policy = policy.forced(true);
        }
        let summary = self.tools.summarize(&call);
        let in_flight = if policy.idempotent {
            InFlight::Retry
        } else {
            let r = ToolResult::error(INTERRUPTED_TOOL_MESSAGE);
            let note = note(&summary, &call.name, &r);
            InFlight::Substitute(
                StepOutput::new(McpCallOutput {
                    approval: None,
                    result: r,
                })
                .with_messages(vec![note]),
            )
        };
        let step = self
            .journal
            .step_with(&run.id, 0, StepKind::McpCall, &call, in_flight, || async {
                let mut asked = None;
                if approval::needs_check(&call.name, &policy, gate) {
                    let (decision, why) = self
                        .decide(
                            run,
                            &call,
                            &policy,
                            gate,
                            taint.reason(),
                            egress,
                            None,
                            cancel,
                        )
                        .await?;
                    asked = Some(decision);
                    if !decision.runs() {
                        let r = ToolResult::error(why);
                        let note = note(&summary, &call.name, &r);
                        return Ok(StepOutput::new(McpCallOutput {
                            approval: asked,
                            result: r,
                        })
                        .with_messages(vec![note]));
                    }
                }
                self.emit(AgentEvent::ToolStarted {
                    run_id: run.id.clone(),
                    id: call.id.clone(),
                    name: call.name.clone(),
                    summary: summary.clone(),
                });
                let mut r = with_call(&call.id, self.tools.call_in(scope, ctx, &call))
                    .await
                    .0;
                r.untrusted |= runs && self.tools.untrusted(&call.name);
                let note = note(&summary, &call.name, &r);
                Ok(StepOutput::new(McpCallOutput {
                    approval: asked,
                    result: r,
                })
                .with_messages(vec![note]))
            })
            .await;
        let step = match step {
            Ok(s) => s,
            Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
            Err(e) => return Err(e.into()),
        };
        let McpCallOutput { approval, result } = step.value;
        if let Some(outcome) = approval {
            self.emit(AgentEvent::Approval {
                run_id: run.id.clone(),
                id: call.id.clone(),
                name: call.name.clone(),
                outcome,
            });
        }
        let ran = approval.is_none_or(ApprovalOutcome::runs);
        if ran {
            self.emit(AgentEvent::ToolFinished {
                run_id: run.id.clone(),
                id: call.id.clone(),
                name: call.name.clone(),
                is_error: result.is_error,
                preview: preview(&result.content),
                output: result.content.clone(),
                replayed: step.replayed,
            });
        }
        if result.untrusted {
            taint.mark(&self.store, &run.id, &taint::tool_reason(&call.name))?;
        }
        tracing::info!(run = %run.id, tool = %call.name, approval = ?approval, is_error = result.is_error, "MCP call finished");
        outcome.final_text = result.content.clone();
        outcome.tool_result = Some(result);
        Ok(RunStatus::Completed)
    }
}

/// The message an `mcp_call` step adds to the session, so it reads as a
/// transcript (and a later conversation there knows what happened). Plain
/// text: there is no `tool_use` to pair a tool result with.
fn note(summary: &str, tool: &str, r: &ToolResult) -> Message {
    let content = if r.untrusted {
        taint::wrap(tool, &r.content)
    } else {
        r.content.clone()
    };
    let head = if r.is_error {
        format!("[MCP call {summary}: failed or not run]")
    } else {
        format!("[MCP call {summary}: ran]")
    };
    Message::user_text(format!("{head}\n{content}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_message_carries_the_call() {
        let call = ToolCall {
            id: "mcp_1".into(),
            name: "lua__notes__add".into(),
            input: serde_json::json!({ "text": "line one\nline two" }),
        };
        let text = input_text("Claude Desktop", &call);
        assert!(text.starts_with("[Claude Desktop called a tool"));
        assert_eq!(call_of(&text), Some(call));
        assert_eq!(call_of("no call here"), None);
    }
}
