//! Anthropic Messages API with streaming, tool use, and prompt caching.

use std::collections::HashMap;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tern_core::config::CacheTtl;
use tern_core::types::{
    ChatRequest, ContentBlock, EventStream, Provider, ProviderError, Role, StopReason, StreamEvent,
    Usage,
};
use tokio_util::sync::CancellationToken;

use crate::{http, sse};

const API_VERSION: &str = "2023-06-01";

pub struct Anthropic {
    id: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    cache_ttl: CacheTtl,
}

impl Anthropic {
    pub fn new(id: &str, base_url: &str, api_key: String) -> Result<Self, ProviderError> {
        Ok(Self {
            id: id.to_owned(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            client: http::client()?,
            cache_ttl: CacheTtl::default(),
        })
    }

    /// Keep the cached system prompt and tools for `ttl` (`1h` costs more
    /// to write; see [`CacheTtl`]).
    pub fn with_cache_ttl(mut self, ttl: CacheTtl) -> Self {
        self.cache_ttl = ttl;
        self
    }
}

/// Anthropic allows at most this many `cache_control` breakpoints a request.
pub const MAX_BREAKPOINTS: usize = 4;

/// The shortest prefix, in tokens, Anthropic caches for `model`. A shorter
/// one with a breakpoint on it is silently not cached (nothing is written
/// or read), so we leave the breakpoint off. Unknown models get the
/// commonest minimum, 1024: a marker on a prefix that turns out too short
/// is ignored, so guessing low costs nothing.
pub fn min_cacheable_tokens(model: &str) -> usize {
    let m = model.to_ascii_lowercase().replace('.', "-");
    let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
    if has(&["opus-4-5", "opus-4-6", "haiku-4-5"]) {
        4096
    } else if has(&[
        "opus-4-7",
        "mythos-preview",
        "3-5-haiku",
        "haiku-3",
        "3-haiku",
    ]) {
        2048
    } else if has(&["sonnet-5-5", "haiku-5", "opus-5", "fable", "mythos"]) {
        512
    } else {
        1024
    }
}

fn cache_control(ttl: CacheTtl) -> Value {
    match ttl {
        CacheTtl::FiveMinutes => json!({ "type": "ephemeral" }),
        CacheTtl::OneHour => json!({ "type": "ephemeral", "ttl": "1h" }),
    }
}

/// Build the request body with the default (5-minute) cache lifetime.
pub fn request_body(req: &ChatRequest) -> Value {
    request_body_with(req, CacheTtl::FiveMinutes)
}

/// Build the request body.
///
/// Anthropic caches by prefix, in the order tools, system, messages, so the
/// body is laid out stable-first and carries up to four breakpoints
/// ([`MAX_BREAKPOINTS`]), each skipped when the prefix before it is under the
/// model's minimum:
///
/// 1. the last tool: tools alone, which several agents share;
/// 2. the system prompt: tools and system together (both use `ttl`);
/// 3. the last block of the previous user turn: a read point that survives
///    this turn's tool loop and, with it, the thinking blocks tern drops from
///    earlier turns (which change those turns' bytes);
/// 4. the last block of the history, moving forward each call, so a turn
///    reads everything the one before it wrote (history requests only).
///
/// Per-run context ([`tern_core::types::CacheHint::volatile`]) follows the
/// last breakpoint, in the final user message. Tools are sorted by name and
/// JSON objects serialize with sorted keys, so equal inputs give equal bytes.
pub fn request_body_with(req: &ChatRequest, ttl: CacheTtl) -> Value {
    // Thinking blocks go back only inside the turn in progress: after the
    // last user message that isn't tool results. Anthropic needs those
    // (signed) to continue a tool-use turn, and drops older ones itself, so
    // they are never sent.
    let is_turn_start = |m: &tern_core::types::Message| {
        m.role == Role::User
            && !m
                .content
                .iter()
                .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
    };
    let turn_start = req.messages.iter().rposition(is_turn_start).unwrap_or(0);
    let mut messages: Vec<Vec<Value>> = req
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut content: Vec<Value> = Vec::new();
            if m.role == Role::Assistant && i > turn_start {
                content.extend(m.echoable_thinking().map(thinking_wire));
            }
            content.extend(
                m.content
                    .iter()
                    .filter(|b| !b.is_thinking())
                    .filter(|b| !matches!(b, ContentBlock::Text { text } if text.is_empty()))
                    .map(|b| serde_json::to_value(wire_ids(b)).unwrap_or(Value::Null)),
            );
            content
        })
        .collect();

    let min = min_cacheable_tokens(&req.model);
    // Rough tokens (JSON bytes / 4, so a slight overcount: a prefix is only
    // skipped when clearly under the minimum).
    let est = |bytes: usize| bytes / 4;
    let history = req.cache.history;
    let system_text = if history {
        req.system.clone()
    } else {
        req.full_system()
    };

    let mut tool_values: Vec<Value> = Vec::new();
    let mut specs: Vec<&tern_core::types::ToolSpec> = req.tools.iter().collect();
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    for t in specs {
        tool_values.push(
            json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema }),
        );
    }
    let tools_bytes: usize = tool_values.iter().map(|t| t.to_string().len()).sum();
    let system_bytes = system_text.len();

    // Breakpoints on the history: the last block of messages[i], if the
    // prefix up to it reaches the minimum.
    let mut prefix_bytes = tools_bytes + system_bytes;
    let mut ends: Vec<usize> = Vec::with_capacity(messages.len());
    for content in &messages {
        prefix_bytes += content.iter().map(|b| b.to_string().len()).sum::<usize>();
        ends.push(prefix_bytes);
    }
    let mut marks: Vec<usize> = Vec::new();
    if history {
        let last = messages.len().wrapping_sub(1);
        let anchor = req.messages[..turn_start]
            .iter()
            .rposition(is_turn_start)
            .filter(|_| turn_start > 0);
        for i in anchor
            .into_iter()
            .chain((!messages.is_empty()).then_some(last))
        {
            if !messages[i].is_empty() && est(ends[i]) >= min && !marks.contains(&i) {
                marks.push(i);
            }
        }
    }
    for i in marks {
        if let Some(block) = messages[i].last_mut() {
            block["cache_control"] = cache_control(CacheTtl::FiveMinutes);
        }
    }
    // Per-run context after the last breakpoint, so it never touches the
    // cached prefix.
    let mut after: Vec<Value> = Vec::new();
    if history && !req.cache.volatile.is_empty() {
        after.push(json!({
            "type": "text",
            "text": format!(
                "Context for this run (from tern, not from the user):\n\n{}",
                req.cache.volatile
            ),
        }));
    }
    if !req.cache.tail.is_empty() {
        after.push(json!({ "type": "text", "text": req.cache.tail }));
    }
    if !after.is_empty() {
        match (messages.last_mut(), req.messages.last()) {
            (Some(content), Some(m)) if m.role == Role::User => content.extend(after),
            _ => messages.push(after),
        }
    }
    let roles: Vec<&str> = req
        .messages
        .iter()
        .map(|m| m.role.as_str())
        .chain(std::iter::repeat_n(
            "user",
            messages.len().saturating_sub(req.messages.len()),
        ))
        .collect();
    let messages: Vec<Value> = messages
        .into_iter()
        .zip(roles)
        .map(|(content, role)| json!({ "role": role, "content": content }))
        .collect();

    let mut body = json!({
        "model": req.model,
        "max_tokens": tern_core::limits::clamp_output(&req.model, req.max_tokens),
        "stream": true,
        "messages": messages,
    });
    if !system_text.is_empty() {
        let mut block = json!({ "type": "text", "text": system_text });
        if est(tools_bytes + system_bytes) >= min {
            block["cache_control"] = cache_control(ttl);
        }
        body["system"] = json!([block]);
    }
    if !tool_values.is_empty() {
        if est(tools_bytes) >= min
            && let Some(last) = tool_values.last_mut()
        {
            last["cache_control"] = cache_control(ttl);
        }
        body["tools"] = Value::Array(tool_values);
        if req.forbid_tool_calls {
            body["tool_choice"] = json!({ "type": "none" });
        }
    }
    body
}

/// A thinking block as Anthropic takes it back: exactly the text and
/// signature it sent (a changed block is rejected), without our own fields.
fn thinking_wire(b: &ContentBlock) -> Value {
    match b {
        ContentBlock::Thinking {
            thinking,
            signature,
            ..
        } => json!({ "type": "thinking", "thinking": thinking, "signature": signature }),
        ContentBlock::RedactedThinking { data } => {
            json!({ "type": "redacted_thinking", "data": data })
        }
        _ => Value::Null,
    }
}

/// Anthropic only takes tool-use ids of letters, digits, `_` and `-`. Ids
/// from another provider in a fallback chain (T6a) may hold more, e.g.
/// `functions.read_file:0`; both ends of a call map the same way, so they
/// still pair up.
fn wire_ids(b: &ContentBlock) -> ContentBlock {
    let clean = |id: &str| -> String {
        id.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    match b {
        ContentBlock::ToolUse { id, name, input } => ContentBlock::ToolUse {
            id: clean(id),
            name: name.clone(),
            input: input.clone(),
        },
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => ContentBlock::ToolResult {
            tool_use_id: clean(tool_use_id),
            content: content.clone(),
            is_error: *is_error,
        },
        other => other.clone(),
    }
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
                self.usage.cache_write_1h_tokens = u["cache_creation"]["ephemeral_1h_input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
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
                    Some("redacted_thinking") => {
                        vec![Ok(StreamEvent::ReasoningRedacted(
                            block["data"].as_str().unwrap_or_default().to_owned(),
                        ))]
                    }
                    Some("thinking") => match block["thinking"].as_str() {
                        Some(t) if !t.is_empty() => {
                            vec![Ok(StreamEvent::ReasoningDelta(t.to_owned()))]
                        }
                        _ => vec![],
                    },
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
                    Some("thinking_delta") => match d["thinking"].as_str() {
                        Some(t) if !t.is_empty() => {
                            vec![Ok(StreamEvent::ReasoningDelta(t.to_owned()))]
                        }
                        _ => vec![],
                    },
                    Some("signature_delta") => {
                        vec![Ok(StreamEvent::ReasoningSignature(
                            d["signature"].as_str().unwrap_or_default().to_owned(),
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

    fn fork_min_tokens(&self, model: &str) -> Option<u64> {
        Some(min_cacheable_tokens(model) as u64)
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
            .json(&request_body_with(&req, self.cache_ttl))
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
            forbid_tool_calls: false,
            cache: Default::default(),
        }
    }

    /// About `n` tokens of text.
    fn words(n: usize) -> String {
        "lorem ipsum dolor ".repeat(n * 4 / 18 + 1)
    }

    /// A request big enough to cache on any model: ~2k tokens of tools and
    /// ~2k of system prompt.
    fn big_req() -> ChatRequest {
        let mut r = req();
        r.system = words(2000);
        r.tools = vec![
            ToolSpec {
                name: "write_file".into(),
                description: words(1000),
                input_schema: json!({"type": "object", "properties": {"path": {}, "content": {}}}),
            },
            ToolSpec {
                name: "read_file".into(),
                description: words(1000),
                input_schema: json!({"type": "object"}),
            },
        ];
        r
    }

    fn tool_turn(i: usize) -> [Message; 2] {
        [
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: format!("t{i}"),
                    name: "read_file".into(),
                    input: json!({"path": format!("f{i}"), "a": 1}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: format!("t{i}"),
                    content: words(300),
                    is_error: false,
                }],
            },
        ]
    }

    /// Two finished turns, then a third with `calls` tool calls so far.
    fn history_req(calls: usize) -> ChatRequest {
        let mut r = big_req();
        r.cache.history = true;
        r.messages = vec![
            Message::user_text(format!("first {}", words(300))),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "first answer".into(),
                }],
            },
            Message::user_text(format!("second {}", words(300))),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "second answer".into(),
                }],
            },
            Message::user_text(format!("third {}", words(300))),
        ];
        for i in 0..calls {
            r.messages.extend(tool_turn(i));
        }
        r
    }

    /// Where `cache_control` sits: "tools", "system", or "messages[i]".
    fn marks(b: &Value) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(t) = b["tools"].as_array()
            && t.iter().any(|t| t.get("cache_control").is_some())
        {
            out.push("tools".to_owned());
        }
        if b["system"][0].get("cache_control").is_some() {
            out.push("system".to_owned());
        }
        for (i, m) in b["messages"].as_array().unwrap().iter().enumerate() {
            let blocks = m["content"].as_array().unwrap();
            for (j, blk) in blocks.iter().enumerate() {
                if blk.get("cache_control").is_some() {
                    out.push(format!("messages[{i}].{j}"));
                }
            }
        }
        out
    }

    fn strip_cache_control(v: &mut Value) {
        match v {
            Value::Object(m) => {
                m.remove("cache_control");
                m.values_mut().for_each(strip_cache_control);
            }
            Value::Array(a) => a.iter_mut().for_each(strip_cache_control),
            _ => {}
        }
    }

    #[test]
    fn body_has_cache_breakpoints() {
        let b = request_body(&big_req());
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["tools"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(marks(&b), ["tools", "system"]);
        assert_eq!(b["messages"][0]["content"][0]["type"], "text");
        assert_eq!(b["stream"], true);
        assert!(b.get("tool_choice").is_none());
    }

    #[test]
    fn nothing_is_marked_when_the_prefix_is_under_the_model_minimum() {
        // `req()`: a few tokens of system and one tiny tool.
        let mut r = req();
        r.cache.history = true;
        assert!(marks(&request_body(&r)).is_empty());
        // ~1500 tokens: enough for a 1024 model, not for a 4096 one.
        let mut r = history_req(0);
        r.system = words(1500);
        r.tools = vec![];
        r.messages.truncate(1);
        r.messages[0] = Message::user_text("hi");
        r.model = "claude-sonnet-4-5".into();
        assert_eq!(marks(&request_body(&r)), ["system", "messages[0].0"]);
        r.model = "claude-opus-4-5-20251101".into();
        assert!(marks(&request_body(&r)).is_empty());
        r.model = "claude-opus-5-5".into();
        assert_eq!(marks(&request_body(&r)), ["system", "messages[0].0"]);
    }

    #[test]
    fn minimum_lengths_by_model() {
        for (m, n) in [
            ("claude-opus-5-5", 512),
            ("claude-sonnet-5-5", 512),
            ("claude-haiku-5-5", 512),
            ("claude-fable-5-1", 512),
            ("claude-sonnet-5", 1024),
            ("claude-opus-4-8", 1024),
            ("claude-sonnet-4-5", 1024),
            ("claude-opus-4-7", 2048),
            ("claude-3-5-haiku-latest", 2048),
            ("claude-opus-4-6", 4096),
            ("claude-opus-4-5-20251101", 4096),
            ("claude-haiku-4-5", 4096),
            ("anthropic/claude-opus-4.5", 4096),
            ("some-new-model", 1024),
        ] {
            assert_eq!(min_cacheable_tokens(m), n, "{m}");
        }
    }

    #[test]
    fn history_gets_a_read_point_at_the_last_turn_and_a_rolling_one() {
        // Third turn, two calls in: [0..4] earlier turns, 4 = this turn's
        // user message, 5..8 the loop so far.
        let b = request_body(&history_req(2));
        assert_eq!(
            marks(&b),
            ["tools", "system", "messages[2].0", "messages[8].0"]
        );
        assert!(marks(&b).len() <= MAX_BREAKPOINTS);
        // The first call of a turn: the user message is the last message,
        // so the rolling one lands there, the read point on the turn before.
        let b = request_body(&history_req(0));
        assert_eq!(
            marks(&b),
            ["tools", "system", "messages[2].0", "messages[4].0"]
        );
        // The first turn of a session has no earlier one.
        let mut r = history_req(0);
        r.messages.truncate(1);
        assert_eq!(
            marks(&request_body(&r)),
            ["tools", "system", "messages[0].0"]
        );
    }

    #[test]
    fn one_off_calls_do_not_cache_their_input_as_history() {
        let mut r = history_req(2);
        r.cache.history = false;
        assert_eq!(marks(&request_body(&r)), ["tools", "system"]);
    }

    #[test]
    fn the_hour_lifetime_applies_to_tools_and_system_only() {
        let b = request_body_with(&history_req(1), CacheTtl::OneHour);
        assert_eq!(b["system"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(b["tools"][1]["cache_control"]["ttl"], "1h");
        // Longer lifetimes must precede shorter ones: messages stay at 5m.
        for m in b["messages"].as_array().unwrap() {
            for blk in m["content"].as_array().unwrap() {
                if let Some(c) = blk.get("cache_control") {
                    assert!(c.get("ttl").is_none(), "{c}");
                }
            }
        }
    }

    #[test]
    fn per_run_context_follows_the_last_breakpoint() {
        let mut r = history_req(1);
        r.cache.volatile = "## Current time\n\nMonday 09:00".into();
        let b = request_body(&r);
        let last = b["messages"].as_array().unwrap().last().unwrap();
        let blocks = last["content"].as_array().unwrap();
        // The tool result keeps the breakpoint; the context is the block after.
        assert!(blocks[0].get("cache_control").is_some());
        assert!(blocks[1]["text"].as_str().unwrap().contains("Monday 09:00"));
        assert!(blocks[1].get("cache_control").is_none());
        assert!(!b["system"].to_string().contains("Monday"));
        // Without history caching it stays in the system prompt, as before.
        r.cache.history = false;
        let b = request_body(&r);
        assert!(
            b["system"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Monday 09:00")
        );
    }

    #[test]
    fn serialization_is_byte_stable() {
        let a = history_req(2);
        assert_eq!(
            request_body(&a).to_string(),
            request_body(&a.clone()).to_string()
        );
        // Tool order and the key order of JSON the model wrote don't matter.
        let mut b = a.clone();
        b.tools.reverse();
        assert_eq!(request_body(&a).to_string(), request_body(&b).to_string());
        let mut c = a.clone();
        let mut input = serde_json::Map::new();
        input.insert("a".into(), json!(1));
        input.insert("path".into(), json!("f0"));
        if let ContentBlock::ToolUse { input: i, .. } = &mut c.messages[5].content[0] {
            *i = Value::Object(input);
        }
        assert_eq!(request_body(&a).to_string(), request_body(&c).to_string());
        // The run's time never reaches anything that is cached.
        let mut d = a.clone();
        d.cache.volatile = "## Current time\n\nMonday 09:00".into();
        let mut e = a;
        e.cache.volatile = "## Current time\n\nMonday 09:01".into();
        let (mut d, mut e) = (request_body(&d), request_body(&e));
        let tail = |v: &mut Value| {
            v["messages"].as_array_mut().unwrap().last_mut().unwrap()["content"]
                .as_array_mut()
                .unwrap()
                .pop()
        };
        assert_ne!(tail(&mut d), tail(&mut e));
        assert_eq!(d.to_string(), e.to_string());
    }

    #[test]
    fn each_turn_reproduces_the_previous_prefix() {
        // Turn by turn the conversation grows; what the last call sent must
        // reappear, byte for byte (markers aside), at the start of the next.
        let mut prev: Option<Value> = None;
        for calls in 0..4 {
            let mut r = history_req(calls);
            r.cache.volatile = format!("## Current time\n\n09:0{calls}");
            let mut body = request_body(&r);
            body["messages"].as_array_mut().unwrap().last_mut().unwrap()["content"]
                .as_array_mut()
                .unwrap()
                .pop();
            strip_cache_control(&mut body);
            if let Some(p) = &prev {
                assert_eq!(p["tools"], body["tools"]);
                assert_eq!(p["system"], body["system"]);
                let (pm, bm) = (
                    p["messages"].as_array().unwrap(),
                    body["messages"].as_array().unwrap(),
                );
                assert_eq!(&bm[..pm.len()], &pm[..], "turn {calls}");
            }
            prev = Some(body);
        }
    }

    #[test]
    fn a_forked_request_reproduces_the_parents_prefix_and_ends_with_the_instruction() {
        let mut parent = history_req(2);
        parent.cache.volatile = "## Current time\n\n09:00".into();
        // The next loop request: two more messages, then compaction forks it.
        let mut fork = history_req(3);
        fork.cache.volatile = parent.cache.volatile.clone();
        fork.cache.tail = tern_core::compaction::fork_instruction(4);
        assert!(!fork.forbid_tool_calls);

        let pb = request_body(&parent);
        let fb = request_body(&fork);
        // Not a byte of tool_choice: changing it would drop the cached history.
        assert!(fb.get("tool_choice").is_none());
        assert_eq!(pb["tools"], fb["tools"]);
        assert_eq!(pb["system"], fb["system"]);
        // The instruction is the last block, after the volatile context, and
        // carries no marker (nothing after it is ever read).
        let last = fb["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(last.last().unwrap()["text"], json!(fork.cache.tail));
        assert!(last.last().unwrap().get("cache_control").is_none());
        assert!(
            last[last.len() - 2]["text"]
                .as_str()
                .unwrap()
                .contains("09:00")
        );
        // Everything before it is the parent's request, byte for byte.
        let (mut pb, mut fb) = (pb, fb);
        for (b, n) in [(&mut pb, 1), (&mut fb, 2)] {
            let content = b["messages"].as_array_mut().unwrap().last_mut().unwrap()["content"]
                .as_array_mut()
                .unwrap();
            for _ in 0..n {
                content.pop();
            }
            strip_cache_control(b);
        }
        let (pm, fm) = (
            pb["messages"].as_array().unwrap(),
            fb["messages"].as_array().unwrap(),
        );
        assert_eq!(&fm[..pm.len()], &pm[..]);
    }

    #[test]
    fn usage_reports_hour_writes() {
        let ev = sse::SseEvent {
            event: Some("message_start".into()),
            data: r#"{"type":"message_start","message":{"usage":{"input_tokens":5,"cache_creation_input_tokens":900,"cache_read_input_tokens":100,"cache_creation":{"ephemeral_5m_input_tokens":300,"ephemeral_1h_input_tokens":600},"output_tokens":1}}}"#.into(),
        };
        let mut d = Decoder::default();
        d.decode(&ev);
        assert_eq!(
            (
                d.usage.cache_write_tokens,
                d.usage.cache_write_1h_tokens,
                d.usage.cache_read_tokens
            ),
            (900, 600, 100)
        );
    }

    /// Needs `ANTHROPIC_API_KEY`: two identical calls, the second must read
    /// what the first wrote.
    /// `cargo test -p tern-providers live_cache -- --ignored`
    #[tokio::test]
    #[ignore = "calls the live Anthropic API"]
    async fn live_cache_reads_on_the_second_call() {
        let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
        let p = Anthropic::new("anthropic", "https://api.anthropic.com", key).unwrap();
        // Unique per run so a warm cache from an earlier run can't pass it.
        let mut r = big_req();
        r.model = "claude-haiku-4-5".into();
        r.system = format!("{} {}", words(5000), std::process::id());
        r.max_tokens = 16;
        r.cache.history = true;
        let mut reads = Vec::new();
        for _ in 0..2 {
            let events: Vec<_> = p
                .stream(r.clone(), CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            let usage = events
                .into_iter()
                .filter_map(|e| match e.unwrap() {
                    StreamEvent::MessageStop { usage, .. } => Some(usage),
                    _ => None,
                })
                .next()
                .unwrap();
            reads.push((usage.cache_write_tokens, usage.cache_read_tokens));
        }
        assert!(reads[0].0 > 0, "first call wrote nothing: {reads:?}");
        assert!(reads[1].1 > 0, "second call read nothing: {reads:?}");
    }

    #[test]
    fn closing_turn_keeps_tools_but_forbids_calls() {
        let mut r = req();
        r.forbid_tool_calls = true;
        let b = request_body(&r);
        assert_eq!(b["tools"][0]["name"], "read_file");
        assert_eq!(b["tool_choice"]["type"], "none");
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

    #[test]
    fn foreign_tool_ids_are_made_valid_on_both_ends() {
        let mut r = req();
        r.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "functions.read_file:0".into(),
                name: "read_file".into(),
                input: json!({}),
            }],
        });
        r.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "functions.read_file:0".into(),
                content: "ok".into(),
                is_error: false,
            }],
        });
        let b = request_body(&r);
        assert_eq!(
            b["messages"][1]["content"][0]["id"],
            "functions_read_file_0"
        );
        assert_eq!(
            b["messages"][2]["content"][0]["tool_use_id"],
            "functions_read_file_0"
        );
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

    fn thinking_turn() -> Vec<Message> {
        let asst = |blocks: Vec<ContentBlock>| Message {
            role: Role::Assistant,
            content: blocks,
        };
        let think = |t: &str, sig: Option<&str>| ContentBlock::Thinking {
            thinking: t.into(),
            signature: sig.map(Into::into),
            duration_ms: Some(1200),
        };
        vec![
            Message::user_text("first question"),
            asst(vec![
                think("old reasoning", Some("sig-old")),
                ContentBlock::Text {
                    text: "first answer".into(),
                },
            ]),
            Message::user_text("second question"),
            asst(vec![
                think("signed reasoning", Some("sig-1")),
                ContentBlock::RedactedThinking { data: "enc".into() },
                think("unsigned reasoning", None),
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "read_file".into(),
                    input: json!({}),
                },
            ]),
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "ok".into(),
                    is_error: false,
                }],
            },
        ]
    }

    #[test]
    fn tool_use_turn_echoes_signed_thinking_exactly() {
        let mut r = req();
        r.messages = thinking_turn();
        let b = request_body(&r);
        let content = b["messages"][3]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "signed reasoning");
        assert_eq!(content[0]["signature"], "sig-1");
        // Only the wire fields go back.
        assert!(content[0].get("duration_ms").is_none());
        assert_eq!(content[1]["type"], "redacted_thinking");
        assert_eq!(content[1]["data"], "enc");
        // Unsigned reasoning (another provider's) would be rejected.
        assert_eq!(content[2]["type"], "tool_use");
        assert_eq!(content.len(), 3);
    }

    #[test]
    fn earlier_turns_do_not_carry_thinking() {
        let mut r = req();
        r.messages = thinking_turn();
        let b = request_body(&r);
        let first = b["messages"][1]["content"].as_array().unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["type"], "text");
    }

    #[test]
    fn thinking_is_not_echoed_when_the_turn_closed() {
        let mut r = req();
        r.messages = thinking_turn();
        // A new question starts a new turn: nothing before it is echoed.
        r.messages.push(Message::user_text("third question"));
        let b = request_body(&r);
        let s = b.to_string();
        assert!(!s.contains("signed reasoning") && !s.contains("redacted_thinking"));
    }

    const THINKING_STREAM: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"First, \"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"check.\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"EqQB\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi.\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":9}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    #[tokio::test]
    async fn streams_thinking_signature_and_redacted_blocks() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(THINKING_STREAM, "text/event-stream"),
            )
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
            &events[..5],
            [
                StreamEvent::ReasoningDelta("First, ".into()),
                StreamEvent::ReasoningDelta("check.".into()),
                StreamEvent::ReasoningSignature("EqQB".into()),
                StreamEvent::ReasoningRedacted("opaque".into()),
                StreamEvent::TextDelta("Hi.".into()),
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
