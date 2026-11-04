//! `channel_read` and `channel_post` (docs/channels-design.md §3): agents
//! reading and writing a channel's posts. They only touch tern's own store.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::channels::author;
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult, current_call_id};
use tern_core::types::ToolSpec;
use tern_store::{ChannelRow, NewPost, PostRow, Store};

use crate::{args, spec};

/// Posts one run may write.
pub const MAX_POSTS_PER_RUN: u32 = 10;
const MAX_READ: usize = 50;

/// Told about each post a run writes (the daemon announces it).
pub type OnPost = Arc<dyn Fn(PostRow) + Send + Sync>;

/// The channel named `slug` (or id), else the one the run's session is in.
fn channel_for(store: &Store, ctx: &ToolCtx, slug: Option<&str>) -> Result<ChannelRow, String> {
    let found = match slug.map(|s| s.trim().trim_start_matches('#')) {
        Some(s) if !s.is_empty() => store.channel(s),
        _ => store.session_channel(&ctx.session_id),
    }
    .map_err(|e| e.to_string())?;
    found.ok_or_else(|| match slug {
        Some(s) => format!("no channel {s:?}"),
        None => "this conversation isn't in a channel".into(),
    })
}

fn when(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub struct ChannelRead {
    store: Store,
}

impl ChannelRead {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    channel: Option<String>,
    before: Option<i64>,
    limit: Option<usize>,
    query: Option<String>,
}

#[async_trait]
impl Tool for ChannelRead {
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "channel_read",
            "Read a channel's posts: what the user, you, other agents and routines said there. Defaults to this conversation's channel, newest page; `before` (a post id) pages back; `query` searches instead.",
            json!({
                "type": "object",
                "properties": {
                    "channel": { "type": "string", "description": "slug, e.g. finances; default: this conversation's channel" },
                    "before": { "type": "integer" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_READ },
                    "query": { "type": "string" }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: ReadArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let channel = match channel_for(&self.store, ctx, a.channel.as_deref()) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(e),
        };
        let limit = a.limit.unwrap_or(20).clamp(1, MAX_READ);
        let rows = match a.query.as_deref().filter(|q| !q.trim().is_empty()) {
            Some(q) => self.store.search_posts(&channel.id, q, a.before, limit),
            None => self.store.posts(&channel.id, a.before, limit).map(|mut r| {
                r.reverse();
                r
            }),
        };
        let rows = match rows {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("reading posts: {e}")),
        };
        if rows.is_empty() {
            return ToolResult::ok(format!("no posts in #{}", channel.slug));
        }
        let text = rows
            .iter()
            .map(|p| {
                let kind = match p.author_kind.as_str() {
                    "routine" => " (routine)",
                    _ => "",
                };
                let reply = p
                    .reply_to
                    .map(|r| format!(", reply to {r}"))
                    .unwrap_or_default();
                format!(
                    "post {} · {}{kind} · {}{reply}{}: {}",
                    p.id,
                    p.author,
                    when(p.created_at),
                    if p.tainted {
                        " · from a tainted run"
                    } else {
                        ""
                    },
                    p.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        // A post a tainted run wrote is third-party text to the reader.
        ToolResult {
            untrusted: rows.iter().any(|p| p.tainted),
            ..ToolResult::ok(text)
        }
    }
}

pub struct ChannelPost {
    store: Store,
    on_post: Option<OnPost>,
}

impl ChannelPost {
    pub fn new(store: Store, on_post: Option<OnPost>) -> Self {
        Self { store, on_post }
    }
}

#[derive(Deserialize)]
struct PostArgs {
    text: String,
    #[serde(default)]
    needs_you: bool,
    channel: Option<String>,
}

#[async_trait]
impl Tool for ChannelPost {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "channel_post",
            "Post an update to a channel's timeline for the user and other agents: progress on long work, or something that needs the user (`needs_you`). Defaults to this conversation's channel; `channel` posts elsewhere (\"put this in #finances\"). Your final answer is posted for you, so don't repeat it. At most 10 per run.",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "needs_you": { "type": "boolean" },
                    "channel": { "type": "string", "description": "slug; default: this conversation's channel" }
                },
                "required": ["text"]
            }),
        )
    }

    /// Granted: it only writes tern's own store, and a tainted run's post
    /// carries the taint on (`approval::TAINT_CONTAINED`).
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::granted_write(true)
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: PostArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let text = a.text.trim();
        if text.is_empty() {
            return ToolResult::error("text is empty");
        }
        let run = match self.store.run(&ctx.run_id) {
            Ok(Some(r)) => r,
            Ok(None) => return ToolResult::error("run not found"),
            Err(e) => return ToolResult::error(e.to_string()),
        };
        if run.parent_run_id.is_some() {
            return ToolResult::error(
                "sub-agents can't post to channels yet; report to your parent instead",
            );
        }
        let channel = match channel_for(&self.store, ctx, a.channel.as_deref()) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(e),
        };
        if channel.archived_at.is_some() {
            return ToolResult::error(format!("#{} is archived", channel.slug));
        }
        let call = current_call_id().unwrap_or_else(tern_core::agent::new_id);
        let key = format!("post:{}:{call}", run.id);
        let already = self.store.post_by_key(&key).ok().flatten();
        if already.is_none() {
            match self.store.run_post_count(&run.id) {
                Ok(n) if n >= MAX_POSTS_PER_RUN => {
                    return ToolResult::error(format!(
                        "this run has posted {n} times, the most it may"
                    ));
                }
                Err(e) => return ToolResult::error(e.to_string()),
                _ => {}
            }
        }
        let written = self.store.add_post(&NewPost {
            channel_id: &channel.id,
            session_id: Some(&ctx.session_id),
            run_id: Some(&run.id),
            author_kind: "agent",
            author_id: author::AGENT,
            author: author::AGENT,
            kind: if a.needs_you { "needs_you" } else { "update" },
            text,
            dedupe_key: Some(&key),
            tainted: run.taint.is_some(),
            ..NewPost::default()
        });
        match written {
            Ok((row, created)) => {
                let id = row.id;
                if created && let Some(f) = &self.on_post {
                    f(row);
                }
                let place = if channel.kind == "main" {
                    channel.title.clone()
                } else {
                    format!("#{}", channel.slug)
                };
                ToolResult::ok(format!("posted to {place} (post {id})"))
            }
            Err(e) => ToolResult::error(format!("posting: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_store::{ChannelRow, NewMessage, NewRun, NewThread};
    use tokio_util::sync::CancellationToken;

    fn setup(taint: Option<&str>) -> (Store, ToolCtx) {
        let store = Store::open_in_memory().unwrap();
        store
            .create_channel(&ChannelRow {
                id: "c".into(),
                slug: "finances".into(),
                title: "Finances".into(),
                kind: "topic".into(),
                created_by: "user".into(),
                ..ChannelRow::default()
            })
            .unwrap();
        store
            .create_thread(&NewThread {
                id: "s",
                source: "api",
                channel_id: "c",
                ..NewThread::default()
            })
            .unwrap();
        store
            .create_run(
                &NewRun {
                    id: "r",
                    session_id: "s",
                    trigger: "user",
                    model: "mock:m",
                    system_prompt: "",
                    ..NewRun::default()
                },
                &NewMessage {
                    role: "user".into(),
                    content_json: "[]".into(),
                    text: "go".into(),
                    summary_of: None,
                },
            )
            .unwrap();
        if let Some(t) = taint {
            store.set_run_taint("r", t).unwrap();
        }
        let ctx = ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        (store, ctx)
    }

    #[tokio::test]
    async fn posts_from_a_tainted_run_are_tainted_and_read_back_untrusted() {
        let (store, ctx) = setup(Some("result of http_fetch"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let post = ChannelPost::new(
            store.clone(),
            Some(Arc::new(move |p: PostRow| sink.lock().unwrap().push(p.id))),
        );
        let r = post
            .call(&ctx, json!({ "text": "Halfway: 40 sources" }))
            .await;
        assert!(!r.is_error, "{}", r.content);
        let p = store.posts("c", None, 5).unwrap().remove(0);
        assert!(p.tainted);
        assert_eq!((p.kind.as_str(), p.author_id.as_str()), ("update", "tern"));
        assert_eq!(seen.lock().unwrap().len(), 1);

        let read = ChannelRead::new(store.clone()).call(&ctx, json!({})).await;
        assert!(read.untrusted, "a tainted post is third-party text");
        assert!(
            read.content.contains("from a tainted run"),
            "{}",
            read.content
        );
        let found = ChannelRead::new(store)
            .call(&ctx, json!({ "channel": "#finances", "query": "sources" }))
            .await;
        assert!(found.content.contains("Halfway"), "{}", found.content);
    }

    #[tokio::test]
    async fn clean_posts_read_clean_and_runs_are_capped() {
        let (store, ctx) = setup(None);
        let post = ChannelPost::new(store.clone(), None);
        for i in 0..MAX_POSTS_PER_RUN {
            let r = tern_core::tool::with_call(
                &format!("c{i}"),
                post.call(
                    &ctx,
                    json!({ "text": format!("update {i}"), "needs_you": i == 0 }),
                ),
            )
            .await
            .0;
            assert!(!r.is_error, "{}", r.content);
        }
        // The same call again (a replayed step) finds its post.
        let again = tern_core::tool::with_call("c3", post.call(&ctx, json!({ "text": "x" })))
            .await
            .0;
        assert!(!again.is_error, "{}", again.content);
        let over = tern_core::tool::with_call("c99", post.call(&ctx, json!({ "text": "x" })))
            .await
            .0;
        assert!(over.is_error, "{}", over.content);
        let read = ChannelRead::new(store.clone())
            .call(&ctx, json!({ "limit": 3 }))
            .await;
        assert!(!read.untrusted);
        assert!(read.content.starts_with("post 8 "), "{}", read.content);
        assert_eq!(
            store.posts("c", None, 50).unwrap().last().unwrap().kind,
            "needs_you"
        );
        let missing = ChannelRead::new(store)
            .call(&ctx, json!({ "channel": "nope" }))
            .await;
        assert!(missing.is_error);
    }
}
