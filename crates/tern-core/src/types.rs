//! Provider-neutral conversation and streaming types.

use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// Concatenated text blocks.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for b in &self.content {
            if let ContentBlock::Text { text } = b {
                out.push_str(text);
            }
        }
        out
    }

    /// Plain-text rendering of every block, for full-text search.
    pub fn search_text(&self) -> String {
        let parts: Vec<String> = self
            .content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => text.clone(),
                ContentBlock::ToolUse { name, input, .. } => format!("{name} {input}"),
                ContentBlock::ToolResult { content, .. } => content.clone(),
            })
            .collect();
        parts.join("\n")
    }

    pub fn tool_uses(&self) -> impl Iterator<Item = ToolCall> + '_ {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some(ToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    /// Offer `tools` (requests with tool history must) but let the model
    /// only answer in text: a run's closing turn. Left out of the
    /// serialized request when false, so journaled digests don't change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forbid_tool_calls: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ToolUseStart {
        id: String,
        name: String,
    },
    ToolUseInputDelta {
        id: String,
        partial_json: String,
    },
    MessageStop {
        usage: Usage,
        stop_reason: StopReason,
    },
    /// Sent first when a fallback model answers instead of the requested
    /// one (T6a): what answered, and why the earlier ones didn't.
    Fallback(Fallback),
}

/// A turn answered by a later model in a fallback chain (T6a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fallback {
    /// `provider:model` that answered.
    pub model: String,
    /// Why the models before it were passed over, e.g. `anthropic:x: HTTP 529`.
    pub reason: String,
}

/// A fully assembled model turn: what the journal stores for an `llm` step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantTurn {
    pub message: Message,
    pub usage: Usage,
    pub stop_reason: StopReason,
    /// Set when a fallback model answered. Absent otherwise, so journaled
    /// turns from before T6a read back unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Fallback>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP {status}: {body}")]
    Http {
        status: u16,
        body: String,
        retry_after: Option<Duration>,
    },
    #[error("network error: {0}")]
    Network(String),
    #[error("provider stream error: {message}")]
    Stream { message: String, retryable: bool },
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Config(String),
}

impl ProviderError {
    pub fn is_retryable(&self) -> bool {
        match self {
            ProviderError::Http { status, .. } => {
                *status == 429 || *status == 408 || *status >= 500
            }
            ProviderError::Network(_) => true,
            ProviderError::Stream { retryable, .. } => *retryable,
            ProviderError::Cancelled | ProviderError::Config(_) => false,
        }
    }

    /// Whether the next model in a fallback chain should get the call once
    /// retries are spent (T6a): server errors, overload, rate limits,
    /// network trouble, and auth or billing failures (401, 402, 403), since
    /// another provider has its own key and account. Never other 4xx: the
    /// request itself is wrong and would be wrong anywhere.
    pub fn moves_to_fallback(&self) -> bool {
        match self {
            ProviderError::Http { status, .. } => {
                self.is_retryable() || (401..=403).contains(status)
            }
            ProviderError::Network(_) => true,
            ProviderError::Stream { retryable, .. } => *retryable,
            ProviderError::Cancelled | ProviderError::Config(_) => false,
        }
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ProviderError::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    /// Output tokens to ask for per turn, when the provider knows its models
    /// allow more than the agent's default. Too low and long tool calls (a
    /// whole file in one write) are cut off mid-argument.
    fn max_output_tokens(&self) -> Option<u32> {
        None
    }
    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_wire_format() {
        let b = ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "ok".into(),
            is_error: false,
        };
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(v["type"], "tool_result");
        assert_eq!(serde_json::from_value::<ContentBlock>(v).unwrap(), b);
    }

    #[test]
    fn retryability() {
        let http = |status| ProviderError::Http {
            status,
            body: String::new(),
            retry_after: None,
        };
        assert!(http(429).is_retryable());
        assert!(http(529).is_retryable());
        assert!(!http(400).is_retryable());
        assert!(!ProviderError::Cancelled.is_retryable());
    }

    #[test]
    fn fallback_eligibility() {
        let http = |status| ProviderError::Http {
            status,
            body: String::new(),
            retry_after: None,
        };
        for s in [401, 402, 403, 429, 500, 503, 529] {
            assert!(http(s).moves_to_fallback(), "{s}");
        }
        for s in [400, 404, 413, 422] {
            assert!(!http(s).moves_to_fallback(), "{s}");
        }
        assert!(ProviderError::Network("reset".into()).moves_to_fallback());
        assert!(!ProviderError::Cancelled.moves_to_fallback());
        assert!(!ProviderError::Config("no key".into()).moves_to_fallback());
    }
}
