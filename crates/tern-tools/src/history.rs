//! `history_search`: details from earlier segments of the stream the run is
//! in (docs/channels-design.md §2.6). A new segment starts with a summary
//! of the last one; this finds what the summary dropped.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;
use tern_store::{HOME, Store};

use crate::{args, spec};

const MAX_LIMIT: usize = 20;

pub struct HistorySearch {
    store: Store,
}

impl HistorySearch {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct Args {
    query: String,
    limit: Option<usize>,
}

fn when(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%a %Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

#[async_trait]
impl Tool for HistorySearch {
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "history_search",
            "Full-text search over the earlier sessions of this conversation (your chat with the user before the latest summary, and older conversations here), not this session. Use it when the summary you were given leaves out a detail.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT }
                },
                "required": ["query"]
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: Args = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let limit = a.limit.unwrap_or(8).clamp(1, MAX_LIMIT);
        let channel = match self.store.session(&ctx.session_id) {
            Ok(s) => s.and_then(|s| s.channel_id).unwrap_or_else(|| HOME.into()),
            Err(e) => return ToolResult::error(format!("searching history: {e}")),
        };
        match self
            .store
            .search_channel_messages(&channel, &a.query, limit, Some(&ctx.session_id))
        {
            Ok(hits) if hits.is_empty() => ToolResult::ok("nothing earlier matches"),
            Ok(hits) => ToolResult::ok(
                hits.iter()
                    .map(|h| {
                        format!(
                            "{} {} (session {}): {}",
                            when(h.created_at),
                            if h.role == "user" { "user" } else { "you" },
                            h.session_id,
                            h.snippet.replace('\n', " ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(e) => ToolResult::error(format!("searching history: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_store::{NewMessage, NewRun, NewThread};
    use tokio_util::sync::CancellationToken;

    fn ctx(session: &str) -> ToolCtx {
        ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: "r".into(),
            session_id: session.into(),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn finds_earlier_segments_but_not_this_one() {
        let store = Store::open_in_memory().unwrap();
        for (id, prev, text) in [
            ("seg1", None, "The Northwind invoice is 310 EUR"),
            ("seg2", Some("seg1"), "What about Northwind again?"),
        ] {
            store
                .create_thread(&NewThread {
                    id,
                    source: "api",
                    channel_id: HOME,
                    prev_session_id: prev,
                    ..NewThread::default()
                })
                .unwrap();
            store
                .create_run(
                    &NewRun {
                        id: &format!("r-{id}"),
                        session_id: id,
                        trigger: "user",
                        model: "mock:m",
                        system_prompt: "",
                        ..NewRun::default()
                    },
                    &NewMessage {
                        role: "user".into(),
                        content_json: "[]".into(),
                        text: text.into(),
                        summary_of: None,
                    },
                )
                .unwrap();
        }
        let tool = HistorySearch::new(store);
        let r = tool
            .call(&ctx("seg2"), json!({ "query": "northwind" }))
            .await;
        assert!(!r.is_error);
        assert!(
            r.content.contains("session seg1") && r.content.contains("[Northwind]"),
            "{}",
            r.content
        );
        assert!(!r.content.contains("seg2"), "{}", r.content);
        let none = tool.call(&ctx("seg2"), json!({ "query": "zebra" })).await;
        assert_eq!(none.content, "nothing earlier matches");
    }
}
