//! Scripted provider for tests, selected with `TERN_PROVIDER=mock:<script.json>`.
//!
//! The reply is chosen by how many assistant turns the request already
//! contains, not by call count, so a resumed process picks up where the
//! crashed one left off.
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

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Script {
    pub turns: Vec<Turn>,
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
        let n = req
            .messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .count();
        let turn = self.script.turns.get(n).cloned().unwrap_or_else(|| Turn {
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
        let input_chars: usize = req.messages.iter().map(|m| m.search_text().len()).sum();
        events.push(Ok(StreamEvent::MessageStop {
            usage: Usage {
                input_tokens: (req.system.len() + input_chars) as u64 / 4,
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
