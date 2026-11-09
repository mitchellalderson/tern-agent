//! Deferred approvals (tooling spec §5.2). [`Agent::defer`] queues a call
//! nobody approved in time; [`Agent::outbox_exec`] drives the system run
//! that executes an approved item. Its one `outbox_exec` step is journaled
//! like a tool step, so a crash never runs the call twice (a non-idempotent
//! call cut off mid-way is recorded as interrupted, not retried).

use serde_json::{Value, json};
use tern_store::{NewMail, NewOutbox, RunRow};

use super::{Agent, AgentError, INTERRUPTED_TOOL_MESSAGE, RunOutcome, RunStatus, preview, taint};
use crate::approval::ApprovalRequest;
use crate::events::AgentEvent;
use crate::journal::{InFlight, StepError, StepKind, StepOutput};
use crate::outbox;
use crate::tool::{ToolCtx, ToolResult, with_call};
use crate::types::{Message, ToolCall};

impl Agent {
    /// Queue `req`'s call as an outbox item (idempotent per call, so a
    /// retried approval step finds the same item). Returns its id.
    pub(super) async fn defer(
        &self,
        run: &RunRow,
        req: &ApprovalRequest,
    ) -> Result<i64, StepError> {
        let expires_at = tern_store::now_ms() + self.cfg.outbox_expiry.as_millis() as i64;
        // A file edit remembers the file as it is now.
        let file = outbox::file_target(&req.call.name, &req.call.input, self.scope_for(run).cwd());
        let file_path = file.as_ref().map(|p| p.to_string_lossy().into_owned());
        let file_hash = file.as_deref().map(outbox::file_hash);
        let item = self.store.add_outbox(&NewOutbox {
            run_id: &run.id,
            session_id: &run.session_id,
            call_id: &req.call.id,
            tool: &req.call.name,
            input_json: &req.call.input.to_string(),
            summary: &req.summary,
            effect: req.effect.as_str(),
            taint: req.taint.as_deref(),
            local_only: req.local_only,
            expires_at,
            file_path: file_path.as_deref(),
            file_hash: file_hash.as_deref(),
        })?;
        tracing::info!(run = %run.id, tool = %req.call.name, outbox = item.id, "nobody answered; queued in the outbox");
        self.ledger_queued(run, req, item.id)?;
        self.approvals.deferred(req, item.id).await;
        Ok(item.id)
    }

    /// Drive the system run that executes an approved outbox item.
    pub(super) async fn outbox_exec(
        &self,
        run: &RunRow,
        ctx: &ToolCtx,
        outcome: &mut RunOutcome,
    ) -> Result<RunStatus, AgentError> {
        let item = self.store.outbox_by_exec_run(&run.id)?.ok_or_else(|| {
            AgentError::Corrupt(format!("run {} executes no outbox item", run.id))
        })?;
        let input: Value = serde_json::from_str(item.effective_input())
            .map_err(|e| AgentError::Corrupt(format!("outbox #{}: {e}", item.id)))?;
        let call = ToolCall {
            id: item.call_id.clone(),
            name: item.tool.clone(),
            input,
        };
        // Edits to one file run in queue order, one at a time: wait for any
        // earlier approved item on it to finish.
        if let Some(path) = item.file_path.as_deref() {
            while self.store.earlier_outbox_running(item.id, path)? {
                if ctx.cancel.is_cancelled() {
                    return Ok(RunStatus::Interrupted);
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
        let policy = self.tools.policy(ctx, &call);
        let in_flight = if policy.idempotent {
            InFlight::Retry
        } else {
            let r = ToolResult::error(INTERRUPTED_TOOL_MESSAGE);
            let note = exec_message(item.id, &item.summary, &call.name, &r, false);
            InFlight::Substitute(StepOutput::new(r).with_messages(vec![note]))
        };
        self.emit(AgentEvent::ToolStarted {
            run_id: run.id.clone(),
            id: call.id.clone(),
            name: call.name.clone(),
            summary: item.summary.clone(),
        });
        let stale_text = item
            .file_path
            .as_deref()
            .map(outbox::stale_message)
            .unwrap_or_default();
        let step_input = json!({ "outbox_id": item.id, "call": call });
        let result = self
            .journal
            .step_with(
                &run.id,
                0,
                StepKind::OutboxExec,
                &step_input,
                in_flight,
                || async {
                    // Nothing the user approves runs a forbidden tool.
                    let stale =
                        outbox::file_changed(item.file_path.as_deref(), item.file_hash.as_deref());
                    let mut r = match policy.forbidden {
                        Some(flag) => ToolResult::error(crate::approval::forbidden_message(flag)),
                        // The file changed since it was queued: don't run it.
                        None if stale => ToolResult::error(stale_text.clone()),
                        None => with_call(&call.id, self.tools.call(ctx, &call)).await.0,
                    };
                    r = r.with_trust(self.tools.trust_floor(&call.name));
                    let note = exec_message(item.id, &item.summary, &call.name, &r, stale);
                    Ok(StepOutput::new(r).with_messages(vec![note]))
                },
            )
            .await;
        let result = match result {
            Ok(r) => r,
            Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
            Err(e) => return Err(e.into()),
        };
        let r = &result.value;
        let stale = r.is_error && !stale_text.is_empty() && r.content == stale_text;
        if !result.replayed && !stale {
            self.ledger_effect(
                run,
                &call,
                policy.effect,
                &item.summary,
                r.is_error,
                Some(item.id),
            )?;
        }
        self.emit(AgentEvent::ToolFinished {
            run_id: run.id.clone(),
            id: call.id.clone(),
            name: call.name.clone(),
            is_error: r.is_error,
            preview: preview(&r.content),
            output: r.content.clone(),
            replayed: result.replayed,
        });
        let result_json = serde_json::to_string(r).map_err(StepError::from)?;
        if stale {
            self.store.mark_outbox_stale(item.id, &result_json)?;
            tracing::info!(run = %run.id, outbox = item.id, tool = %call.name, "outbox item skipped: its file changed");
        } else {
            self.store
                .finish_outbox(item.id, r.is_error, &result_json)?;
            tracing::info!(run = %run.id, outbox = item.id, tool = %call.name, is_error = r.is_error, "outbox item executed");
        }
        // Tell the run that queued it, if it is still working.
        if let Some(done) = self.store.outbox_item(item.id)? {
            tell_run(&self.store, &done, Some(&preview(&r.content)))?;
        }
        outcome.final_text = format!(
            "Outbox #{} {}: {}",
            item.id,
            if stale {
                "skipped"
            } else if r.is_error {
                "failed"
            } else {
                "ran"
            },
            item.summary
        );
        Ok(RunStatus::Completed)
    }
}

/// The message an `outbox_exec` step adds to the session, so later runs
/// there know the call was made. Plain text: there is no `tool_use` to pair
/// a tool result with.
fn exec_message(id: i64, summary: &str, tool: &str, r: &ToolResult, stale: bool) -> Message {
    let content = taint::frame(tool, None, r);
    let head = if stale {
        format!(
            "[outbox #{id}: the user approved {summary}, but the file changed since it was queued, so it didn't run]"
        )
    } else if r.is_error {
        format!("[outbox #{id}: the user approved {summary}, and it failed]")
    } else {
        format!("[outbox #{id}: the user approved {summary}, and it ran]")
    };
    Message::user_text(format!("{head}\n{content}"))
}

/// Mail the run that queued `item` how it was decided, if that run is still
/// working (it reads it at its next step). Once per item and state.
pub fn tell_run(
    store: &tern_store::Store,
    item: &tern_store::OutboxRow,
    result: Option<&str>,
) -> Result<(), tern_store::StoreError> {
    let running = store
        .run(&item.run_id)?
        .is_some_and(|r| r.status == RunStatus::Running.as_str() || r.status == "queued");
    if !running {
        return Ok(());
    }
    let text = outbox::decided_note(item, result);
    store.post_mail(&NewMail {
        to_run_id: &item.run_id,
        from_run_id: None,
        kind: outbox::MAIL_KIND,
        body: &json!({ "text": text }).to_string(),
        dedupe_key: &format!("outbox:{}:{}", item.id, item.state),
    })?;
    Ok(())
}
