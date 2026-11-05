//! Code auto mode's side of the agent loop (docs/alpha-coding-agent.md §3.5):
//! the snapshot before a run's first unasked change, and the ledger row for
//! every call the mode let through.

use serde_json::json;
use tern_store::{ACTOR_TERN, LedgerEntry, RunRow};

use super::{Agent, AgentEvent};
use crate::approval::{self, ApprovalOutcome, Autonomy, Gate};
use crate::checkpoint::{self, Checkpoint, Skipped};
use crate::journal::StepError;
use crate::tool::{Effect, Reach, ToolCtx, ToolPolicy};
use crate::types::ToolCall;

/// What a run's snapshot attempt came to.
pub(super) type Snapshot = Result<Checkpoint, Skipped>;

/// Whether `call` runs unasked because the run is in code auto mode, as
/// opposed to a grant, a policy default or a person: a change (not a read)
/// that either is a shell command the classifier cleared inside a verified
/// sandbox, or a workspace-bound file write.
pub(super) fn by_code_mode(tool: &str, policy: &ToolPolicy, gate: Gate) -> bool {
    gate.autonomy == Autonomy::Code
        && !gate.tainted
        && policy.effect != Effect::Read
        && !policy.requires_approval
        && !policy.unbounded
        && (policy.auto_code || (policy.granted && policy.reach == Reach::Workspace))
        && approval::check(tool, policy, false, gate) == approval::Check::Allow
}

impl Agent {
    /// A call just ran unasked under code auto mode: snapshot first (once per
    /// run), then record it. Emits the `Approval` event so front ends can badge
    /// the row.
    pub(super) async fn code_mode_step(
        &self,
        run: &RunRow,
        call: &ToolCall,
        ctx: &ToolCtx,
    ) -> Result<(), StepError> {
        let snap = self.snapshot_once(run, ctx).await?;
        let command = (call.name == "shell")
            .then(|| call.input["command"].as_str().map(str::to_owned))
            .flatten();
        let e = LedgerEntry::new(
            "approval",
            &call.name,
            self.journal.redact_text(&format!(
                "ran under code auto mode: {}",
                self.tools.summarize(call)
            )),
        )
        .run(&run.id, &run.session_id)
        .actor(ACTOR_TERN)
        .detail(json!({
            "decision": "approved_by_mode",
            "reason": approval::CODE_MODE_REASON,
            "mode": "code",
            "workspace_id": ctx.scope.id(),
            "root": ctx.scope.root().display().to_string(),
            "sandbox": (call.name == "shell").then(|| ctx.scope.shell_bound().as_str()),
            "command": command.map(|c| self.journal.redact_text(&c)),
            "checkpoint": snap.as_ref().ok().map(|c| c.commit.clone()),
            "agent": run.label,
        }));
        self.store.ledger_append(&e)?;
        self.emit(AgentEvent::Approval {
            run_id: run.id.clone(),
            id: call.id.clone(),
            name: call.name.clone(),
            outcome: ApprovalOutcome::ApprovedByMode,
        });
        Ok(())
    }

    /// The run's snapshot, taken the first time this is called for the run.
    async fn snapshot_once(&self, run: &RunRow, ctx: &ToolCtx) -> Result<Snapshot, StepError> {
        let known = self
            .scoped
            .lock()
            .ok()
            .and_then(|g| g.snapshots.get(&run.id).cloned());
        if let Some(s) = known {
            return Ok(s);
        }
        let snap: Snapshot = match self.cfg.checkpoints.clone() {
            None => Err(Skipped::Failed {
                message: "snapshots aren't configured".into(),
            }),
            Some(base) => {
                let (id, root, run_id) = (
                    ctx.scope.id().to_owned(),
                    ctx.scope.root().to_path_buf(),
                    run.id.clone(),
                );
                tokio::task::spawn_blocking(move || checkpoint::create(&base, &id, &root, &run_id))
                    .await
                    .unwrap_or_else(|e| {
                        Err(Skipped::Failed {
                            message: e.to_string(),
                        })
                    })
            }
        };
        if let Ok(mut g) = self.scoped.lock() {
            g.snapshots.insert(run.id.clone(), snap.clone());
        }
        let summary = match &snap {
            Ok(c) => format!("saved a snapshot before this run's changes ({})", &c.commit[..c.commit.len().min(10)]),
            Err(s) => s.describe(),
        };
        let e = LedgerEntry::new("code_auto.checkpoint", "checkpoint", summary)
            .run(&run.id, &run.session_id)
            .actor(ACTOR_TERN)
            .detail(json!({
                "workspace_id": ctx.scope.id(),
                "root": ctx.scope.root().display().to_string(),
                "commit": snap.as_ref().ok().map(|c| c.commit.clone()),
                "skipped": snap.as_ref().err(),
            }));
        self.store.ledger_append(&e)?;
        self.emit(AgentEvent::Checkpoint {
            run_id: run.id.clone(),
            commit: snap.as_ref().ok().map(|c| c.commit.clone()),
            skipped: snap.as_ref().err().map(Skipped::describe),
        });
        Ok(snap)
    }
}
