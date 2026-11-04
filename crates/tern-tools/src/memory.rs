//! Long-term memory and past-conversation search (spec §9.2, §9.3).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::TernHome;
use tern_core::memory::{MemoryConfig, ProfileFile};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult};
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
    fn effect(&self) -> Effect {
        Effect::Write
    }

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
        ToolPolicy::granted_write(false)
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

// ---- profile_edit ----

/// Reads and writes the capped files injected into every prompt, `user.md`
/// and `memory.md` (spec §7, T5a). A write over a file's cap is refused
/// with a request to consolidate, never truncated.
pub struct ProfileEdit {
    home: TernHome,
    caps: MemoryConfig,
}

impl ProfileEdit {
    pub fn new(home: TernHome, caps: MemoryConfig) -> Self {
        Self { home, caps }
    }
}

#[derive(Deserialize)]
struct ProfileArgs {
    file: String,
    mode: String,
    #[serde(default)]
    content: String,
}

#[async_trait]
impl Tool for ProfileEdit {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "profile_edit",
            &format!(
                "Read or change the notes that are in your system prompt in every conversation: \
                 `user` (user.md, about the user, capped at {} tokens) or `memory` (memory.md, core \
                 facts, capped at {} tokens). mode \"read\" returns the file; \"append\" adds a line \
                 or a few; \"replace\" rewrites the whole file (to consolidate it). Keep only what every \
                 conversation needs; save other facts with `memory_save`. A write over the cap is refused.",
                self.caps.user_md_tokens, self.caps.memory_md_tokens
            ),
            json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "enum": ["user", "memory"] },
                    "mode": { "type": "string", "enum": ["read", "append", "replace"] },
                    "content": { "type": "string" }
                },
                "required": ["file", "mode"]
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, raw: &Value) -> ToolPolicy {
        match raw.get("mode").and_then(Value::as_str) {
            Some("read") => ToolPolicy::new(Effect::Read, true),
            Some("append") => ToolPolicy::granted_write(false),
            _ => ToolPolicy::granted_write(true),
        }
    }

    async fn call(&self, _: &ToolCtx, raw: Value) -> ToolResult {
        let a: ProfileArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let Some(file) = ProfileFile::parse(&a.file) else {
            return ToolResult::error(format!(
                "unknown file {:?}: use \"user\" or \"memory\"",
                a.file
            ));
        };
        let path = file.path(&self.home);
        let current = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return ToolResult::error(format!("reading {}: {e}", file.file_name())),
        };
        let next = match a.mode.as_str() {
            "read" => {
                let text = tern_core::memory::injectable(&current);
                let size = tern_core::memory::tokens(&text);
                let cap = file.cap(&self.caps);
                return ToolResult::ok(if text.is_empty() {
                    format!("{} is empty (cap {cap} tokens)", file.file_name())
                } else {
                    format!(
                        "{} (about {size} of {cap} tokens):\n\n{text}",
                        file.file_name()
                    )
                });
            }
            "append" if a.content.trim().is_empty() => {
                return ToolResult::error("content is empty");
            }
            "append" => {
                let mut t = current.trim_end().to_owned();
                if !t.is_empty() {
                    t.push('\n');
                }
                t.push_str(a.content.trim());
                t.push('\n');
                t
            }
            "replace" => format!("{}\n", a.content.trim()),
            other => {
                return ToolResult::error(format!(
                    "unknown mode {other:?}: use \"read\", \"append\" or \"replace\""
                ));
            }
        };
        if let Some(err) = tern_core::memory::over_cap_error(file, &self.caps, &next, &current) {
            return ToolResult::error(err);
        }
        let tmp = path.with_extension("md.tmp");
        if let Err(e) = std::fs::write(&tmp, &next).and_then(|()| std::fs::rename(&tmp, &path)) {
            let _ = std::fs::remove_file(&tmp);
            return ToolResult::error(format!("writing {}: {e}", file.file_name()));
        }
        let size = tern_core::memory::tokens(&tern_core::memory::injectable(&next));
        ToolResult::ok(format!(
            "saved {} (about {size} of {} tokens); it is in the prompt from the next conversation",
            file.file_name(),
            file.cap(&self.caps)
        ))
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
    fn effect(&self) -> Effect {
        Effect::Read
    }

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
        ToolPolicy::new(Effect::Read, true)
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
    fn effect(&self) -> Effect {
        Effect::Read
    }

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
        ToolPolicy::new(Effect::Read, true)
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

    #[tokio::test]
    async fn profile_edit_appends_replaces_and_refuses_over_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let home = TernHome::new(tmp.path());
        let caps = MemoryConfig {
            memory_md_tokens: 20,
            ..MemoryConfig::default()
        };
        let tool = ProfileEdit::new(home.clone(), caps);
        let edit =
            |mode: &str, content: &str| json!({"file": "memory", "mode": mode, "content": content});

        let r = tool
            .call(&ctx(), json!({"file": "memory", "mode": "read"}))
            .await;
        assert_eq!(r.content, "memory.md is empty (cap 20 tokens)");
        assert!(
            !tool
                .call(&ctx(), edit("append", "Sam is the user's son"))
                .await
                .is_error
        );
        assert!(
            !tool
                .call(&ctx(), edit("append", "Prefers metric"))
                .await
                .is_error
        );
        assert_eq!(
            std::fs::read_to_string(home.memory_path()).unwrap(),
            "Sam is the user's son\nPrefers metric\n"
        );

        let over = tool.call(&ctx(), edit("append", &"x".repeat(60))).await;
        assert!(over.is_error);
        assert!(
            over.content.contains("over its cap of 20"),
            "{}",
            over.content
        );
        assert!(
            over.content.contains("Current memory.md:\n\nSam is"),
            "{}",
            over.content
        );
        assert_eq!(
            std::fs::read_to_string(home.memory_path()).unwrap(),
            "Sam is the user's son\nPrefers metric\n",
            "a refused write changes nothing"
        );

        assert!(
            !tool
                .call(&ctx(), edit("replace", "Son: Sam. Metric."))
                .await
                .is_error
        );
        let r = tool
            .call(&ctx(), json!({"file": "memory", "mode": "read"}))
            .await;
        assert!(r.content.ends_with("Son: Sam. Metric."), "{}", r.content);
        assert!(tool.call(&ctx(), edit("delete", "")).await.is_error);
        assert!(
            tool.call(&ctx(), json!({"file": "soul", "mode": "read"}))
                .await
                .is_error
        );

        assert!(tool.policy(&ctx(), &json!({"mode": "read"})).effect == Effect::Read);
        assert!(!tool.policy(&ctx(), &json!({"mode": "append"})).idempotent);
    }
}
