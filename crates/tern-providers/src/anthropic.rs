//! Anthropic Messages API with streaming, tool use, and prompt caching.

use std::collections::HashMap;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tern_core::types::{
    ChatRequest, ContentBlock, EventStream, Provider, ProviderError, StopReason, StreamEvent, Usage,
};
use tokio_util::sync::CancellationToken;

use crate::{http, sse};

const API_VERSION: &str = "2023-06-01";
/// Every current Claude model allows at least this many output tokens.
const MAX_OUTPUT_TOKENS: u32 = 32_000;

pub struct Anthropic {
    id: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl Anthropic {
    pub fn new(id: &str, base_url: &str, api_key: String) -> Result<Self, ProviderError> {
        Ok(Self {
            id: id.to_owned(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            client: http::client()?,
        })
    }
}

/// Build the request body. The system prompt and the tool list each carry a
/// cache breakpoint so both are reused across turns.
pub fn request_body(req: &ChatRequest) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let content: Vec<&ContentBlock> = m
                .content
                .iter()
                .filter(|b| !matches!(b, ContentBlock::Text { text } if text.is_empty()))
                .collect();
            json!({ "role": m.role.as_str(), "content": content })
        })
        .collect();
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "stream": true,
        "messages": messages,
    });
    if !req.system.is_empty() {
        body["system"] = json!([{
            "type": "text",
            "text": req.system,
            "cache_control": { "type": "ephemeral" },
        }]);
    }
    if !req.tools.is_empty() {
        let mut tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema }))
            .collect();
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({ "type": "ephemeral" });
        }
        body["tools"] = Value::Array(tools);
    }
    body
}

#[derive(Default)]
struct Decoder {
    tool_ids: HashMap<u64, String>,
    usage: Usage,
    stop_reason: Option<StopReason>,
}

impl Decoder {
    fn decode(&mut self, ev: &sse::SseEvent) -> Vec<Result<StreamEvent, ProviderError>> {
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            return vec![];
        };
        let kind = v["type"].as_str().unwrap_or_default();
        match kind {
            "message_start" => {
                let u = &v["message"]["usage"];
                self.usage.input_tokens = u["input_tokens"].as_u64().unwrap_or(0);
                self.usage.cache_read_tokens = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                self.usage.cache_write_tokens =
                    u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                self.usage.output_tokens = u["output_tokens"].as_u64().unwrap_or(0);
                vec![]
            }
            "content_block_start" => {
                let block = &v["content_block"];
                match block["type"].as_str() {
                    Some("tool_use") => {
                        let id = block["id"].as_str().unwrap_or_default().to_owned();
                        let name = block["name"].as_str().unwrap_or_default().to_owned();
                        if let Some(idx) = v["index"].as_u64() {
                            self.tool_ids.insert(idx, id.clone());
                        }
                        vec![Ok(StreamEvent::ToolUseStart { id, name })]
                    }
                    Some("text") => match block["text"].as_str() {
                        Some(t) if !t.is_empty() => vec![Ok(StreamEvent::TextDelta(t.to_owned()))],
                        _ => vec![],
                    },
                    _ => vec![],
                }
            }
            "content_block_delta" => {
                let d = &v["delta"];
                match d["type"].as_str() {
                    Some("text_delta") => {
                        vec![Ok(StreamEvent::TextDelta(
                            d["text"].as_str().unwrap_or_default().to_owned(),
                        ))]
                    }
                    Some("input_json_delta") => {
                        let id = v["index"]
                            .as_u64()
                            .and_then(|i| self.tool_ids.get(&i))
                            .cloned()
                            .unwrap_or_default();
                        vec![Ok(StreamEvent::ToolUseInputDelta {
                            id,
                            partial_json: d["partial_json"].as_str().unwrap_or_default().to_owned(),
                        })]
                    }
                    _ => vec![],
                }
            }
            "message_delta" => {
                if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                    self.usage.output_tokens = n;
                }
                self.stop_reason = v["delta"]["stop_reason"].as_str().map(|r| match r {
                    "end_turn" | "stop_sequence" => StopReason::EndTurn,
                    "tool_use" => StopReason::ToolUse,
                    "max_tokens" => StopReason::MaxTokens,
                    _ => StopReason::Other,
                });
                vec![]
            }
            "message_stop" => vec![Ok(StreamEvent::MessageStop {
                usage: self.usage,
                stop_reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
            })],
            "error" => {
                let err_type = v["error"]["type"].as_str().unwrap_or("error");
                let message = v["error"]["message"].as_str().unwrap_or("unknown error");
                vec![Err(ProviderError::Stream {
                    message: format!("{err_type}: {message}"),
                    retryable: matches!(
                        err_type,
                        "overloaded_error" | "api_error" | "rate_limit_error"
                    ),
                })]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Provider for Anthropic {
    fn id(&self) -> &str {
        &self.id
    }

    fn max_output_tokens(&self) -> Option<u32> {
        Some(MAX_OUTPUT_TOKENS)
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let send = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&request_body(&req))
            .send();
        let resp = tokio::select! {
            r = send => r.map_err(http::network)?,
            () = cancel.cancelled() => return Err(ProviderError::Cancelled),
        };
        let resp = http::check(resp).await?;
        let mut decoder = Decoder::default();
        let stream = sse::events(resp).flat_map(move |ev| {
            let items = match ev {
                Ok(ev) => decoder.decode(&ev),
                Err(e) => vec![Err(http::network(e))],
            };
            futures::stream::iter(items)
        });
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_core::types::{Message, Role, ToolSpec};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req() -> ChatRequest {
        ChatRequest {
            model: "claude-x".into(),
            system: "be brief".into(),
            messages: vec![Message::user_text("hi")],
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "Read".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: 100,
        }
    }

    #[test]
    fn body_has_cache_breakpoints() {
        let b = request_body(&req());
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["tools"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["messages"][0]["content"][0]["type"], "text");
        assert_eq!(b["stream"], true);
    }

    #[test]
    fn body_drops_empty_text_blocks() {
        let mut r = req();
        r.messages.push(Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: String::new(),
                },
                ContentBlock::ToolUse {
                    id: "t".into(),
                    name: "x".into(),
                    input: json!({}),
                },
            ],
        });
        let b = request_body(&r);
        assert_eq!(b["messages"][1]["content"].as_array().unwrap().len(), 1);
    }

    const STREAM: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me \"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"look.\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.txt\\\"}\"}}\n\n",
        "event: ping\ndata: {\"type\":\"ping\"}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":30}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    #[tokio::test]
    async fn streams_text_and_tool_use() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "k"))
            .and(header("anthropic-version", API_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_raw(STREAM, "text/event-stream"))
            .mount(&server)
            .await;
        let p = Anthropic::new("anthropic", &server.uri(), "k".into()).unwrap();
        let events: Vec<_> = p
            .stream(req(), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            events,
            [
                StreamEvent::TextDelta("Let me ".into()),
                StreamEvent::TextDelta("look.".into()),
                StreamEvent::ToolUseStart {
                    id: "toolu_1".into(),
                    name: "read_file".into()
                },
                StreamEvent::ToolUseInputDelta {
                    id: "toolu_1".into(),
                    partial_json: "{\"path\":".into()
                },
                StreamEvent::ToolUseInputDelta {
                    id: "toolu_1".into(),
                    partial_json: "\"a.txt\"}".into()
                },
                StreamEvent::MessageStop {
                    usage: Usage {
                        input_tokens: 12,
                        output_tokens: 30,
                        ..Usage::default()
                    },
                    stop_reason: StopReason::ToolUse,
                },
            ]
        );
    }

    #[tokio::test]
    async fn http_errors_carry_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "3")
                    .set_body_string("slow down"),
            )
            .mount(&server)
            .await;
        let p = Anthropic::new("anthropic", &server.uri(), "k".into()).unwrap();
        let Err(err) = p.stream(req(), CancellationToken::new()).await else {
            panic!("expected error");
        };
        assert!(err.is_retryable());
        assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(3)));
    }

    #[tokio::test]
    async fn overloaded_stream_error_is_retryable() {
        let server = MockServer::start().await;
        let body = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let p = Anthropic::new("anthropic", &server.uri(), "k".into()).unwrap();
        let mut s = p.stream(req(), CancellationToken::new()).await.unwrap();
        let err = s.next().await.unwrap().unwrap_err();
        assert!(err.is_retryable(), "{err}");
    }
}
