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

/// Whether `model` on OpenRouter is an Anthropic one, which (alone among
/// OpenRouter's models) takes Anthropic-style `cache_control` markers on
/// text parts. Everything else caches by itself from a stable prefix.
fn is_anthropic_on_openrouter(model: &str) -> bool {
    model.to_ascii_lowercase().starts_with("anthropic/")
}

pub fn request_body(req: &ChatRequest) -> Value {
    request_body_for(req, false)
}

/// `anthropic_cache`: mark the system prompt and the last user message with
/// `cache_control` (OpenRouter passes it to Anthropic) and send per-run
/// context as a closing user message, after both, so the cached prefix stays
/// identical. Otherwise the request is as plain Chat Completions, with the
/// per-run context folded into `system`.
pub fn request_body_for(req: &ChatRequest, anthropic_cache: bool) -> Value {
    let mut messages = Vec::new();
    let min = crate::anthropic::min_cacheable_tokens(&req.model);
    let ephemeral = json!({ "type": "ephemeral" });
    let part = |text: &str, mark: bool| {
        let mut p = json!({ "type": "text", "text": text });
        if mark {
            p["cache_control"] = ephemeral.clone();
        }
        json!([p])
    };
    let system = if anthropic_cache {
        req.system.clone()
    } else {
        req.full_system()
    };
    if !system.is_empty() {
        let tools_len: usize = req
            .tools
            .iter()
            .map(|t| t.description.len() + t.input_schema.to_string().len())
            .sum();
        let mark = anthropic_cache && (system.len() + tools_len) / 4 >= min;
        messages.push(json!({
            "role": "system",
            "content": if mark { part(&system, true) } else { Value::String(system) },
        }));
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
    if anthropic_cache {
        let prefix_len: usize = messages.iter().map(|m| m.to_string().len()).sum();
        if (prefix_len / 4) >= min
            && let Some(m) = messages
                .iter_mut()
                .rev()
                .find(|m| m["role"] == "user" && m["content"].is_string())
        {
            let text = m["content"].as_str().unwrap_or_default().to_owned();
            m["content"] = part(&text, true);
        }
        if !req.cache.volatile.is_empty() {
            messages.push(json!({
                "role": "user",
                "content": format!(
                    "Context for this run (from tern, not from the user):\n\n{}",
                    req.cache.volatile
                ),
            }));
        }
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": tern_core::limits::clamp_output(&req.model, req.max_tokens),
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
            // `prompt_tokens` counts cached tokens too; `input_tokens` is the
            // uncached rest (as Anthropic reports it), so cache hit rate and
            // pricing add up.
            let prompt = u["prompt_tokens"].as_u64();
            let details = &u["prompt_tokens_details"];
            self.usage.cache_read_tokens = details["cached_tokens"].as_u64().unwrap_or(0);
            self.usage.cache_write_tokens = details["cache_write_tokens"].as_u64().unwrap_or(0);
            self.usage.input_tokens = match prompt {
                Some(p) => {
                    p.saturating_sub(self.usage.cache_read_tokens + self.usage.cache_write_tokens)
                }
                None => self.usage.input_tokens,
            };
            self.usage.output_tokens = u["completion_tokens"]
                .as_u64()
                .unwrap_or(self.usage.output_tokens);
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
            .json(&request_body_for(
                &req,
                self.attribution && is_anthropic_on_openrouter(&req.model),
            ));
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
            cache: Default::default(),
        }
    }

    fn cached_req() -> ChatRequest {
        let mut r = req();
        r.model = "anthropic/claude-sonnet-4.5".into();
        r.system = "s ".repeat(4000);
        r.cache.history = true;
        r.cache.volatile = "## Current time\n\nMonday".into();
        r
    }

    #[test]
    fn anthropic_on_openrouter_marks_system_and_last_user_and_trails_the_context() {
        let b = request_body_for(&cached_req(), true);
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[0]["content"][0]["cache_control"]["type"], "ephemeral");
        assert!(!m[0].to_string().contains("Monday"));
        assert_eq!(m[1]["content"][0]["cache_control"]["type"], "ephemeral");
        let last = m.last().unwrap();
        assert_eq!(last["role"], "user");
        assert!(last["content"].as_str().unwrap().contains("Monday"));
        // Nothing else is touched: no marker outside the two.
        assert_eq!(b.to_string().matches("cache_control").count(), 2);
    }

    #[test]
    fn other_openai_compatible_requests_are_plain_with_the_context_in_system() {
        let b = request_body_for(&cached_req(), false);
        assert!(!b.to_string().contains("cache_control"));
        let m = b["messages"].as_array().unwrap();
        assert!(m[0]["content"].as_str().unwrap().contains("Monday"));
        assert!(
            !m.last()
                .unwrap()
                .to_string()
                .contains("Context for this run")
        );
    }

    #[test]
    fn cached_prompt_tokens_are_split_out_of_input() {
        let mut d = Decoder::default();
        d.decode(r#"{"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":800,"cache_write_tokens":100}}}"#);
        assert_eq!(
            (
                d.usage.input_tokens,
                d.usage.cache_read_tokens,
                d.usage.cache_write_tokens
            ),
            (100, 800, 100)
        );
        assert_eq!(d.usage.cache_hit_rate(), Some(0.8));
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
