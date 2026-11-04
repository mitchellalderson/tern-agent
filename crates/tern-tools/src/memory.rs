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
    /// A private agent's save goes to the shared memories instead of its own.
    #[serde(default)]
    shared: bool,
}

/// The agent a run acts as and whether its memory is private (agent
/// profiles §1.3); the default agent with shared memory if unknown.
fn scope(store: &Store, ctx: &ToolCtx) -> (String, bool) {
    store
        .run_agent(&ctx.run_id)
        .unwrap_or_else(|_| (tern_store::DEFAULT_AGENT.into(), false))
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
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "shared": { "type": "boolean", "description": "an agent with private memory: save it where every agent can find it (facts about the user, not your own notes)" }
                },
                "required": ["content"]
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::granted_write(false)
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: SaveArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        if a.content.trim().is_empty() {
            return ToolResult::error("content is empty");
        }
        let (agent, private) = scope(&self.store, ctx);
        let own = (private && !a.shared).then_some(agent.as_str());
        match self.store.save_agent_memory(a.content.trim(), &a.tags, own) {
            Ok(id) if own.is_some() => ToolResult::ok(format!("saved memory #{id} (yours)")),
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
    /// To find a run's agent: a private agent's `memory` is its own file.
    store: Option<Store>,
}

impl ProfileEdit {
    pub fn new(home: TernHome, caps: MemoryConfig) -> Self {
        Self {
            home,
            caps,
            store: None,
        }
    }

    pub fn with_store(self, store: Store) -> Self {
        Self {
            store: Some(store),
            ..self
        }
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

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
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
        // `user.md` is always shared; `memory.md` is the agent's own when its
        // memory is private (agent profiles §1.3).
        let path = match (&self.store, file) {
            (Some(store), ProfileFile::Memory) => {
                let (agent, private) = scope(store, ctx);
                tern_core::agents::memory_path(&self.home, &agent, private)
            }
            _ => file.path(&self.home),
        };
        if let Some(dir) = path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            return ToolResult::error(format!("creating {}: {e}", dir.display()));
        }
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

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: SearchArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let limit = a.limit.unwrap_or(10).clamp(1, MAX_LIMIT);
        let (agent, private) = scope(&self.store, ctx);
        let own = private.then_some(agent.as_str());
        match self.store.search_agent_memories(&a.query, limit, own) {
            Ok(hits) if hits.is_empty() => ToolResult::ok("no matching memories"),
            Ok(hits) => ToolResult::ok(
                hits.iter()
                    .map(|m| {
                        let tags = if m.tags.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", m.tags)
                        };
                        let own = if m.agent_id.is_some() { ", yours" } else { "" };
                        format!(
                            "#{} ({}{own}){tags}: {}",
                            m.id,
                            date(m.created_at),
                            m.content
                        )
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
        // Only this agent's conversations, never another agent's DM.
        let agent = self
            .store
            .session_agent(&ctx.session_id)
            .unwrap_or_else(|_| tern_store::DEFAULT_AGENT.into());
        match self
            .store
            .search_agent_messages(&a.query, limit, Some(&ctx.session_id), &agent)
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

    #[tokio::test]
    async fn a_private_agent_keeps_its_own_memories_and_memory_md() {
        let tmp = tempfile::tempdir().unwrap();
        let home = TernHome::new(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let (agent, dm) = store
            .create_agent(&tern_store::AgentRow {
                id: "tern:research".into(),
                slug: "research".into(),
                name: "Research".into(),
                memory: "private".into(),
                ..Default::default()
            })
            .unwrap();
        store
            .create_thread(&tern_store::NewThread {
                id: "dm",
                source: "api",
                channel_id: &dm,
                ..Default::default()
            })
            .unwrap();
        store.create_session("home", "api", None).unwrap();
        let msg = tern_store::NewMessage {
            role: "user".into(),
            content_json: "[]".into(),
            text: "hi".into(),
            summary_of: None,
        };
        for (run, session, scope) in [("rr", "dm", Some("private")), ("rt", "home", None)] {
            store
                .create_run(
                    &tern_store::NewRun {
                        id: run,
                        session_id: session,
                        trigger: "user",
                        model: "m:m",
                        system_prompt: "",
                        memory_scope: scope,
                        ..Default::default()
                    },
                    &msg,
                )
                .unwrap();
        }
        let ctx = |run: &str, session: &str| ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: run.into(),
            session_id: session.into(),
            cancel: CancellationToken::new(),
        };
        let (research, tern) = (ctx("rr", "dm"), ctx("rt", "home"));
        let save = MemorySave::new(store.clone());
        let r = save
            .call(&research, json!({"content": "acme uses postgres"}))
            .await;
        assert!(r.content.ends_with("(yours)"), "{}", r.content);
        save.call(
            &research,
            json!({"content": "acme user prefers email", "shared": true}),
        )
        .await;
        save.call(&tern, json!({"content": "acme invoices are monthly"}))
            .await;
        let search = MemorySearch::new(store.clone());
        let seen = search
            .call(&research, json!({"query": "acme"}))
            .await
            .content;
        assert_eq!(seen.lines().count(), 3, "{seen}");
        assert!(seen.contains("yours"));
        let seen = search.call(&tern, json!({"query": "acme"})).await.content;
        assert_eq!(seen.lines().count(), 2, "{seen}");
        assert!(!seen.contains("postgres"));

        // memory.md is Research's own; user.md stays shared.
        let edit =
            ProfileEdit::new(home.clone(), MemoryConfig::default()).with_store(store.clone());
        let r = edit
            .call(
                &research,
                json!({"file": "memory", "mode": "append", "content": "Research note"}),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        let own = tern_core::agents::memory_path(&home, &agent.id, true);
        assert!(
            std::fs::read_to_string(own)
                .unwrap()
                .contains("Research note")
        );
        assert!(!home.memory_path().exists());
        edit.call(
            &research,
            json!({"file": "user", "mode": "append", "content": "Name: Sam"}),
        )
        .await;
        assert!(
            std::fs::read_to_string(home.user_path())
                .unwrap()
                .contains("Sam")
        );

        // session_search never reaches another agent's DM.
        store
            .create_run(
                &tern_store::NewRun {
                    id: "rr2",
                    session_id: "dm",
                    trigger: "user",
                    model: "m:m",
                    system_prompt: "",
                    ..Default::default()
                },
                &tern_store::NewMessage {
                    text: "the zebra project".into(),
                    ..msg.clone()
                },
            )
            .unwrap();
        let ss = SessionSearch::new(store.clone());
        let hit = ss.call(&tern, json!({"query": "zebra"})).await.content;
        assert_eq!(hit, "no matching messages");
    }
}
