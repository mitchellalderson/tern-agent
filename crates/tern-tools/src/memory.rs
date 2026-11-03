//! Long-term memory and past-conversation search (spec §9.2, §9.3).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::tool::{Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;
use tern_store::Store;

use crate::{args, spec};

const MAX_LIMIT: usize = 50;

fn date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

// ---- memory_save ----

pub struct MemorySave {
    store: Store,
}

impl MemorySave {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct SaveArgs {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[async_trait]
impl Tool for MemorySave {
    fn spec(&self) -> ToolSpec {
        spec(
            "memory_save",
            "Save a durable fact to long-term memory (preferences, people, ongoing projects). Write it so it makes sense on its own later.",
            json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["content"]
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy {
            idempotent: false,
            requires_approval: false,
        }
    }

    async fn call(&self, _: &ToolCtx, raw: Value) -> ToolResult {
        let a: SaveArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        if a.content.trim().is_empty() {
            return ToolResult::error("content is empty");
        }
        match self.store.save_memory(a.content.trim(), &a.tags) {
            Ok(id) => ToolResult::ok(format!("saved memory #{id}")),
            Err(e) => ToolResult::error(format!("saving memory: {e}")),
        }
    }
}

// ---- memory_search ----

pub struct MemorySearch {
    store: Store,
}

impl MemorySearch {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    limit: Option<usize>,
}

#[async_trait]
impl Tool for MemorySearch {
    fn spec(&self) -> ToolSpec {
        spec(
            "memory_search",
            "Search long-term memory by keywords. Results are ranked by relevance.",
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
        ToolPolicy {
            idempotent: true,
            requires_approval: false,
        }
    }

    async fn call(&self, _: &ToolCtx, raw: Value) -> ToolResult {
        let a: SearchArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let limit = a.limit.unwrap_or(10).clamp(1, MAX_LIMIT);
        match self.store.search_memories(&a.query, limit) {
            Ok(hits) if hits.is_empty() => ToolResult::ok("no matching memories"),
            Ok(hits) => ToolResult::ok(
                hits.iter()
                    .map(|m| {
                        let tags = if m.tags.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", m.tags)
                        };
                        format!("#{} ({}){tags}: {}", m.id, date(m.created_at), m.content)
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(e) => ToolResult::error(format!("searching memory: {e}")),
        }
    }
}

// ---- session_search ----

pub struct SessionSearch {
    store: Store,
}

impl SessionSearch {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for SessionSearch {
    fn spec(&self) -> ToolSpec {
        spec(
            "session_search",
            "Full-text search over messages from past conversations (not this one). Returns snippets with session id and date.",
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
        ToolPolicy {
            idempotent: true,
            requires_approval: false,
        }
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: SearchArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let limit = a.limit.unwrap_or(10).clamp(1, MAX_LIMIT);
        match self
            .store
            .search_messages(&a.query, limit, Some(&ctx.session_id))
        {
            Ok(hits) if hits.is_empty() => ToolResult::ok("no matching messages"),
            Ok(hits) => ToolResult::ok(
                hits.iter()
                    .map(|h| {
                        let title = h.session_title.as_deref().unwrap_or("untitled");
                        format!(
                            "session {} \"{title}\" ({}, {}): {}",
                            h.session_id,
                            date(h.created_at),
                            h.role,
                            h.snippet.replace('\n', " ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(e) => ToolResult::error(format!("searching sessions: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> ToolCtx {
        ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn save_then_search() {
        let store = Store::open_in_memory().unwrap();
        let save = MemorySave::new(store.clone());
        let r = save
            .call(
                &ctx(),
                json!({"content": "Sam's birthday is March 3", "tags": ["people"]}),
            )
            .await;
        assert_eq!(r.content, "saved memory #1");
        assert!(save.call(&ctx(), json!({"content": "  "})).await.is_error);

        let search = MemorySearch::new(store);
        let hit = search
            .call(&ctx(), json!({"query": "when is Sam's birthday"}))
            .await;
        assert!(
            hit.content.contains("#1") && hit.content.contains("[people]"),
            "{}",
            hit.content
        );
        let miss = search.call(&ctx(), json!({"query": "zebra"})).await;
        assert_eq!(miss.content, "no matching memories");
    }
}
