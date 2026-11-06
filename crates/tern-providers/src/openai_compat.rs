//! OpenAI-compatible Chat Completions (OpenRouter, Ollama, vLLM, LM Studio).

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tern_core::types::{
    ChatRequest, ContentBlock, EventStream, Provider, ProviderError, Role, StopReason, StreamEvent,
    Usage,
};
use tokio_util::sync::CancellationToken;

use crate::{http, sse};

pub struct OpenAiCompat {
    id: String,
    pub(crate) base_url: String,
    api_key: Option<String>,
    /// OpenRouter asks apps to identify themselves with two headers.
    pub(crate) attribution: bool,
    client: reqwest::Client,
}

/// Sent to OpenRouter so the app shows up by name on its dashboards.
const OPENROUTER_REFERER: &str = "https://github.com/mitchellalderson/tern-agent";
const OPENROUTER_TITLE: &str = "tern";

impl OpenAiCompat {
    pub fn new(id: &str, base_url: &str, api_key: Option<String>) -> Result<Self, ProviderError> {
        // Tidy a base URL pasted wrong (a missing `/api/v1`, a pasted
        // `/chat/completions`), so a mistake in config doesn't 404 every call.
        let base_url = tern_core::presets::normalize_base_url(base_url);
        let attribution = tern_core::models::host_of(&base_url) == "openrouter.ai";
        Ok(Self {
            id: id.to_owned(),
            base_url,
            api_key,
            attribution,
            client: http::client()?,
        })
    }
}

pub fn request_body(req: &ChatRequest) -> Value {
    let mut messages = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({ "role": "system", "content": req.system }));
    }
    for m in &req.messages {
        let text: String = m
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        match m.role {
            Role::User => {
                // Tool results become `tool` messages, which must directly
                // follow the assistant message that requested them.
                for b in &m.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = b
                    {
                        let content = if *is_error {
                            format!("ERROR: {content}")
                        } else {
                            content.clone()
                        };
                        messages.push(json!({ "role": "tool", "tool_call_id": tool_use_id, "content": content }));
                    }
                }
                if !text.is_empty() {
                    messages.push(json!({ "role": "user", "content": text }));
                }
            }
            Role::Assistant => {
                let calls: Vec<Value> = m
                    .tool_uses()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": { "name": c.name, "arguments": c.input.to_string() },
                        })
                    })
                    .collect();
                let mut msg = json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                });
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                messages.push(msg);
            }
        }
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "stream": true,
        "stream_options": { "include_usage": true },
        "messages": messages,
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": { "name": t.name, "description": t.description, "parameters": t.input_schema },
                })
            })
            .collect();
        if req.forbid_tool_calls {
            body["tool_choice"] = json!("none");
        }
    }
    body
}

#[derive(Default)]
struct Decoder {
    /// index -> id of tool calls already announced.
    tools: BTreeMap<u64, String>,
    usage: Usage,
    finish: Option<StopReason>,
    done: bool,
}

impl Decoder {
    fn decode(&mut self, data: &str) -> Vec<Result<StreamEvent, ProviderError>> {
        if self.done {
            return vec![];
        }
        if data.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return vec![];
        };
        if let Some(err) = v.get("error") {
            let message = err["message"]
                .as_str()
                .unwrap_or("unknown error")
                .to_owned();
            return vec![Err(ProviderError::Stream {
                message,
                retryable: false,
            })];
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage.input_tokens = u["prompt_tokens"]
                .as_u64()
                .unwrap_or(self.usage.input_tokens);
            self.usage.output_tokens = u["completion_tokens"]
                .as_u64()
                .unwrap_or(self.usage.output_tokens);
            self.usage.cache_read_tokens = u["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0);
        }
        let mut out = Vec::new();
        let Some(choice) = v["choices"].get(0) else {
            return out;
        };
        let delta = &choice["delta"];
        // Reasoning rides beside `content` under a few names: DeepSeek and
        // Gemini's endpoint say `reasoning_content`, OpenRouter `reasoning`.
        if let Some(t) = ["reasoning_content", "reasoning"]
            .iter()
            .find_map(|k| delta[*k].as_str().filter(|t| !t.is_empty()))
        {
            out.push(Ok(StreamEvent::ReasoningDelta(t.to_owned())));
        }
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            out.push(Ok(StreamEvent::TextDelta(t.to_owned())));
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for (pos, call) in calls.iter().enumerate() {
                let index = call["index"].as_u64().unwrap_or(pos as u64);
                let id = match self.tools.get(&index) {
                    Some(id) => id.clone(),
                    None => {
                        let id = call["id"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("call_{index}"));
                        let name = call["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                        self.tools.insert(index, id.clone());
                        out.push(Ok(StreamEvent::ToolUseStart {
                            id: id.clone(),
                            name,
                        }));
                        id
                    }
                };
                let args = &call["function"]["arguments"];
                let partial = match args {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    // Some servers send arguments as an object.
                    other => other.to_string(),
                };
                if !partial.is_empty() {
                    out.push(Ok(StreamEvent::ToolUseInputDelta {
                        id,
                        partial_json: partial,
                    }));
                }
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish = Some(match reason {
                "stop" => StopReason::EndTurn,
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                _ => StopReason::Other,
            });
        }
        out
    }

    fn finish(&mut self) -> Vec<Result<StreamEvent, ProviderError>> {
        if self.done {
            return vec![];
        }
        self.done = true;
        let stop_reason = if self.tools.is_empty() {
            self.finish.unwrap_or(StopReason::EndTurn)
        } else {
            StopReason::ToolUse
        };
        vec![Ok(StreamEvent::MessageStop {
            usage: self.usage,
            stop_reason,
        })]
    }
}

#[async_trait]
impl Provider for OpenAiCompat {
    fn id(&self) -> &str {
        &self.id
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let mut builder = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&request_body(&req));
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        if self.attribution {
            builder = builder
                .header("HTTP-Referer", OPENROUTER_REFERER)
                .header("X-Title", OPENROUTER_TITLE);
        }
        let resp = tokio::select! {
            r = builder.send() => r.map_err(http::network)?,
            () = cancel.cancelled() => return Err(ProviderError::Cancelled),
        };
        let resp = http::check(resp).await?;
        let mut decoder = Decoder::default();
        // A trailing `None` lets servers that omit `[DONE]` still finish cleanly.
        let stream = sse::events(resp)
            .map(Some)
            .chain(futures::stream::once(async { None }))
            .flat_map(move |ev| {
                let items = match ev {
                    Some(Ok(ev)) => decoder.decode(&ev.data),
                    Some(Err(e)) => vec![Err(http::network(e))],
                    None => decoder.finish(),
                };
                futures::stream::iter(items)
            });
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_core::types::{Message, ToolSpec};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req() -> ChatRequest {
        ChatRequest {
            model: "qwen".into(),
            system: "sys".into(),
            messages: vec![
                Message::user_text("hi"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "c1".into(),
                        name: "read_file".into(),
                        input: json!({"path": "a"}),
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "c1".into(),
                        content: "nope".into(),
                        is_error: true,
                    }],
                },
            ],
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "Read".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: 50,
            forbid_tool_calls: false,
        }
    }

    #[test]
    fn body_maps_tool_turns() {
        let b = request_body(&req());
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[0]["role"], "system");
        assert_eq!(
            m[2]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a\"}"
        );
        assert_eq!(m[2]["content"], Value::Null);
        assert_eq!(m[3]["role"], "tool");
        assert_eq!(m[3]["content"], "ERROR: nope");
        assert_eq!(b["tools"][0]["function"]["name"], "read_file");
        assert!(b.get("tool_choice").is_none());

        let mut r = req();
        r.forbid_tool_calls = true;
        assert_eq!(request_body(&r)["tool_choice"], "none");
    }

    async fn collect(body: &str) -> Vec<StreamEvent> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer k"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(body.to_owned(), "text/event-stream"),
            )
            .mount(&server)
            .await;
        let p = OpenAiCompat::new("or", &format!("{}/v1", server.uri()), Some("k".into())).unwrap();
        p.stream(req(), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[tokio::test]
    async fn streams_tool_calls_and_usage() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":1}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(
            collect(body).await,
            [
                StreamEvent::TextDelta("ok ".into()),
                StreamEvent::ToolUseStart {
                    id: "call_a".into(),
                    name: "read_file".into()
                },
                StreamEvent::ToolUseInputDelta {
                    id: "call_a".into(),
                    partial_json: "{\"pa".into()
                },
                StreamEvent::ToolUseInputDelta {
                    id: "call_a".into(),
                    partial_json: "th\":1}".into()
                },
                StreamEvent::MessageStop {
                    usage: Usage {
                        input_tokens: 7,
                        output_tokens: 3,
                        ..Usage::default()
                    },
                    stop_reason: StopReason::ToolUse,
                },
            ]
        );
    }

    #[tokio::test]
    async fn parses_reasoning_under_either_field_name() {
        // DeepSeek / Gemini's endpoint: `reasoning_content`. OpenRouter: `reasoning`.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"ok.\",\"content\":null}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"\",\"content\":\"Hi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let ev = collect(body).await;
        assert_eq!(
            &ev[..3],
            [
                StreamEvent::ReasoningDelta("hmm ".into()),
                StreamEvent::ReasoningDelta("ok.".into()),
                StreamEvent::TextDelta("Hi".into()),
            ]
        );
    }

    #[test]
    fn reasoning_is_never_sent_back() {
        let mut r = req();
        r.messages.push(tern_core::types::Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "private".into(),
                    signature: Some("sig".into()),
                    duration_ms: None,
                },
                ContentBlock::RedactedThinking { data: "enc".into() },
                ContentBlock::Text {
                    text: "answer".into(),
                },
            ],
        });
        let s = request_body(&r).to_string();
        assert!(!s.contains("private") && !s.contains("enc\"") && s.contains("answer"));
    }

    #[tokio::test]
    async fn finishes_without_done_marker_and_fills_missing_ids() {
        // Ollama-style: whole arguments object, no id, no [DONE].
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"name\":\"ls\",\"arguments\":{\"p\":1}}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );
        let ev = collect(body).await;
        assert_eq!(
            ev[0],
            StreamEvent::ToolUseStart {
                id: "call_0".into(),
                name: "ls".into()
            }
        );
        assert_eq!(
            ev[1],
            StreamEvent::ToolUseInputDelta {
                id: "call_0".into(),
                partial_json: "{\"p\":1}".into()
            }
        );
        assert!(matches!(
            ev[2],
            StreamEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
        assert_eq!(ev.len(), 3);
    }
}
