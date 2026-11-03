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

use crate::approval::{self, ApprovalHandler, ApprovalOutcome, ApprovalRequest, Decision};
use crate::compaction;
use crate::config::ModelRef;
use crate::events::AgentEvent;
use crate::journal::{InFlight, Journal, StepError, StepKind, StepOutput};
use crate::tool::{ToolCtx, ToolRegistry, ToolResult};
use crate::types::{
    AssistantTurn, ChatRequest, ContentBlock, Message, Provider, ProviderError, Role, StreamEvent,
    ToolCall, Usage,
};

pub const MAX_LLM_ATTEMPTS: u32 = 5;
pub const INTERRUPTED_TOOL_MESSAGE: &str = "interrupted: this call may have partially executed before a crash; verify state before retrying";

/// Resolves the provider half of a `provider:model` reference.
pub type ProviderFactory =
    Arc<dyn Fn(&str) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync>;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub run_id: String,
    pub status: RunStatus,
    /// Text of the last assistant message.
    pub final_text: String,
    pub error: Option<String>,
    pub usage: Usage,
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
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
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
        self.emit(AgentEvent::RunStarted {
            run_id: run.id.clone(),
            session_id: run.session_id.clone(),
        });

        let mut outcome = RunOutcome {
            run_id: run.id.clone(),
            status: RunStatus::Running,
            final_text: String::new(),
            error: None,
            usage: Usage::default(),
        };
        let result = self.drive_inner(&run, &cancel, &mut outcome).await;
        match result {
            Ok(status) => outcome.status = status,
            Err(e) => {
                outcome.status = RunStatus::Failed;
                outcome.error = Some(e.to_string());
            }
        }
        self.store
            .set_run_status(&run.id, outcome.status.as_str(), outcome.error.as_deref())?;
        self.emit(AgentEvent::RunFinished {
            run_id: run.id.clone(),
            status: outcome.status.as_str().into(),
            error: outcome.error.clone(),
        });
        Ok(outcome)
    }

    async fn drive_inner(
        &self,
        run: &RunRow,
        cancel: &CancellationToken,
        outcome: &mut RunOutcome,
    ) -> Result<RunStatus, AgentError> {
        let model: ModelRef = run
            .model
            .as_deref()
            .ok_or_else(|| AgentError::Corrupt(format!("run {} has no model", run.id)))?
            .parse()?;
        let provider = (self.providers)(&model.provider)?;
        let system = run.system_prompt.clone().unwrap_or_default();
        let ctx = ToolCtx {
            workspace: self.cfg.workspace.clone(),
            run_id: run.id.clone(),
            session_id: run.session_id.clone(),
            cancel: cancel.clone(),
        };

        let tools = self.tools.specs();
        let window = self.cfg.context_window(&model.provider);
        let mut idx: u32 = 0;
        let mut iterations: u32 = 0;
        // Provider-reported tokens / our estimate for the last request; used
        // to correct the chars/4 estimate. Derived from journaled usage, so
        // replay makes the same compaction decisions.
        let mut correction = 1.0_f64;
        loop {
            if cancel.is_cancelled() {
                return Ok(RunStatus::Interrupted);
            }
            let mut messages = self.context(run, idx)?;
            let estimate = compaction::estimate_tokens(&system, &messages, &tools);
            if (estimate as f64 * correction) > window as f64 * compaction::THRESHOLD {
                match self.compact(run, idx, &model, &provider, cancel).await {
                    Ok(true) => {
                        idx += 1;
                        messages = self.context(run, idx)?;
                    }
                    Ok(false) => {}
                    Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
                    Err(e) => return Err(e.into()),
                }
            }
            let estimate = compaction::estimate_tokens(&system, &messages, &tools);
            let req = ChatRequest {
                model: model.model.clone(),
                system: system.clone(),
                messages,
                tools: tools.clone(),
                max_tokens: self.cfg.max_tokens,
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
                            Ok(StepOutput::new(turn)
                                .with_usage(usage)
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

            let calls: Vec<ToolCall> = turn.value.message.tool_uses().collect();
            if calls.is_empty() {
                return Ok(RunStatus::Completed);
            }
            for call in calls {
                let policy = self.tools.policy(&ctx, &call);
                if policy.requires_approval {
                    let decision = self.approval_step(run, idx, &call, cancel).await;
                    idx += 1;
                    match decision {
                        Ok(ApprovalOutcome::Approved) => {}
                        Ok(_) => continue,
                        Err(StepError::Cancelled) => return Ok(RunStatus::Interrupted),
                        Err(e) => return Err(e.into()),
                    }
                }

                self.emit(AgentEvent::ToolStarted {
                    run_id: run.id.clone(),
                    id: call.id.clone(),
                    name: call.name.clone(),
                    summary: summarize(&call),
                });
                let in_flight = if policy.idempotent {
                    InFlight::Retry
                } else {
                    let r = ToolResult::error(INTERRUPTED_TOOL_MESSAGE);
                    InFlight::Substitute(
                        StepOutput::new(r.clone()).with_messages(vec![tool_result(&call, &r)]),
                    )
                };
                let result = self
                    .journal
                    .step_with(&run.id, idx, StepKind::Tool, &call, in_flight, || async {
                        let r = self.tools.call(&ctx, &call).await;
                        let msg = tool_result(&call, &r);
                        Ok(StepOutput::new(r).with_messages(vec![msg]))
                    })
                    .await?;
                idx += 1;
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

            if iterations >= self.cfg.max_iterations {
                outcome.final_text.push_str(&format!(
                    "\n\n[stopped: reached max_iterations ({})]",
                    self.cfg.max_iterations
                ));
                return Ok(RunStatus::Completed);
            }
        }
    }

    async fn approval_step(
        &self,
        run: &RunRow,
        idx: u32,
        call: &ToolCall,
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
                    let decision = self.decide(call, cancel).await?;
                    let messages = match decision {
                        ApprovalOutcome::Approved => vec![],
                        ApprovalOutcome::Denied => {
                            vec![tool_result(
                                call,
                                &ToolResult::error("denied: the user did not approve this call"),
                            )]
                        }
                        ApprovalOutcome::Timeout => {
                            vec![tool_result(
                                call,
                                &ToolResult::error("denied: approval timed out"),
                            )]
                        }
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

    /// Allowlist first, then ask. Runs inside the approval step so the
    /// decision is journaled even when the allowlist answers it.
    async fn decide(
        &self,
        call: &ToolCall,
        cancel: &CancellationToken,
    ) -> Result<ApprovalOutcome, StepError> {
        let allowlist = self.store.allowlist(&call.name)?;
        if approval::allowed_by(&allowlist, call) {
            return Ok(ApprovalOutcome::Approved);
        }
        let req = ApprovalRequest {
            call: call.clone(),
            summary: summarize(call),
            pattern: approval::pattern_for(call),
        };
        let asked = tokio::time::timeout(self.cfg.approval_timeout, self.approvals.ask(&req));
        let decision = tokio::select! {
            d = asked => d,
            () = cancel.cancelled() => return Err(StepError::Cancelled),
        };
        Ok(match decision {
            Ok(Decision::AllowOnce) => ApprovalOutcome::Approved,
            Ok(Decision::AlwaysAllow) => {
                self.store.add_allowlist(&call.name, &req.pattern)?;
                ApprovalOutcome::Approved
            }
            Ok(Decision::Deny) => ApprovalOutcome::Denied,
            Err(_) => ApprovalOutcome::Timeout,
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

    /// Summarize old context as step `idx`. Returns false if there was
    /// nothing to compact (no step recorded).
    async fn compact(
        &self,
        run: &RunRow,
        idx: u32,
        model: &ModelRef,
        provider: &Arc<dyn Provider>,
        cancel: &CancellationToken,
    ) -> Result<bool, StepError> {
        let rows = compaction::effective(self.store.run_context(run, idx)?);
        let Some(plan) = compaction::plan(&rows) else {
            return Ok(false);
        };
        let old = to_messages(rows[..plan.split].to_vec())
            .map_err(|e| StepError::Failed(e.to_string()))?;
        let transcript = compaction::transcript(&old);
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
                        .with_messages(vec![compaction::summary_message(&summary)])
                        .with_summary_of(plan.cover_through))
                },
            )
            .await?;
        self.emit(AgentEvent::Compacted {
            run_id: run.id.clone(),
            cover_through: plan.cover_through,
        });
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
    Ok(out)
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

fn tool_result(call: &ToolCall, r: &ToolResult) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: call.id.clone(),
            content: r.content.clone(),
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
        })
    }
}

#[cfg(test)]
mod tests;
