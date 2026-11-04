//! The agent loop (spec §6). Every model call, approval, and tool call goes
//! through the journal, so a run can be resumed after a crash by driving it
//! again from step 0: completed steps replay from storage.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use tern_store::{RunRow, Store};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::approval::{
    self, ApprovalHandler, ApprovalOutcome, ApprovalRequest, Autonomy, Decision, Gate,
};
use crate::compaction;
use crate::config::ModelRef;
use crate::events::AgentEvent;
use crate::journal::{InFlight, Journal, StepError, StepKind, StepOutput};
use crate::memory;
use crate::subagent::{HostSlot, InboxEntry, InboxOutput};
use crate::tool::{ProgressSink, ToolCtx, ToolRegistry, ToolResult, with_call, with_progress};
use crate::types::{
    AssistantTurn, ChatRequest, ContentBlock, Message, Provider, ProviderError, Role, StopReason,
    StreamEvent, ToolCall, Usage,
};

mod carry;
mod channel;
mod ledger;
mod script;
pub use carry::CarryOutput;
pub use channel::DigestOutput;
pub mod mcp;
mod outbox;
pub use outbox::tell_run as tell_outbox_run;
pub mod taint;
use taint::Taint;

pub const MAX_LLM_ATTEMPTS: u32 = 5;
/// The result of a tool call cut off by the output limit. Its arguments are
/// incomplete, so it is never run.
pub const TRUNCATED_TOOL_MESSAGE: &str = "not run: your reply hit the output token limit before this call's arguments were complete, so they were cut off. Nothing ran. Send less in one call: split a long file into a smaller first version and add to it, or drop what isn't needed";
pub const INTERRUPTED_TOOL_MESSAGE: &str = "interrupted: this call may have partially executed before a crash; verify state before retrying";
/// Starts the note appended to the final text of a run stopped by a limit.
pub const STOPPED_MARKER: &str = "[stopped: ";
/// Sent with the closing turn of a run that reached a limit, so it reports
/// what it has instead of stopping mid-task with nothing to show.
pub const WRAP_UP_MESSAGE: &str = "You have used up your {limit}, so you can't call any more tools. Write your final reply now from what you have: lead with the answer or result in the shape you were asked for, then say what you checked, what is unfinished and what you would do next.";

/// Resolves a `provider:model` reference to the provider that serves it:
/// a fallback chain when config lists one for that model (T6a).
pub type ProviderFactory =
    Arc<dyn Fn(&ModelRef) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub workspace: PathBuf,
    pub max_iterations: u32,
    pub max_tokens: u32,
    pub approval_timeout: Duration,
    /// Base delay for LLM retry backoff (doubles per attempt, with jitter).
    pub retry_base: Duration,
    /// Context window in tokens, by provider name.
    pub context_windows: std::collections::BTreeMap<String, u64>,
    /// Context window for providers not in `context_windows`.
    pub default_context_window: u64,
    /// Flush durable facts to memory before compacting (`[memory].flush`).
    pub memory_flush: bool,
    /// How long a `write`/`destructive` approval waits before it goes to
    /// the outbox, where the approval handler can defer (`[outbox]`).
    pub approval_wait: Duration,
    /// How long an outbox item waits for the user.
    pub outbox_expiry: Duration,
    /// What one `python` script may do (`[python]`).
    pub script: crate::script::ScriptLimits,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            max_iterations: 40,
            max_tokens: 8192,
            approval_timeout: Duration::from_secs(600),
            retry_base: Duration::from_secs(1),
            context_windows: std::collections::BTreeMap::new(),
            default_context_window: 128_000,
            memory_flush: false,
            approval_wait: Duration::from_secs(120),
            outbox_expiry: Duration::from_secs(72 * 60 * 60),
            script: crate::script::ScriptLimits::default(),
        }
    }
}

impl AgentConfig {
    pub fn context_window(&self, provider: &str) -> u64 {
        self.context_windows
            .get(provider)
            .copied()
            .unwrap_or(self.default_context_window)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct RunOutcome {
    pub run_id: String,
    pub status: RunStatus,
    /// Text of the last assistant message.
    pub final_text: String,
    pub error: Option<String>,
    pub usage: Usage,
    /// A scheduled run that completed with `NO_REPLY`: deliver nothing.
    #[serde(default)]
    pub quiet: bool,
    /// An `mcp` run's one call: what the MCP client gets back (T6d).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<ToolResult>,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("run {0} not found")]
    RunNotFound(String),
    #[error("run {0} is {1}, not resumable")]
    NotResumable(String, String),
    #[error(transparent)]
    Store(#[from] tern_store::StoreError),
    #[error(transparent)]
    Step(#[from] StepError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("corrupt stored data: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
}

pub struct Agent {
    store: Store,
    journal: Journal,
    providers: ProviderFactory,
    tools: Arc<ToolRegistry>,
    approvals: Arc<dyn ApprovalHandler>,
    events: broadcast::Sender<AgentEvent>,
    cfg: AgentConfig,
    subagents: HostSlot,
}

/// What the journal stores as an `llm` step's input: a digest of the full
/// request (so any change is detected) without storing the whole history
/// again for every step.
#[derive(Serialize)]
struct LlmStepInput<'a> {
    model: &'a str,
    messages: usize,
    request_digest: String,
}

impl Agent {
    pub fn new(
        journal: Journal,
        providers: ProviderFactory,
        tools: Arc<ToolRegistry>,
        approvals: Arc<dyn ApprovalHandler>,
        cfg: AgentConfig,
    ) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            store: journal.store().clone(),
            journal,
            providers,
            tools,
            approvals,
            events,
            cfg,
            subagents: HostSlot::default(),
        }
    }

    /// Deliver sub-agent mail and report run ends through `slot`'s host.
    pub fn with_subagents(mut self, slot: HostSlot) -> Self {
        self.subagents = slot;
        self
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    /// The event bus, for components that emit on a run's behalf (the
    /// sub-agent host announcing a spawn).
    pub fn event_sender(&self) -> broadcast::Sender<AgentEvent> {
        self.events.clone()
    }
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// The provider (a fallback chain where configured) that serves
    /// `model`, for one-off calls outside a run, such as a routine's
    /// notification gate (T1b).
    pub fn provider(&self, model: &ModelRef) -> Result<Arc<dyn Provider>, ProviderError> {
        (self.providers)(model)
    }

    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Context window for `provider`, the one compaction measures against.
    pub fn context_window(&self, provider: &str) -> u64 {
        self.cfg.context_window(provider)
    }

    fn emit(&self, ev: AgentEvent) {
        let _ = self.events.send(ev);
    }

    /// Record a new run with its input message. Model and system prompt are
    /// frozen on the run so a resumed run rebuilds identical requests.
    pub fn start_run(
        &self,
        session_id: &str,
        user_text: &str,
        model: &ModelRef,
        system_prompt: &str,
        trigger: &str,
    ) -> Result<RunRow, AgentError> {
        self.start_run_as(
            session_id,
            user_text,
            model,
            system_prompt,
            trigger,
            None,
            None,
            None,
        )
    }

    /// [`Agent::start_run`] for an agent profile: its tool scope (JSON tool
    /// names and `*` patterns) and memory scope, frozen on the run. `taint`
    /// starts it tainted, for that reason (untrusted input, e.g. a watch
    /// event).
    #[allow(clippy::too_many_arguments)]
    pub fn start_run_as(
        &self,
        session_id: &str,
        user_text: &str,
        model: &ModelRef,
        system_prompt: &str,
        trigger: &str,
        tool_scope: Option<&str>,
        memory_scope: Option<&str>,
        taint: Option<&str>,
    ) -> Result<RunRow, AgentError> {
        let run_id = new_id();
        let input = self
            .journal
            .to_row(&Message::user_text(user_text))
            .map_err(|e| AgentError::Corrupt(e.to_string()))?;
        Ok(self.store.create_run(
            &tern_store::NewRun {
                id: &run_id,
                session_id,
                trigger,
                model: &model.to_string(),
                system_prompt,
                tool_scope,
                memory_scope,
                taint,
                ..tern_store::NewRun::default()
            },
            &input,
        )?)
    }

    /// Resume an interrupted or crashed run.
    pub async fn resume(
        &self,
        run_id: &str,
        cancel: CancellationToken,
    ) -> Result<RunOutcome, AgentError> {
        let run = self
            .store
            .run(run_id)?
            .ok_or_else(|| AgentError::RunNotFound(run_id.into()))?;
        if run.status == RunStatus::Completed.as_str() {
            return Err(AgentError::NotResumable(run.id, run.status));
        }
        self.store
            .set_run_status(run_id, RunStatus::Running.as_str(), None)?;
        self.drive(run_id, cancel).await
    }

    /// Drive a run from step 0 to completion, replaying journaled steps.
    pub async fn drive(
        &self,
        run_id: &str,
        cancel: CancellationToken,
    ) -> Result<RunOutcome, AgentError> {
        let run = self
            .store
            .run(run_id)?
            .ok_or_else(|| AgentError::RunNotFound(run_id.into()))?;
        tracing::info!(run = %run.id, session = %run.session_id, model = ?run.model, "run started");
        self.emit(AgentEvent::RunStarted {
            run_id: run.id.clone(),
            session_id: run.session_id.clone(),
            parent_run_id: run.parent_run_id.clone(),
        });

        let mut outcome = RunOutcome {
            run_id: run.id.clone(),
            status: RunStatus::Running,
            final_text: String::new(),
            error: None,
            usage: Usage::default(),
            quiet: false,
            tool_result: None,
        };
        let result = self.drive_inner(&run, &cancel, &mut outcome).await;
        match result {
            Ok(status) => outcome.status = status,
            Err(e) => {
                outcome.status = RunStatus::Failed;
                outcome.error = Some(e.to_string());
            }
        }
        let host = self.subagents.get();
        if outcome.status == RunStatus::Interrupted && outcome.error.is_none() {
            outcome.error = host.and_then(|h| h.interrupt_reason(&run));
        }
        self.store
            .set_run_status(&run.id, outcome.status.as_str(), outcome.error.as_deref())?;
        // Decided from the journaled answer, so a resumed run decides the same.
        if outcome.status == RunStatus::Completed
            && run.trigger == "cron"
            && run.parent_run_id.is_none()
            && crate::quiet::is_no_reply(&outcome.final_text)
        {
            outcome.quiet = true;
            self.store.set_run_quiet(&run.id)?;
        }
        if let Some(h) = host {
            h.run_ended(&run, &outcome).await;
        }
        match &outcome.error {
            Some(e) => {
                tracing::warn!(run = %run.id, status = outcome.status.as_str(), error = %self.journal.redact_text(e), "run finished")
            }
            None => tracing::info!(run = %run.id, status = outcome.status.as_str(), "run finished"),
        }
        self.emit(AgentEvent::RunFinished {
            run_id: run.id.clone(),
            status: outcome.status.as_str().into(),
            error: outcome.error.clone(),
            quiet: outcome.quiet,
        });
        Ok(outcome)
    }

    async fn drive_inner(
        &self,
        run: &RunRow,
        cancel: &CancellationToken,
        outcome: &mut RunOutcome,
    ) -> Result<RunStatus, AgentError> {
        // A call from an MCP client runs no model.
        if run.trigger == mcp::TRIGGER {
            let ctx = ToolCtx {
                workspace: self.cfg.workspace.clone(),
                run_id: run.id.clone(),
                session_id: run.session_id.clone(),
                cancel: cancel.clone(),
            };
            self.tools.prepare().await;
            return self.mcp_exec(run, &ctx, cancel, outcome).await;
        }
        let model: ModelRef = run
            .model
            .as_deref()
            .ok_or_else(|| AgentError::Corrupt(format!("run {} has no model", run.id)))?
            .parse()?;
        let provider = (self.providers)(&model)?;
        let system = run.system_prompt.clone().unwrap_or_default();
        let ctx = ToolCtx {
            workspace: self.cfg.workspace.clone(),
            run_id: run.id.clone(),
            session_id: run.session_id.clone(),
            cancel: cancel.clone(),
        };

        self.tools.prepare().await;
        if run.trigger == crate::outbox::TRIGGER {
            return self.outbox_exec(run, &ctx, outcome).await;
        }
        // Frozen on the run when it started, so replay asks where it asked.
        let autonomy = Autonomy::of_run(run.autonomy.as_deref());
        // Sub-agents run with the scope, limits and budget frozen on their row.
        let scope: Option<Vec<String>> = run
            .tool_scope
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| AgentError::Corrupt(format!("run {} tool_scope: {e}", run.id)))?;
        let scope = scope.as_deref();
        let tools = self.tools.specs_for(scope);
        let max_iterations = run.max_iterations.unwrap_or(self.cfg.max_iterations);
        let window = self.cfg.context_window(&model.provider);
        let max_tokens = provider.max_output_tokens().unwrap_or(self.cfg.max_tokens);
        let mut idx: u32 = 0;
        let mut iterations: u32 = 0;
        // Provider-reported tokens / our estimate for the last request; used
        // to correct the chars/4 estimate. Derived from journaled usage, so
        // replay makes the same compaction decisions.
        let mut correction = 1.0_f64;
        // Set once a limit is reached: the next model call is the closing
        // turn, made without tools.
        let mut wrap_up: Option<Limit> = None;
        let mut taint = Taint::at_start(run);
        loop {
            if cancel.is_cancelled() {
                return Ok(RunStatus::Interrupted);
            }
            if let Some(prev) = self.carry_due(run, idx)? {
                match self.carry_step(run, &prev, &model, &provider, cancel).await {
                    Ok(()) => idx += 1,
                    Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
                    Err(e) => return Err(e.into()),
                }
            }
            if let Some(channel) = self.channel_due(run, idx)? {
                match self.channel_step(run, idx, &channel, &mut taint).await {
                    Ok(()) => idx += 1,
                    Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
                    Err(e) => return Err(e.into()),
                }
            }
            if self.inbox_due(run, idx, false)? {
                match self.inbox_step(run, idx, false, &mut taint, cancel).await {
                    Ok(_) => idx += 1,
                    Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
                    Err(e) => return Err(e.into()),
                }
            }
            let mut messages = self.context(run, idx)?;
            let estimate = compaction::estimate_tokens(&system, &messages, &tools);
            if (estimate as f64 * correction) > window as f64 * compaction::THRESHOLD {
                match self
                    .compact(run, idx, &model, &provider, cancel, &mut outcome.usage)
                    .await
                {
                    Ok(0) => {}
                    Ok(steps) => {
                        idx += steps;
                        messages = self.context(run, idx)?;
                    }
                    Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
                    Err(e) => return Err(e.into()),
                }
            }
            let estimate = compaction::estimate_tokens(&system, &messages, &tools);
            if let Some(limit) = wrap_up {
                push_user_text(&mut messages, &limit.wrap_up_message());
            }
            let req = ChatRequest {
                model: model.model.clone(),
                system: system.clone(),
                messages,
                tools: tools.clone(),
                max_tokens,
                forbid_tool_calls: wrap_up.is_some(),
            };
            let step_input = LlmStepInput {
                model: &req.model,
                messages: req.messages.len(),
                request_digest: crate::journal::input_hash(StepKind::Llm, &req)
                    .map_err(StepError::from)?,
            };
            let turn = self
                .journal
                .step_with(
                    &run.id,
                    idx,
                    StepKind::Llm,
                    &step_input,
                    InFlight::Retry,
                    || {
                        let req = req.clone();
                        async {
                            let turn = self.call_llm(&run.id, &provider, req, cancel, true).await?;
                            let usage = turn.usage;
                            let messages = vec![turn.message.clone()];
                            let answered = turn.fallback.clone();
                            Ok(StepOutput::new(turn)
                                .with_usage(usage)
                                .answered_by(answered.as_ref())
                                .with_messages(messages))
                        }
                    },
                )
                .await;
            let turn = match turn {
                Ok(t) => t,
                Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
                Err(e) => return Err(e.into()),
            };
            idx += 1;
            iterations += 1;
            let u = &turn.value.usage;
            let reported = u.input_tokens + u.cache_read_tokens + u.cache_write_tokens;
            if reported > 0 && estimate > 0 {
                correction = reported as f64 / estimate as f64;
            }
            // This call's input: the least the next one costs.
            let last_input = reported;
            add_usage(&mut outcome.usage, &turn.value.usage);
            outcome.final_text = turn.value.message.text();
            self.emit(AgentEvent::AssistantMessage {
                run_id: run.id.clone(),
                text: outcome.final_text.clone(),
                replayed: turn.replayed,
            });
            if !turn.replayed {
                self.emit(AgentEvent::Usage {
                    run_id: run.id.clone(),
                    usage: turn.value.usage,
                });
            }

            if let Some(limit) = wrap_up {
                // Any call it makes anyway is left unanswered; the session's
                // next request closes it.
                outcome.final_text.push_str(&limit.note(&outcome.usage));
                return Ok(RunStatus::Completed);
            }

            let calls: Vec<ToolCall> = turn.value.message.tool_uses().collect();
            if calls.is_empty() {
                // No finishing with live children: wait for their reports
                // and give them to the model before the run may complete.
                if !self.inbox_due(run, idx, true)? {
                    return Ok(RunStatus::Completed);
                }
                if let Some(cap) = token_cap(run, &outcome.usage) {
                    outcome.error = Some(cap);
                    return Ok(RunStatus::Failed);
                }
                // It has given its answer; no closing turn needed.
                if let Some(limit) =
                    limit_reached(run, iterations, max_iterations, &outcome.usage, 0)
                {
                    outcome.final_text.push_str(&limit.note(&outcome.usage));
                    return Ok(RunStatus::Completed);
                }
                match self.inbox_step(run, idx, true, &mut taint, cancel).await {
                    Ok(0) => return Ok(RunStatus::Completed),
                    Ok(_) => {
                        idx += 1;
                        continue;
                    }
                    Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
                    Err(e) => return Err(e.into()),
                }
            }
            // Hitting the output limit cuts off the last call mid-argument.
            let truncated_id = (turn.value.stop_reason == StopReason::MaxTokens)
                .then(|| calls.last().map(|c| c.id.clone()))
                .flatten();
            for call in calls {
                let truncated = truncated_id.as_deref() == Some(call.id.as_str());
                let in_scope = crate::tool::in_scope(scope, &call.name);
                let runs = in_scope && !truncated;
                let host = runs.then(|| self.tools.egress_host(&call)).flatten();
                let gate = Gate {
                    tainted: taint.is_set(),
                    autonomy,
                };
                let mut policy = if runs {
                    self.tools.policy(&ctx, &call)
                } else {
                    // Refused without running; nothing to approve or redo.
                    crate::tool::ToolPolicy::new(crate::tool::Effect::Read, true)
                };
                // Asks only because of the tainted host limit: the prompt
                // then offers the host-limit choices.
                let limited = taint.limits(host.as_deref());
                let egress = (limited && !approval::needs_check(&call.name, &policy, gate))
                    .then_some(host.as_deref())
                    .flatten();
                if limited {
                    policy = policy.forced(true);
                }
                if approval::needs_check(&call.name, &policy, gate) {
                    let decision = self
                        .approval_step(
                            run,
                            idx,
                            &call,
                            &policy,
                            gate,
                            taint.reason(),
                            egress,
                            cancel,
                        )
                        .await;
                    idx += 1;
                    match decision {
                        Ok(ApprovalOutcome::Approved) => {}
                        Ok(ApprovalOutcome::ApprovedForRun) => taint.allow_all_egress(),
                        Ok(_) => continue,
                        Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
                        Err(e) => return Err(e.into()),
                    }
                }

                self.emit(AgentEvent::ToolStarted {
                    run_id: run.id.clone(),
                    id: call.id.clone(),
                    name: call.name.clone(),
                    summary: self.tools.summarize(&call),
                });
                let untrusted = in_scope && !truncated && self.tools.untrusted(&call.name);
                let in_flight = if policy.idempotent {
                    InFlight::Retry
                } else {
                    let r = ToolResult::error(INTERRUPTED_TOOL_MESSAGE);
                    InFlight::Substitute(
                        StepOutput::new(r.clone()).with_messages(vec![tool_result(&call, &r)]),
                    )
                };
                let result = match self
                    .journal
                    .step_with(&run.id, idx, StepKind::Tool, &call, in_flight, || async {
                        if truncated {
                            let r = ToolResult::error(TRUNCATED_TOOL_MESSAGE);
                            let msg = tool_result(&call, &r);
                            return Ok(StepOutput::new(r).with_messages(vec![msg]));
                        }
                        let sink: ProgressSink = {
                            let (events, run_id, id) =
                                (self.events.clone(), run.id.clone(), call.id.clone());
                            Arc::new(move |chunk: &str| {
                                let _ = events.send(AgentEvent::ToolOutput {
                                    run_id: run_id.clone(),
                                    id: id.clone(),
                                    chunk: chunk.to_owned(),
                                });
                            })
                        };
                        let call_fut = with_call(
                            &call.id,
                            with_progress(sink, self.tools.call_in(scope, &ctx, &call)),
                        );
                        // A script's own tool calls are sub-steps after this one.
                        let (mut r, consumes) = self
                            .serve_scripts(
                                run,
                                idx,
                                &call,
                                scope,
                                &ctx,
                                gate,
                                taint.reason(),
                                &taint.egress,
                                call_fut,
                            )
                            .await?;
                        // A tool may mark its own result (channel_read of
                        // tainted posts).
                        r.untrusted |= untrusted;
                        let msg = tool_result(&call, &r);
                        Ok(StepOutput::new(r)
                            .with_messages(vec![msg])
                            .with_consumes(consumes))
                    })
                    .await
                {
                    Ok(r) => r,
                    // A script cancelled while it waited for an approval.
                    Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
                    Err(e) => return Err(e.into()),
                };
                idx += 1;
                if !result.replayed && !truncated {
                    let summary = self.tools.summarize(&call);
                    self.ledger_effect(
                        run,
                        &call,
                        policy.effect,
                        &summary,
                        result.value.is_error,
                        None,
                    )?;
                }
                taint.contacted(host);
                // A script's own calls: the hosts they reached count too.
                while self.journal.peek(&run.id, idx)? == Some(StepKind::PyCall) {
                    let sub = self.script_substep(&run.id, idx)?;
                    taint.contacted(sub.host);
                    if sub.allow_all_egress {
                        taint.allow_all_egress();
                    }
                    idx += 1;
                }
                if result.value.untrusted {
                    taint.mark(&self.store, &run.id, &taint::tool_reason(&call.name))?;
                }
                if result.value.is_error {
                    // The first line of a tool error is usually the reason.
                    let reason = self.journal.redact_text(&preview(&result.value.content));
                    tracing::warn!(run = %run.id, tool = %call.name, replayed = result.replayed, %reason, "tool call failed");
                } else {
                    tracing::info!(run = %run.id, tool = %call.name, replayed = result.replayed, "tool call finished");
                }
                self.emit(AgentEvent::ToolFinished {
                    run_id: run.id.clone(),
                    id: call.id.clone(),
                    name: call.name.clone(),
                    is_error: result.value.is_error,
                    preview: preview(&result.value.content),
                    output: result.value.content.clone(),
                    replayed: result.replayed,
                });
                if cancel.is_cancelled() {
                    return Ok(RunStatus::Interrupted);
                }
            }

            if let Some(cap) = token_cap(run, &outcome.usage) {
                outcome.error = Some(cap);
                return Ok(RunStatus::Failed);
            }
            wrap_up = limit_reached(run, iterations, max_iterations, &outcome.usage, last_input);
        }
    }

    /// Whether step `idx` is an inbox step: on replay, exactly when one was
    /// recorded there; on the live frontier, when the run has mail (or, at
    /// the end of a turn, children that haven't reported). Replay never
    /// consults live mail, so it can't diverge from the original run.
    fn inbox_due(&self, run: &RunRow, idx: u32, at_turn_end: bool) -> Result<bool, AgentError> {
        match self.journal.peek(&run.id, idx)? {
            Some(kind) => Ok(kind == StepKind::Inbox),
            None => match self.subagents.get() {
                Some(h) => Ok(h.inbox_due(&run.id, at_turn_end)?),
                // Messages the user queued while the run worked.
                None => Ok(!self.store.pending_mail(&run.id)?.is_empty()),
            },
        }
    }

    /// Deliver pending mail as one user message, consuming it in the same
    /// transaction. `wait` blocks until there is mail (the end-of-turn
    /// inbox). Returns how many pieces were delivered.
    async fn inbox_step(
        &self,
        run: &RunRow,
        idx: u32,
        wait: bool,
        taint: &mut Taint,
        cancel: &CancellationToken,
    ) -> Result<usize, StepError> {
        let out = self
            .journal
            .step_with(
                &run.id,
                idx,
                StepKind::Inbox,
                &json!({ "wait": wait }),
                InFlight::Retry,
                || async {
                    let mail = match self.subagents.get() {
                        Some(h) => h.take_mail(&run.id, wait, cancel).await?,
                        None => self.store.pending_mail(&run.id)?,
                    };
                    let mut tainted = Vec::with_capacity(mail.len());
                    for m in &mail {
                        let from = match &m.from_run_id {
                            Some(id) => self.store.run(id)?,
                            None => None,
                        };
                        tainted.push(from.is_some_and(|r| r.taint.is_some()));
                    }
                    let text = mail
                        .iter()
                        .zip(&tainted)
                        .map(|(m, &t)| {
                            let text = crate::subagent::render_mail(m);
                            if t {
                                taint::wrap("agent mail", &text)
                            } else {
                                text
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let entries: Vec<InboxEntry> = mail
                        .iter()
                        .zip(&tainted)
                        .map(|(m, &tainted)| {
                            let body: Value = serde_json::from_str(&m.body).unwrap_or(Value::Null);
                            let field =
                                |k: &str| body.get(k).and_then(Value::as_str).map(Into::into);
                            InboxEntry {
                                id: m.id,
                                kind: m.kind.clone(),
                                from_run_id: m.from_run_id.clone(),
                                agent: field("agent"),
                                status: field("status"),
                                tainted,
                            }
                        })
                        .collect();
                    let consumes = mail.iter().map(|m| m.id).collect();
                    let messages = if text.is_empty() {
                        vec![]
                    } else {
                        vec![Message::user_text(&text)]
                    };
                    Ok(StepOutput::new(InboxOutput {
                        mail: entries,
                        waited: wait,
                    })
                    .with_messages(messages)
                    .with_consumes(consumes))
                },
            )
            .await?;
        if !out.replayed && !out.value.mail.is_empty() {
            tracing::info!(run = %run.id, idx, mail = out.value.mail.len(), "delivered agent mail");
        }
        if out.value.mail.iter().any(|m| m.tainted) {
            taint.mark(&self.store, &run.id, taint::TAINTED_MAIL)?;
        }
        Ok(out.value.mail.len())
    }

    #[allow(clippy::too_many_arguments)]
    async fn approval_step(
        &self,
        run: &RunRow,
        idx: u32,
        call: &ToolCall,
        policy: &crate::tool::ToolPolicy,
        gate: Gate,
        taint: Option<&str>,
        egress: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<ApprovalOutcome, StepError> {
        let out = self
            .journal
            .step_with(
                &run.id,
                idx,
                StepKind::Approval,
                call,
                InFlight::Retry,
                || async {
                    let (decision, why) = self
                        .decide(run, call, policy, gate, taint, egress, None, cancel)
                        .await?;
                    let messages = match decision {
                        ApprovalOutcome::Approved | ApprovalOutcome::ApprovedForRun => vec![],
                        _ => vec![tool_result(call, &ToolResult::error(why))],
                    };
                    Ok(StepOutput::new(decision).with_messages(messages))
                },
            )
            .await?;
        self.emit(AgentEvent::Approval {
            run_id: run.id.clone(),
            id: call.id.clone(),
            name: call.name.clone(),
            outcome: out.value,
        });
        Ok(out.value)
    }

    /// The effect policy with the allowlist, the run's taint and autonomy,
    /// then ask; nobody answering in time sends a write to the outbox.
    /// Runs inside the approval step so the decision is journaled even when
    /// the policy or the allowlist answers it. Returns the outcome and what
    /// the model is told when the call doesn't run.
    #[allow(clippy::too_many_arguments)]
    async fn decide(
        &self,
        run: &RunRow,
        call: &ToolCall,
        policy: &crate::tool::ToolPolicy,
        gate: Gate,
        taint: Option<&str>,
        egress: Option<&str>,
        script: Option<String>,
        cancel: &CancellationToken,
    ) -> Result<(ApprovalOutcome, String), StepError> {
        let always_ok = self.tools.allow_always(call);
        // A tool that opts out ignores old allowlist entries too.
        let granted = policy.forbidden.is_none()
            && always_ok
            && approval::allowed_by(&self.store.allowlist(&call.name)?, call);
        let (effect, always) = match approval::check(&call.name, policy, granted, gate) {
            approval::Check::Allow => return Ok((ApprovalOutcome::Approved, String::new())),
            approval::Check::Forbid(flag) => {
                tracing::warn!(run = %run.id, tool = %call.name, flag, "refused a forbidden tool");
                self.ledger_refused(run, call, flag)?;
                let why = approval::forbidden_message(flag);
                return Ok((ApprovalOutcome::Denied, why));
            }
            approval::Check::ReadOnly => {
                let why = approval::READONLY_MESSAGE.to_owned();
                return Ok((ApprovalOutcome::Denied, why));
            }
            approval::Check::Ask { effect, always } => (
                effect,
                always && always_ok && approval::pattern_for(call).is_some(),
            ),
        };
        // The tainted host limit: a standing grant for this host answers it.
        let egress = egress.filter(|_| effect == crate::tool::Effect::Read);
        if let Some(h) = egress
            && self
                .store
                .allowlist(&call.name)?
                .contains(&approval::host_pattern(h))
        {
            return Ok((ApprovalOutcome::Approved, String::new()));
        }
        let req = ApprovalRequest {
            id: new_id(),
            run_id: run.id.clone(),
            session_id: run.session_id.clone(),
            call: call.clone(),
            summary: self.tools.summarize(call),
            effect,
            pattern: approval::pattern_for(call).filter(|_| always),
            parent_run_id: run.parent_run_id.clone(),
            agent_label: run.label.clone(),
            taint: taint.map(Into::into),
            local_only: policy.local_approval,
            egress_host: egress.map(Into::into),
            script,
        };
        // A write may wait in the outbox instead (tooling spec §5.2).
        let deferrable = effect != crate::tool::Effect::Read && self.approvals.can_defer();
        let wait = if deferrable {
            self.cfg.approval_wait
        } else {
            self.cfg.approval_timeout
        };
        let asked = tokio::time::timeout(wait, self.approvals.ask(&req));
        let decision = tokio::select! {
            d = asked => d,
            () = cancel.cancelled() => return Err(StepError::Cancelled),
        };
        let denied = |why: &str| (ApprovalOutcome::Denied, why.to_owned());
        // A call that goes to the outbox is recorded as queued instead.
        let queued = deferrable && matches!(decision, Ok(Decision::Unanswered) | Err(_));
        if !queued {
            self.ledger_decision(run, &req, decision.as_ref().ok().copied())?;
        }
        Ok(match decision {
            Ok(Decision::AllowOnce) => (ApprovalOutcome::Approved, String::new()),
            // The host-limit choices; anywhere else they mean "once".
            Ok(Decision::AllowRun) if egress.is_some() => {
                (ApprovalOutcome::ApprovedForRun, String::new())
            }
            Ok(Decision::AlwaysAllowHost) => {
                if let Some(h) = egress {
                    self.store
                        .add_allowlist(&call.name, &approval::host_pattern(h))?;
                }
                (ApprovalOutcome::Approved, String::new())
            }
            Ok(Decision::AllowRun) => (ApprovalOutcome::Approved, String::new()),
            Ok(Decision::AlwaysAllow) => {
                // Without a pattern "always" can only mean "this once".
                if let Some(p) = &req.pattern {
                    self.store.add_allowlist(&call.name, p)?;
                }
                (ApprovalOutcome::Approved, String::new())
            }
            Ok(Decision::Deny) => denied("denied: the user did not approve this call"),
            Ok(Decision::Unanswered) | Err(_) if deferrable => {
                let id = self.defer(run, &req).await?;
                (
                    ApprovalOutcome::Deferred,
                    crate::outbox::deferred_message(id),
                )
            }
            Ok(Decision::Unanswered) => denied("denied: nobody was there to approve this call"),
            Err(_) => (
                ApprovalOutcome::Timeout,
                "denied: approval timed out".to_owned(),
            ),
        })
    }

    /// One model call with retries on transient errors (spec §6).
    async fn call_llm(
        &self,
        run_id: &str,
        provider: &Arc<dyn Provider>,
        req: ChatRequest,
        cancel: &CancellationToken,
        emit_text: bool,
    ) -> Result<AssistantTurn, StepError> {
        let mut attempt = 1;
        loop {
            let err = match self
                .stream_once(run_id, provider, req.clone(), cancel, emit_text)
                .await
            {
                Ok(turn) => return Ok(turn),
                Err(ProviderError::Cancelled) => return Err(StepError::Cancelled),
                Err(e) => e,
            };
            if !err.is_retryable() || attempt >= MAX_LLM_ATTEMPTS {
                return Err(StepError::Failed(err.to_string()));
            }
            let delay = err
                .retry_after()
                .unwrap_or_else(|| backoff(self.cfg.retry_base, attempt));
            tracing::warn!(run_id, attempt, ?delay, error = %err, "model call failed; retrying");
            self.emit(AgentEvent::Retrying {
                run_id: run_id.into(),
                attempt,
                delay_ms: delay.as_millis() as u64,
                error: err.to_string(),
            });
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = cancel.cancelled() => return Err(StepError::Cancelled),
            }
            attempt += 1;
        }
    }

    async fn stream_once(
        &self,
        run_id: &str,
        provider: &Arc<dyn Provider>,
        req: ChatRequest,
        cancel: &CancellationToken,
        emit_text: bool,
    ) -> Result<AssistantTurn, ProviderError> {
        let mut stream = provider.stream(req, cancel.clone()).await?;
        let mut asm = Assembler::default();
        loop {
            let next = tokio::select! {
                n = stream.next() => n,
                () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            };
            match next {
                None => break,
                Some(Err(e)) => return Err(e),
                Some(Ok(ev)) => {
                    if let (true, StreamEvent::TextDelta(t)) = (emit_text, &ev) {
                        self.emit(AgentEvent::TextDelta {
                            run_id: run_id.into(),
                            text: t.clone(),
                        });
                    }
                    if asm.push(ev) {
                        break;
                    }
                }
            }
        }
        asm.finish()
    }

    /// Conversation as step `idx` sees it, with consecutive same-role
    /// messages merged (tool results from sequential tool steps).
    fn context(&self, run: &RunRow, idx: u32) -> Result<Vec<Message>, AgentError> {
        let rows = compaction::effective(self.store.run_context(run, idx)?);
        to_messages(rows)
    }

    /// Summarize old context as step `idx`, after a memory flush when one
    /// applies. Returns the number of steps recorded (0: nothing to
    /// compact). The flush's usage is added to `usage`.
    async fn compact(
        &self,
        run: &RunRow,
        idx: u32,
        model: &ModelRef,
        provider: &Arc<dyn Provider>,
        cancel: &CancellationToken,
        usage: &mut Usage,
    ) -> Result<u32, StepError> {
        let rows = compaction::effective(self.store.run_context(run, idx)?);
        let Some(plan) = compaction::plan(&rows) else {
            return Ok(0);
        };
        let old = to_messages(rows[..plan.split].to_vec())
            .map_err(|e| StepError::Failed(e.to_string()))?;
        let transcript = compaction::transcript(&old);
        let flushed = self
            .flush_memory(
                run,
                idx,
                model,
                provider,
                &transcript,
                plan.cover_through,
                cancel,
                usage,
            )
            .await?;
        let idx = idx + u32::from(flushed);
        let input = compaction::StepInput {
            cover_through: plan.cover_through,
            transcript_digest: blake3::hash(transcript.as_bytes()).to_hex().to_string(),
        };
        tracing::info!(run = %run.id, idx, cover_through = plan.cover_through, "compacting context");
        let req = ChatRequest {
            model: model.model.clone(),
            system: compaction::SYSTEM_PROMPT.into(),
            messages: vec![compaction::summary_request(&transcript)],
            tools: vec![],
            max_tokens: compaction::SUMMARY_MAX_TOKENS,
            forbid_tool_calls: false,
        };
        self.journal
            .step_with(
                &run.id,
                idx,
                StepKind::Compaction,
                &input,
                InFlight::Retry,
                || async {
                    let turn = self.call_llm(&run.id, provider, req, cancel, false).await?;
                    let summary = turn.message.text();
                    let usage = turn.usage;
                    Ok(StepOutput::new(summary.clone())
                        .with_usage(usage)
                        .answered_by(turn.fallback.as_ref())
                        .with_messages(vec![compaction::summary_message(&summary)])
                        .with_summary_of(plan.cover_through))
                },
            )
            .await?;
        self.emit(AgentEvent::Compacted {
            run_id: run.id.clone(),
            cover_through: plan.cover_through,
        });
        Ok(1 + u32::from(flushed))
    }

    /// Give the model one silent turn, with only `memory_save`, to keep
    /// durable facts from the span about to be summarized (spec §7, T5a).
    /// Returns whether a step was recorded at `idx`. It never fails the
    /// run: errors are recorded in its output and compaction goes ahead.
    #[allow(clippy::too_many_arguments)]
    async fn flush_memory(
        &self,
        run: &RunRow,
        idx: u32,
        model: &ModelRef,
        provider: &Arc<dyn Provider>,
        transcript: &str,
        cover_through: i64,
        cancel: &CancellationToken,
        usage: &mut Usage,
    ) -> Result<bool, StepError> {
        let scope = [memory::FLUSH_TOOL.to_owned()];
        let tools = self.tools.specs_for(Some(&scope));
        // Replay follows the journal; only the live frontier decides.
        match self.journal.peek(&run.id, idx)? {
            Some(StepKind::MemoryFlush) => {}
            Some(_) => return Ok(false),
            None => {
                // A tainted run's transcript may carry injected "facts".
                let tainted = self.store.run(&run.id)?.is_some_and(|r| r.taint.is_some());
                if !self.cfg.memory_flush
                    || tainted
                    || tools.is_empty()
                    || !memory::flush_applies(run)
                    || !memory::flush_affordable(run, usage, transcript)
                {
                    return Ok(false);
                }
            }
        }
        let input = memory::FlushInput {
            cover_through,
            transcript_digest: blake3::hash(transcript.as_bytes()).to_hex().to_string(),
        };
        let interrupted = memory::FlushOutput {
            error: Some("interrupted by a crash; not repeated".into()),
            ..Default::default()
        };
        let req = ChatRequest {
            model: model.model.clone(),
            system: memory::FLUSH_SYSTEM_PROMPT.into(),
            messages: vec![memory::flush_request(transcript)],
            tools,
            max_tokens: memory::FLUSH_MAX_TOKENS,
            forbid_tool_calls: false,
        };
        let out = self
            .journal
            .step_with(
                &run.id,
                idx,
                StepKind::MemoryFlush,
                &input,
                InFlight::Substitute(StepOutput::new(interrupted)),
                || async {
                    let mut out = memory::FlushOutput::default();
                    let mut answered = None;
                    match self.call_llm(&run.id, provider, req, cancel, false).await {
                        Err(StepError::Cancelled) => return Err(StepError::Cancelled),
                        Err(e) => out.error = Some(e.to_string()),
                        Ok(turn) => {
                            out.usage = turn.usage;
                            answered = turn.fallback.clone();
                            let ctx = ToolCtx {
                                workspace: self.cfg.workspace.clone(),
                                run_id: run.id.clone(),
                                session_id: run.session_id.clone(),
                                cancel: cancel.clone(),
                            };
                            let calls = turn
                                .message
                                .tool_uses()
                                .filter(|c| c.name == memory::FLUSH_TOOL);
                            for call in calls.take(memory::MAX_FLUSH_SAVES) {
                                let policy = self.tools.policy(&ctx, &call);
                                let gate = Gate {
                                    tainted: false,
                                    autonomy: Autonomy::of_run(run.autonomy.as_deref()),
                                };
                                if approval::needs_check(&call.name, &policy, gate) {
                                    out.saved.push("skipped: would need approval".into());
                                    continue;
                                }
                                let r = self.tools.call_in(Some(&scope), &ctx, &call).await;
                                out.saved.push(if r.is_error {
                                    format!("error: {}", r.content)
                                } else {
                                    r.content
                                });
                            }
                        }
                    }
                    let usage = out.usage;
                    Ok(StepOutput::new(out)
                        .with_usage(usage)
                        .answered_by(answered.as_ref()))
                },
            )
            .await?;
        let flush = out.value;
        add_usage(usage, &flush.usage);
        match &flush.error {
            Some(e) => {
                tracing::warn!(run = %run.id, idx, error = %e, "memory flush failed; compacting anyway")
            }
            None => {
                tracing::info!(run = %run.id, idx, saved = flush.saved.len(), replayed = out.replayed, "memory flush")
            }
        }
        Ok(true)
    }
}

fn to_messages(rows: Vec<tern_store::MessageRow>) -> Result<Vec<Message>, AgentError> {
    let mut out: Vec<Message> = Vec::with_capacity(rows.len());
    for row in rows {
        let role = match row.role.as_str() {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            other => {
                return Err(AgentError::Corrupt(format!(
                    "message {} has role {other:?}",
                    row.id
                )));
            }
        };
        let content: Vec<ContentBlock> = serde_json::from_str(&row.content_json)
            .map_err(|e| AgentError::Corrupt(format!("message {}: {e}", row.id)))?;
        match out.last_mut() {
            Some(prev) if prev.role == role => prev.content.extend(content),
            _ => out.push(Message { role, content }),
        }
    }
    close_dangling_tool_uses(&mut out);
    Ok(out)
}

/// The result given to a call that never got one.
pub const NOT_RUN_TOOL_MESSAGE: &str =
    "not run: the run stopped (cancelled, or replaced by a new message) before this call executed";

/// Give every tool call without a result an error result, first in the next
/// message. A run cancelled mid-batch (say, while waiting on approval)
/// leaves calls unanswered in the session, and providers reject any later
/// request that includes them.
fn close_dangling_tool_uses(messages: &mut Vec<Message>) {
    let mut i = 0;
    while i < messages.len() {
        if messages[i].role != Role::Assistant {
            i += 1;
            continue;
        }
        let answered: std::collections::HashSet<&str> = match messages.get(i + 1) {
            Some(next) if next.role == Role::User => next
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                    _ => None,
                })
                .collect(),
            _ => Default::default(),
        };
        let missing: Vec<ContentBlock> = messages[i]
            .tool_uses()
            .filter(|c| !answered.contains(c.id.as_str()))
            .map(|c| ContentBlock::ToolResult {
                tool_use_id: c.id,
                content: NOT_RUN_TOOL_MESSAGE.into(),
                is_error: true,
            })
            .collect();
        if !missing.is_empty() {
            match messages.get_mut(i + 1) {
                Some(next) if next.role == Role::User => {
                    next.content.splice(0..0, missing);
                }
                _ => messages.insert(
                    i + 1,
                    Message {
                        role: Role::User,
                        content: missing,
                    },
                ),
            }
        }
        i += 1;
    }
}

/// How recently a crashed run must have made progress to be auto-resumed.
pub const RESUME_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Startup recovery (spec §5.3). Runs still marked `running` were cut off by
/// a crash. Those updated within `max_age` are returned to be resumed; older
/// ones are marked `interrupted` and left for `tern resume`.
pub fn triage_running_runs(store: &Store, max_age: Duration) -> Result<Vec<RunRow>, AgentError> {
    let cutoff = tern_store::now_ms() - max_age.as_millis() as i64;
    let mut resumable = Vec::new();
    for run in store.runs_with_status(RunStatus::Running.as_str())? {
        if run.updated_at >= cutoff {
            resumable.push(run);
        } else {
            tracing::info!(run = %run.id, "marking stale running run as interrupted");
            store.set_run_status(
                &run.id,
                RunStatus::Interrupted.as_str(),
                Some("interrupted by a crash more than 24 hours before restart"),
            )?;
        }
    }
    Ok(resumable)
}

/// Time-ordered unique id (UUIDv7, hex without dashes).
pub fn new_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

fn tokens_used(usage: &Usage) -> u64 {
    usage.input_tokens + usage.output_tokens + usage.cache_read_tokens + usage.cache_write_tokens
}

/// Why a scheduled run with a token budget (its schedule's
/// `tokens_per_run`) must stop, once it has used it all. It fails rather
/// than finishing with a note: nobody asked for a partial answer.
/// Sub-agents keep the note, so their parent gets what they found.
fn token_cap(run: &RunRow, usage: &Usage) -> Option<String> {
    let budget = run.budget_tokens.filter(|_| run.trigger == "cron")?;
    let used = tokens_used(usage);
    (used >= budget).then(|| {
        format!("stopped: this run used {used} tokens, over its cap of {budget} (tokens_per_run)")
    })
}

/// A limit a run has reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limit {
    Iterations(u32),
    Tokens(u64),
}

impl Limit {
    fn wrap_up_message(self) -> String {
        let limit = match self {
            Limit::Iterations(max) => format!("step limit ({max} steps)"),
            Limit::Tokens(budget) => format!("token budget ({budget} tokens)"),
        };
        format!("[{}]", WRAP_UP_MESSAGE.replace("{limit}", &limit))
    }

    /// Appended to the final text of the run it stopped.
    fn note(self, usage: &Usage) -> String {
        match self {
            Limit::Iterations(max) => {
                format!("\n\n{STOPPED_MARKER}reached max_iterations ({max})]")
            }
            Limit::Tokens(budget) => format!(
                "\n\n{STOPPED_MARKER}token budget ({} of {budget} tokens)]",
                tokens_used(usage)
            ),
        }
    }
}

/// The limit a run has reached, if any. The token budget counts as reached
/// once the next call, costing at least `next_input` (the last call's
/// input), would cross it, so the closing turn lands near the budget rather
/// than a whole context past it. Both come from journaled values, so replay
/// decides the same.
fn limit_reached(
    run: &RunRow,
    iterations: u32,
    max: u32,
    usage: &Usage,
    next_input: u64,
) -> Option<Limit> {
    if iterations >= max {
        return Some(Limit::Iterations(max));
    }
    // A scheduled run's budget is its tokens_per_run cap, which fails it
    // instead (`token_cap`).
    match run.budget_tokens.filter(|_| run.trigger != "cron") {
        Some(budget) if tokens_used(usage) + next_input >= budget => Some(Limit::Tokens(budget)),
        _ => None,
    }
}

/// Add `text` to the conversation as the user's, after any tool results.
fn push_user_text(messages: &mut Vec<Message>, text: &str) {
    let block = ContentBlock::Text {
        text: text.to_owned(),
    };
    match messages.last_mut() {
        Some(last) if last.role == Role::User => last.content.push(block),
        _ => messages.push(Message {
            role: Role::User,
            content: vec![block],
        }),
    }
}

fn add_usage(total: &mut Usage, u: &Usage) {
    total.input_tokens += u.input_tokens;
    total.output_tokens += u.output_tokens;
    total.cache_read_tokens += u.cache_read_tokens;
    total.cache_write_tokens += u.cache_write_tokens;
}

/// Exponential backoff with jitter: base * 2^(attempt-1), capped at 30s,
/// scaled by a random factor in [0.5, 1.0).
pub fn backoff(base: Duration, attempt: u32) -> Duration {
    let exp = base.saturating_mul(1u32 << attempt.saturating_sub(1).min(10));
    let capped = exp.min(Duration::from_secs(30));
    capped.mul_f64(0.5 + fastrand::f64() * 0.5)
}

/// The tool result message; an untrusted one is wrapped
/// ([`taint::wrap`]).
fn tool_result(call: &ToolCall, r: &ToolResult) -> Message {
    let content = if r.untrusted {
        taint::wrap(&call.name, &r.content)
    } else {
        r.content.clone()
    };
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: call.id.clone(),
            content,
            is_error: r.is_error,
        }],
    }
}

/// One-line description of a tool call for UIs and approval prompts.
pub fn summarize(call: &ToolCall) -> String {
    let arg = |k: &str| call.input.get(k).and_then(Value::as_str);
    let detail = match call.name.as_str() {
        "shell" => arg("command").map(str::to_owned),
        "http_fetch" => arg("url").map(str::to_owned),
        "search_files" => arg("pattern").map(|p| format!("/{p}/")),
        _ => arg("path").map(str::to_owned),
    }
    .unwrap_or_else(|| call.input.to_string());
    let detail: String = detail.chars().take(200).collect();
    format!("{} {}", call.name, detail)
}

/// First line of a tool result, shortened for one-line displays.
pub fn preview(s: &str) -> String {
    let first = s.lines().next().unwrap_or_default();
    first.chars().take(120).collect()
}

/// Builds an assistant message from stream events.
#[derive(Default)]
struct Assembler {
    blocks: Vec<ContentBlock>,
    text: String,
    tool_json: HashMap<String, String>,
    stop: Option<(Usage, crate::types::StopReason)>,
    fallback: Option<crate::types::Fallback>,
}

impl Assembler {
    /// Returns true once the message is complete.
    fn push(&mut self, ev: StreamEvent) -> bool {
        match ev {
            StreamEvent::TextDelta(t) => self.text.push_str(&t),
            StreamEvent::ToolUseStart { id, name } => {
                self.flush_text();
                self.tool_json.insert(id.clone(), String::new());
                self.blocks.push(ContentBlock::ToolUse {
                    id,
                    name,
                    input: Value::Null,
                });
            }
            StreamEvent::ToolUseInputDelta { id, partial_json } => {
                self.tool_json
                    .entry(id)
                    .or_default()
                    .push_str(&partial_json);
            }
            StreamEvent::MessageStop { usage, stop_reason } => {
                self.stop = Some((usage, stop_reason));
                return true;
            }
            StreamEvent::Fallback(f) => self.fallback = Some(f),
        }
        false
    }

    fn flush_text(&mut self) {
        if !self.text.is_empty() {
            self.blocks.push(ContentBlock::Text {
                text: std::mem::take(&mut self.text),
            });
        }
    }

    fn finish(mut self) -> Result<AssistantTurn, ProviderError> {
        let Some((usage, stop_reason)) = self.stop else {
            return Err(ProviderError::Stream {
                message: "stream ended before the message was complete".into(),
                retryable: true,
            });
        };
        self.flush_text();
        for b in &mut self.blocks {
            if let ContentBlock::ToolUse { id, input, .. } = b {
                let raw = self.tool_json.remove(id.as_str()).unwrap_or_default();
                *input = if raw.trim().is_empty() {
                    json!({})
                } else {
                    match serde_json::from_str::<Value>(&raw) {
                        Ok(v @ Value::Object(_)) => v,
                        // Providers require an object; the tool will report bad arguments.
                        _ => json!({ "_invalid_arguments": raw }),
                    }
                };
            }
        }
        Ok(AssistantTurn {
            message: Message {
                role: Role::Assistant,
                content: self.blocks,
            },
            usage,
            stop_reason,
            fallback: self.fallback,
        })
    }
}

#[cfg(test)]
mod tests;
