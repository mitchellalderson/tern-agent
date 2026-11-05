//! What the agent loop adds to the activity ledger (tooling spec §8.1):
//! the user's approval decisions, refused calls, queued outbox items, and
//! every `write` or `destructive` call that ran. The store adds the rest
//! (grants, outbox decisions, connections) as it makes those changes.

use serde_json::json;
use tern_store::{ACTOR_TERN, LedgerEntry, RunRow};

use super::Agent;
use crate::approval::{ApprovalRequest, Decision};
use crate::journal::StepError;
use crate::tool::Effect;
use crate::types::ToolCall;

/// The connector or MCP server a tool belongs to (`lua__gmail__send`,
/// `mcp__github__create_issue`).
pub(crate) fn tool_connector(tool: &str) -> Option<&str> {
    let rest = tool
        .strip_prefix("lua__")
        .or_else(|| tool.strip_prefix("mcp__"))?;
    rest.split("__").next().filter(|n| !n.is_empty())
}

/// How a decision reads in the ledger.
pub(crate) fn decision_label(d: Option<Decision>) -> &'static str {
    match d {
        Some(Decision::AllowOnce) => "allowed once",
        Some(Decision::AlwaysAllow) => "always allowed",
        Some(Decision::AllowRun) => "allowed for this run",
        Some(Decision::AlwaysAllowHost) => "always allowed the site",
        Some(Decision::AllowSession) => "allowed for this conversation",
        Some(Decision::CoveredByGrant) => "covered by a grant",
        Some(Decision::Deny) => "denied",
        Some(Decision::Unanswered) => "unanswered",
        None => "timed out",
    }
}

impl Agent {
    fn ledger_entry(
        &self,
        kind: &str,
        run: &RunRow,
        call: &ToolCall,
        summary: String,
    ) -> LedgerEntry {
        let mut e = LedgerEntry::new(kind, &call.name, self.journal.redact_text(&summary))
            .run(&run.id, &run.session_id);
        if let Some(c) = tool_connector(&call.name) {
            e = e.connector(c);
        }
        e
    }

    /// The user answered (or didn't answer) an approval prompt.
    pub(super) fn ledger_decision(
        &self,
        run: &RunRow,
        req: &ApprovalRequest,
        decision: Option<Decision>,
    ) -> Result<(), StepError> {
        let label = decision_label(decision);
        let e = self
            .ledger_entry(
                "approval",
                run,
                &req.call,
                format!("{label}: {}", req.headline()),
            )
            .detail(json!({
                "decision": label,
                "effect": req.effect.as_str(),
                "pattern": req.pattern.as_ref().filter(|_| decision == Some(Decision::AlwaysAllow)),
                // What a scoped answer covers: this key, in that scope.
                "grant": req.grant.as_ref().filter(|_| matches!(
                    decision,
                    Some(
                        Decision::AllowRun
                            | Decision::AllowSession
                            | Decision::AlwaysAllow
                            | Decision::CoveredByGrant
                    )
                )),
                "taint": req.taint,
                "egress_host": req.egress_host,
                "script": req.script_note(),
                "agent": req.agent_label,
            }));
        self.store.ledger_append(&e)?;
        Ok(())
    }

    /// A call refused without asking (`money_movement`, `trade`).
    pub(super) fn ledger_refused(
        &self,
        run: &RunRow,
        call: &ToolCall,
        flag: &str,
    ) -> Result<(), StepError> {
        let summary = format!("refused {} (flagged {flag})", self.tools.summarize(call));
        let e = self
            .ledger_entry("approval", run, call, summary)
            .actor(ACTOR_TERN)
            .detail(json!({ "decision": "refused", "flag": flag }));
        self.store.ledger_append(&e)?;
        Ok(())
    }

    /// A call nobody answered went to the outbox.
    pub(super) fn ledger_queued(
        &self,
        run: &RunRow,
        req: &ApprovalRequest,
        id: i64,
    ) -> Result<(), StepError> {
        let e = self
            .ledger_entry(
                "outbox.queue",
                run,
                &req.call,
                format!("queued outbox #{id}: {}", req.headline()),
            )
            .actor(ACTOR_TERN)
            .detail(json!({ "outbox_id": id, "effect": req.effect.as_str(), "taint": req.taint }));
        self.store.ledger_append(&e)?;
        Ok(())
    }

    /// A `write` or `destructive` call ran (from the run, or from the
    /// outbox once approved). Reads aren't recorded.
    pub(super) fn ledger_effect(
        &self,
        run: &RunRow,
        call: &ToolCall,
        effect: Effect,
        summary: &str,
        is_error: bool,
        outbox: Option<i64>,
    ) -> Result<(), StepError> {
        if effect == Effect::Read {
            return Ok(());
        }
        let (agent, _) = self.store.run_agent(&run.id)?;
        let activation = call.name == "connector_activate";
        let kind = if activation {
            "connector.activate"
        } else {
            "effect"
        };
        let outcome = if is_error { " (failed)" } else { "" };
        let mut e = self
            .ledger_entry(kind, run, call, format!("{effect} · {summary}{outcome}"))
            .actor(&format!("agent:{agent}"))
            .detail(json!({
                "effect": effect.as_str(),
                "error": is_error,
                "outbox_id": outbox,
                "agent_label": run.label,
            }));
        if activation && let Some(name) = call.input.get("name").and_then(|v| v.as_str()) {
            e = e.connector(name);
        }
        self.store.ledger_append(&e)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_name_their_connector() {
        assert_eq!(tool_connector("lua__gmail__send"), Some("gmail"));
        assert_eq!(tool_connector("mcp__github__create_issue"), Some("github"));
        assert_eq!(tool_connector("write_file"), None);
        assert_eq!(tool_connector("lua__"), None);
    }
}
