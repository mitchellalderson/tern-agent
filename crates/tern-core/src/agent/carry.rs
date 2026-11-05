//! The `carry` step (docs/channels-design.md §2.6): the first run of a new
//! segment of a stream (Home, a Telegram chat) starts with a summary of the
//! segment before it, its open items and its last exchanges.

use serde::{Deserialize, Serialize};
use tern_store::RunRow;
use tokio_util::sync::CancellationToken;

use super::{Agent, AgentError, to_messages};
use crate::channels::{CARRY_EXCHANGES_MAX, Carry};
use crate::compaction;
use crate::config::ModelRef;
use crate::journal::{InFlight, StepError, StepKind, StepOutput};
use crate::types::{ChatRequest, Message, Provider};

#[derive(Serialize)]
struct CarryInput<'a> {
    prev_session_id: &'a str,
}

/// What a `carry` step records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CarryOutput {
    pub prev_session_id: String,
    #[serde(flatten)]
    pub carry: Carry,
}

impl Agent {
    /// Whether step `idx` is a carry step: on replay, exactly when one was
    /// recorded there; live, for step 0 of the first top-level run of a
    /// session that continues another.
    pub(super) fn carry_due(&self, run: &RunRow, idx: u32) -> Result<Option<String>, AgentError> {
        if idx != 0 || run.parent_run_id.is_some() {
            return Ok(None);
        }
        let Some(prev) = self
            .store
            .session(&run.session_id)?
            .and_then(|s| s.prev_session_id)
        else {
            return Ok(None);
        };
        match self.journal.peek(&run.id, idx)? {
            Some(kind) => Ok((kind == StepKind::Carry).then_some(prev)),
            None => {
                let first = self.store.runs_for_session(&run.session_id)?;
                let first = first.iter().find(|r| r.parent_run_id.is_none());
                Ok(first.is_some_and(|r| r.id == run.id).then_some(prev))
            }
        }
    }

    /// Record the carry step: one cheap summary call (journaled, so replay
    /// doesn't summarize again) and one user message.
    pub(super) async fn carry_step(
        &self,
        run: &RunRow,
        prev: &str,
        model: &ModelRef,
        provider: &std::sync::Arc<dyn Provider>,
        cancel: &CancellationToken,
    ) -> Result<(), StepError> {
        let input = CarryInput {
            prev_session_id: prev,
        };
        let out = self
            .journal
            .step_with(
                &run.id,
                0,
                StepKind::Carry,
                &input,
                InFlight::Retry,
                || async {
                    let rows = self.store.session_messages(prev)?;
                    let (from, to) = match (rows.first(), rows.last()) {
                        (Some(a), Some(b)) => (a.created_at, b.created_at),
                        _ => (0, 0),
                    };
                    let mut carry = Carry {
                        from,
                        to,
                        open: self.open_items(prev)?,
                        last: self.last_exchanges(prev)?,
                        summary: String::new(),
                    };
                    let mut usage = crate::types::Usage::default();
                    let mut answered = None;
                    let effective = compaction::effective(rows);
                    if !effective.is_empty() {
                        let old =
                            to_messages(effective).map_err(|e| StepError::Failed(e.to_string()))?;
                        let req = ChatRequest {
                            model: model.model.clone(),
                            system: compaction::SYSTEM_PROMPT.into(),
                            messages: vec![compaction::summary_request(&compaction::transcript(
                                &old,
                            ))],
                            tools: vec![],
                            max_tokens: compaction::SUMMARY_MAX_TOKENS,
                            forbid_tool_calls: false,
                        };
                        let turn = self.call_llm(&run.id, provider, req, cancel, false).await?;
                        carry.summary = turn.message.text();
                        usage = turn.usage;
                        answered = turn.fallback;
                    }
                    let message = Message::user_text(carry.render());
                    Ok(StepOutput::new(CarryOutput {
                        prev_session_id: prev.to_owned(),
                        carry,
                    })
                    .with_usage(usage)
                    .answered_by(answered.as_ref())
                    .with_messages(vec![message]))
                },
            )
            .await?;
        if !out.replayed {
            tracing::info!(run = %run.id, prev, "carried the previous segment over");
        }
        Ok(())
    }

    /// Runs and sub-agents still working in `session`.
    fn open_items(&self, session: &str) -> Result<Vec<String>, StepError> {
        let mut open = Vec::new();
        for r in self.store.runs_for_session(session)? {
            if r.parent_run_id.is_some() {
                continue;
            }
            if matches!(r.status.as_str(), "running" | "queued") {
                open.push("a reply there is still being worked on".to_owned());
            }
            let mut stack = vec![r.id.clone()];
            while let Some(id) = stack.pop() {
                for child in self.store.children(&id)? {
                    if matches!(child.status.as_str(), "running" | "queued") {
                        let label = child.label.clone().unwrap_or_else(|| "a sub-agent".into());
                        open.push(format!("{label} still running"));
                    }
                    stack.push(child.id);
                }
            }
        }
        Ok(open)
    }

    /// The last two exchanges of `session`: each top-level run's input and
    /// its final text, cut to [`CARRY_EXCHANGES_MAX`] in all by `render`.
    fn last_exchanges(&self, session: &str) -> Result<Vec<(String, String)>, StepError> {
        let rows = self.store.session_messages(session)?;
        let runs: Vec<RunRow> = self
            .store
            .runs_for_session(session)?
            .into_iter()
            .filter(|r| r.parent_run_id.is_none())
            .collect();
        let mut out = Vec::new();
        for r in runs.iter().rev().take(2).rev() {
            let you = r
                .input_message_id
                .and_then(|id| rows.iter().find(|m| m.id == id))
                .map(|m| m.text.clone())
                .unwrap_or_default();
            let tern = rows
                .iter()
                .rev()
                .find(|m| m.run_id.as_deref() == Some(r.id.as_str()) && m.role == "assistant")
                .and_then(|m| to_messages(vec![m.clone()]).ok())
                .and_then(|m| m.first().map(Message::text))
                .unwrap_or_default();
            if !you.trim().is_empty() {
                out.push((cut(&you), cut(&tern)));
            }
        }
        Ok(out)
    }
}

fn cut(s: &str) -> String {
    let s = s.trim();
    crate::text::prefix_bytes(s, CARRY_EXCHANGES_MAX).to_owned()
}
