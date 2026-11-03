//! Scripted provider for tests, selected with `TERN_PROVIDER=mock:<script.json>`.
//!
//! The reply is chosen by how many assistant turns the request already
//! contains, not by call count, so a resumed process picks up where the
//! crashed one left off. Compaction requests get a summary that records how
//! many turns it replaced (`mock-turns=N`), so counting survives compaction.
//!
//! ```json
//! { "turns": [
//!     { "text": "Writing it.", "tool_calls": [{ "name": "write_file", "input": { "path": "a", "content": "hi" } }] },
//!     { "text": "Done." }
//! ] }
//! ```

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tern_core::types::{
    ChatRequest, EventStream, Provider, ProviderError, Role, StopReason, StreamEvent, Usage,
};
use tokio_util::sync::CancellationToken;

/// Assistant turns in the request, including ones folded into summaries.
pub fn turns_so_far(req: &ChatRequest) -> usize {
    let live = req
        .messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .count();
    let summarized: usize = req
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| summarized_turns(&m.text()))
        .sum();
    live + summarized
}

fn summarized_turns(text: &str) -> usize {
    text.split("mock-turns=")
        .skip(1)
        .filter_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<usize>().ok()
        })
        .sum()
}

fn text_stream(text: String) -> EventStream {
    let usage = Usage {
        output_tokens: text.len() as u64 / 4,
        ..Usage::default()
    };
    Box::pin(futures::stream::iter(vec![
        Ok(StreamEvent::TextDelta(text)),
        Ok(StreamEvent::MessageStop {
            usage,
            stop_reason: StopReason::EndTurn,
        }),
    ]))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Script {
    #[serde(default)]
    pub turns: Vec<Turn>,
    /// Alternative scripts chosen by the conversation's first user message:
    /// the first entry whose `matches` it contains wins; otherwise `turns`.
    #[serde(default)]
    pub conversations: Vec<Conversation>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Conversation {
    pub matches: String,
    pub turns: Vec<Turn>,
}

impl Script {
    fn turns_for(&self, req: &ChatRequest) -> &[Turn] {
        let first = req
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .map(|m| m.text())
            .unwrap_or_default();
        self.conversations
            .iter()
            .find(|c| first.contains(&c.matches))
            .map_or(&self.turns, |c| &c.turns)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Turn {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tool_calls: Vec<ScriptedCall>,
    /// Delay before the reply streams, for cancellation tests.
    #[serde(default)]
    pub delay_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScriptedCall {
    pub name: String,
    #[serde(default)]
    pub input: Value,
}

pub struct MockProvider {
    script: Script,
}

impl MockProvider {
    pub fn new(script: Script) -> Self {
        Self { script }
    }

    pub fn from_file(path: &Path) -> Result<Self, ProviderError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ProviderError::Config(format!("mock script {}: {e}", path.display())))?;
        let script = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Config(format!("mock script {}: {e}", path.display())))?;
        Ok(Self::new(script))
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        if req.system == tern_core::compaction::SYSTEM_PROMPT {
            let text = req.messages.iter().map(|m| m.text()).collect::<String>();
            let n = text.matches("### assistant").count() + summarized_turns(&text);
            return Ok(text_stream(format!("mock summary mock-turns={n}")));
        }
        let n = turns_so_far(&req);
        let turn = self
            .script
            .turns_for(&req)
            .get(n)
            .cloned()
            .unwrap_or_else(|| Turn {
                text: "(mock script exhausted)".into(),
                ..Turn::default()
            });
        if turn.delay_ms > 0 {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(turn.delay_ms)) => {}
                () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            }
        }
        let mut events = Vec::new();
        let chars: Vec<char> = turn.text.chars().collect();
        for chunk in chars.chunks(8) {
            events.push(Ok(StreamEvent::TextDelta(chunk.iter().collect())));
        }
        for (i, call) in turn.tool_calls.iter().enumerate() {
            let id = format!("mock_{n}_{i}");
            events.push(Ok(StreamEvent::ToolUseStart {
                id: id.clone(),
                name: call.name.clone(),
            }));
            events.push(Ok(StreamEvent::ToolUseInputDelta {
                id,
                partial_json: call.input.to_string(),
            }));
        }
        events.push(Ok(StreamEvent::MessageStop {
            usage: Usage {
                input_tokens: tern_core::compaction::estimate_tokens(
                    &req.system,
                    &req.messages,
                    &req.tools,
                ),
                output_tokens: turn.text.len() as u64 / 4,
                ..Usage::default()
            },
            stop_reason: if turn.tool_calls.is_empty() {
                StopReason::EndTurn
            } else {
                StopReason::ToolUse
            },
        }));
        Ok(Box::pin(futures::stream::iter(events)))
    }
}
