//! The step journal: memoized, crash-safe execution of run steps (spec §5).
//!
//! Every step is persisted as `started` before it runs and `completed` (with
//! its output and any conversation messages it produced, in one transaction)
//! after. Replaying a run returns stored outputs instead of executing again.

use std::future::Future;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tern_store::{NewMessage, Store, StoreError};

use crate::types::{Message, Usage};

#[cfg(test)]
mod tests;

/// Env var: abort the process right after step `n` is marked `started`.
pub const CRASH_AFTER_ENV: &str = "TERN_CRASH_AFTER_STEP";

const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Llm,
    Tool,
    Approval,
    Compaction,
}

impl StepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Llm => "llm",
            StepKind::Tool => "tool",
            StepKind::Approval => "approval",
            StepKind::Compaction => "compaction",
        }
    }
}

/// What to do when a step is found `started`, i.e. the process died mid-step.
pub enum InFlight<O> {
    /// Execute again (LLM calls, compaction, approvals, idempotent tools).
    Retry,
    /// Don't execute; record and return this output instead (non-idempotent tools).
    Substitute(StepOutput<O>),
}

/// A step's result plus what to persist alongside it.
#[derive(Debug, Clone)]
pub struct StepOutput<O> {
    pub output: O,
    pub usage: Option<Usage>,
    /// Conversation messages this step appends, committed with the step.
    pub messages: Vec<Message>,
    /// Marks the appended messages as a compaction summary replacing every
    /// message with id <= this value.
    pub summary_of: Option<i64>,
}

impl<O> StepOutput<O> {
    pub fn new(output: O) -> Self {
        Self {
            output,
            usage: None,
            messages: Vec::new(),
            summary_of: None,
        }
    }

    pub fn with_summary_of(mut self, cover_through: i64) -> Self {
        self.summary_of = Some(cover_through);
        self
    }

    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = Some(usage);
        self
    }

    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome<O> {
    pub value: O,
    /// True if the value came from the journal rather than a live execution.
    pub replayed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum StepError {
    #[error("{0}")]
    Failed(String),
    #[error("cancelled")]
    Cancelled,
    #[error("journal storage: {0}")]
    Store(#[from] StoreError),
    #[error("journal serialization: {0}")]
    Serde(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct Journal {
    store: Store,
    redactions: Arc<Vec<String>>,
    crash_after: Option<u32>,
}

impl Journal {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            redactions: Arc::new(Vec::new()),
            crash_after: None,
        }
    }

    /// Honor `TERN_CRASH_AFTER_STEP` (fault injection for tests).
    pub fn with_crash_after_from_env(mut self) -> Self {
        self.crash_after = std::env::var(CRASH_AFTER_ENV)
            .ok()
            .and_then(|v| v.parse().ok());
        self
    }

    /// Secret values to scrub from everything the journal writes.
    pub fn with_redactions(mut self, secrets: impl IntoIterator<Item = String>) -> Self {
        let mut v: Vec<String> = secrets.into_iter().filter(|s| s.len() >= 4).collect();
        // Longest first, so a secret that contains another is fully replaced.
        v.sort_by_key(|s| std::cmp::Reverse(s.len()));
        self.redactions = Arc::new(v);
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Execute `f` as step `idx` of `run`, or return its memoized output.
    /// Crashed-in-flight steps are retried.
    pub async fn step<I, O, F, Fut>(
        &self,
        run: &str,
        idx: u32,
        kind: StepKind,
        input: &I,
        f: F,
    ) -> Result<O, StepError>
    where
        I: Serialize,
        O: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<O, StepError>>,
    {
        let outcome = self
            .step_with(run, idx, kind, input, InFlight::Retry, || async {
                f().await.map(StepOutput::new)
            })
            .await?;
        Ok(outcome.value)
    }

    /// Full form of [`Journal::step`]: choose the in-flight policy and persist
    /// usage and derived messages with the step.
    pub async fn step_with<I, O, F, Fut>(
        &self,
        run: &str,
        idx: u32,
        kind: StepKind,
        input: &I,
        in_flight: InFlight<O>,
        f: F,
    ) -> Result<Outcome<O>, StepError>
    where
        I: Serialize,
        O: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<StepOutput<O>, StepError>>,
    {
        let input = serde_json::to_value(input)?;
        let hash = input_hash(kind, &input)?;

        if let Some(row) = self.store.step(run, idx)? {
            if row.input_hash != hash {
                tracing::warn!(
                    run,
                    idx,
                    kind = kind.as_str(),
                    "journal divergence: step input changed; discarding this and later steps"
                );
                self.store.truncate_steps(run, idx)?;
            } else {
                match row.status.as_str() {
                    "completed" => {
                        let stored = row.output_json.as_deref().unwrap_or("null");
                        return Ok(Outcome {
                            value: serde_json::from_str(stored)?,
                            replayed: true,
                        });
                    }
                    "started" => {
                        if let InFlight::Substitute(sub) = in_flight {
                            tracing::warn!(
                                run,
                                idx,
                                "step was interrupted by a crash; not re-executing"
                            );
                            self.complete(run, idx, &sub)?;
                            return Ok(Outcome {
                                value: sub.output,
                                replayed: false,
                            });
                        }
                        tracing::info!(
                            run,
                            idx,
                            kind = kind.as_str(),
                            "re-executing step interrupted by a crash"
                        );
                    }
                    // `failed`: retried by the caller's policy; execute again.
                    _ => {}
                }
            }
        }

        let mut redacted_input = input;
        self.redact(&mut redacted_input);
        self.store.start_step(
            run,
            idx,
            kind.as_str(),
            &hash,
            &serde_json::to_string(&redacted_input)?,
        )?;
        if self.crash_after == Some(idx) {
            tracing::error!(run, idx, "{CRASH_AFTER_ENV} reached; aborting");
            std::process::abort();
        }

        match f().await {
            Ok(out) => {
                self.complete(run, idx, &out)?;
                Ok(Outcome {
                    value: out.output,
                    replayed: false,
                })
            }
            Err(e) => {
                self.store
                    .fail_step(run, idx, &self.redact_str(&e.to_string()))?;
                Err(e)
            }
        }
    }

    fn complete<O: Serialize>(
        &self,
        run: &str,
        idx: u32,
        out: &StepOutput<O>,
    ) -> Result<(), StepError> {
        let mut output = serde_json::to_value(&out.output)?;
        self.redact(&mut output);
        let usage = out.usage.as_ref().map(serde_json::to_string).transpose()?;
        let messages = out
            .messages
            .iter()
            .map(|m| {
                self.to_row(m).map(|mut row| {
                    row.summary_of = out.summary_of;
                    row
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.store.complete_step(
            run,
            idx,
            &serde_json::to_string(&output)?,
            usage.as_deref(),
            &messages,
        )?;
        Ok(())
    }

    /// Serialize a message for storage, redacting secrets.
    pub fn to_row(&self, m: &Message) -> Result<NewMessage, serde_json::Error> {
        let mut content = serde_json::to_value(&m.content)?;
        self.redact(&mut content);
        Ok(NewMessage {
            role: m.role.as_str().to_owned(),
            content_json: serde_json::to_string(&content)?,
            text: self.redact_str(&m.search_text()),
            summary_of: None,
        })
    }

    fn redact(&self, v: &mut Value) {
        if self.redactions.is_empty() {
            return;
        }
        match v {
            Value::String(s) => {
                if self.redactions.iter().any(|r| s.contains(r.as_str())) {
                    *s = self.redact_str(s);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|v| self.redact(v)),
            Value::Object(o) => o.values_mut().for_each(|v| self.redact(v)),
            _ => {}
        }
    }

    fn redact_str(&self, s: &str) -> String {
        let mut out = s.to_owned();
        for r in self.redactions.iter() {
            if out.contains(r.as_str()) {
                out = out.replace(r.as_str(), REDACTED);
            }
        }
        out
    }
}

/// blake3 over the canonical JSON of `{kind, input}`.
pub fn input_hash<I: Serialize>(kind: StepKind, input: &I) -> Result<String, serde_json::Error> {
    let value = serde_json::json!({
        "kind": kind.as_str(),
        "input": serde_json::to_value(input)?,
    });
    let mut canonical = String::new();
    write_canonical(&value, &mut canonical)?;
    Ok(blake3::hash(canonical.as_bytes()).to_hex().to_string())
}

/// Compact JSON with object keys sorted. Explicit rather than relying on
/// `serde_json`'s default `BTreeMap`, which any dependency enabling the
/// `preserve_order` feature would silently change.
fn write_canonical(v: &Value, out: &mut String) -> Result<(), serde_json::Error> {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k)?);
                out.push(':');
                if let Some(child) = map.get(k) {
                    write_canonical(child, out)?;
                }
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar)?),
    }
    Ok(())
}
