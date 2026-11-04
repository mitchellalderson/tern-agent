//! Channels, their threads, and posts (docs/channels-design.md §2).

use rusqlite::{OptionalExtension, Row, params};

use crate::repo::{SESSION_COLS, session_row};
use crate::{MessageHit, Result, SessionRow, Store, fts_query, now_ms};

/// Home's id (and slug). Created by the migration; never archived.
pub const HOME: &str = "home";

/// Most characters of a post a Home pointer repeats.
pub const POINTER_CHARS: usize = 280;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelRow {
    pub id: String,
    pub slug: String,
    pub title: String,
    /// `main` (Home) or `topic`.
    pub kind: String,
    pub instructions: Option<String>,
    /// The main thread; for Home, the newest segment of its stream.
    pub main_session_id: Option<String>,
    pub created_by: String,
    pub archived_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// A main channel: whose DM it is (`None` = the default agent). A topic
    /// channel: its lead.
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PostRow {
    pub id: i64,
    pub channel_id: String,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    /// `user`, `agent`, `subagent` or `routine`.
    pub author_kind: String,
    /// Stable: `owner`, `tern`, `tern/<label>`, `routine:<schedule id>`.
    pub author_id: String,
    /// Shown: `you`, `tern`, a routine's title.
    pub author: String,
    /// `message`, `answer`, `update`, `needs_you` or `pointer`.
    pub kind: String,
    pub text: String,
    pub reply_to: Option<i64>,
    pub points_to: Option<i64>,
    pub tainted: bool,
    pub dedupe_key: Option<String>,
    pub created_at: i64,
    /// A `needs_you` post or a pointer the user marked done.
    pub resolved_at: Option<i64>,
    /// The agents it @mentions (agent profiles §5.2), by id.
    pub mentions: Vec<String>,
    /// How far down a wake chain it is (§5.3): the user's posts are 0, a
    /// post by a run a depth-d post woke is d + 1.
    pub wake_depth: i64,
}

/// Values for a new post.
#[derive(Debug, Clone, Default)]
pub struct NewPost<'a> {
    pub channel_id: &'a str,
    pub session_id: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub author_kind: &'a str,
    pub author_id: &'a str,
    pub author: &'a str,
    pub kind: &'a str,
    pub text: &'a str,
    pub reply_to: Option<i64>,
    pub points_to: Option<i64>,
    /// Written by a tainted run (tooling spec §5.3): a reader treats it as
    /// untrusted.
    pub tainted: bool,
    /// Makes the write idempotent: a second post with the same key returns
    /// the first.
    pub dedupe_key: Option<&'a str>,
    /// When it happened, if not now (a marker placed where a moved
    /// conversation was).
    pub created_at: Option<i64>,
}

/// What [`Store::move_thread`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// The channel it left.
    pub from: String,
    /// That was a main channel (Home, later an agent's DM).
    pub from_main: bool,
    /// It was that channel's main thread.
    pub was_main: bool,
    /// The empty segment that took its place in a stream.
    pub fresh: Option<String>,
}

fn self_posts_seen(tx: &rusqlite::Transaction<'_>, session: &str) -> rusqlite::Result<i64> {
    tx.query_row(
        "SELECT posts_seen FROM sessions WHERE id = ?1",
        [session],
        |r| r.get(0),
    )
}

/// Values for a new thread (a session in a channel).
#[derive(Debug, Clone, Default)]
pub struct NewThread<'a> {
    pub id: &'a str,
    pub source: &'a str,
    pub channel_id: &'a str,
    pub model: Option<&'a str>,
    /// Gateway chats: the chat id.
    pub external_ref: Option<&'a str>,
    pub prev_session_id: Option<&'a str>,
    pub segment_reason: Option<&'a str>,
    pub branched_from: Option<&'a str>,
    /// Whose thread it is; default: the channel's (a DM's owner, a topic
    /// channel's lead).
    pub agent_id: Option<&'a str>,
}

/// Leaves out of a digest the posts that woke a run in session `?2`: the
/// run was started with their text.
const NOT_WAKER: &str =
    "AND id NOT IN (SELECT woken_by FROM runs WHERE session_id = ?2 AND woken_by IS NOT NULL)";

const CHANNEL_COLS: &str = "id, slug, title, kind, instructions, main_session_id, created_by, \
    archived_at, created_at, updated_at, agent_id";
const POST_COLS: &str = "id, channel_id, session_id, run_id, author_kind, author_id, author, kind, \
    text, reply_to, points_to, tainted, dedupe_key, created_at, resolved_at, mentions, wake_depth";

fn channel_row(r: &Row<'_>) -> rusqlite::Result<ChannelRow> {
    Ok(ChannelRow {
        id: r.get(0)?,
        slug: r.get(1)?,
        title: r.get(2)?,
        kind: r.get(3)?,
        instructions: r.get(4)?,
        main_session_id: r.get(5)?,
        created_by: r.get(6)?,
        archived_at: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        agent_id: r.get(10)?,
    })
}

fn post_row(r: &Row<'_>) -> rusqlite::Result<PostRow> {
    Ok(PostRow {
        id: r.get(0)?,
        channel_id: r.get(1)?,
        session_id: r.get(2)?,
        run_id: r.get(3)?,
        author_kind: r.get(4)?,
        author_id: r.get(5)?,
        author: r.get(6)?,
        kind: r.get(7)?,
        text: r.get(8)?,
        reply_to: r.get(9)?,
        points_to: r.get(10)?,
        tainted: r.get::<_, i64>(11)? != 0,
        dedupe_key: r.get(12)?,
        created_at: r.get(13)?,
        resolved_at: r.get(14)?,
        mentions: r
            .get::<_, Option<String>>(15)?
            .map(|m| crate::members_repo::ids_from_json(&m))
            .unwrap_or_default(),
        wake_depth: r.get(16)?,
    })
}

impl Store {
    // ---- channels ----

    pub fn create_channel(&self, c: &ChannelRow) -> Result<ChannelRow> {
        let now = now_ms();
        self.conn()?.execute(
            "INSERT INTO channels (id, slug, title, kind, instructions, created_by, created_at, updated_at,
               agent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
            params![c.id, c.slug, c.title, c.kind, c.instructions, c.created_by, now, c.agent_id],
        )?;
        Ok(ChannelRow {
            created_at: now,
            updated_at: now,
            main_session_id: None,
            archived_at: None,
            ..c.clone()
        })
    }

    /// Channels, Home first, then by title; archived ones only if asked.
    pub fn channels(&self, include_archived: bool) -> Result<Vec<ChannelRow>> {
        let sql = format!(
            "SELECT {CHANNEL_COLS} FROM channels WHERE ?1 OR archived_at IS NULL
             ORDER BY kind = 'main' DESC, title COLLATE NOCASE"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([include_archived], channel_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A channel by id or slug.
    pub fn channel(&self, id_or_slug: &str) -> Result<Option<ChannelRow>> {
        let sql = format!("SELECT {CHANNEL_COLS} FROM channels WHERE id = ?1 OR slug = ?1");
        Ok(self
            .conn()?
            .query_row(&sql, [id_or_slug], channel_row)
            .optional()?)
    }

    /// Change a channel's title, instructions (`Some(None)` clears them) or
    /// archived state. False if there is no such channel.
    pub fn update_channel(
        &self,
        id: &str,
        title: Option<&str>,
        instructions: Option<Option<&str>>,
        archived: Option<bool>,
    ) -> Result<bool> {
        let now = now_ms();
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE channels SET updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        if n == 0 {
            return Ok(false);
        }
        if let Some(t) = title {
            conn.execute(
                "UPDATE channels SET title = ?2 WHERE id = ?1",
                params![id, t],
            )?;
        }
        if let Some(i) = instructions {
            conn.execute(
                "UPDATE channels SET instructions = ?2 WHERE id = ?1",
                params![id, i],
            )?;
        }
        if let Some(a) = archived {
            conn.execute(
                "UPDATE channels SET archived_at = ?2 WHERE id = ?1",
                params![id, a.then_some(now)],
            )?;
        }
        Ok(true)
    }

    pub fn set_main_session(&self, channel_id: &str, session_id: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE channels SET main_session_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![channel_id, session_id, now_ms()],
        )?;
        Ok(())
    }

    /// The channel a session is a thread of.
    pub fn session_channel(&self, session_id: &str) -> Result<Option<ChannelRow>> {
        let sql = format!(
            "SELECT {} FROM channels c JOIN sessions s ON s.channel_id = c.id WHERE s.id = ?1",
            CHANNEL_COLS
                .split(", ")
                .map(|c| format!("c.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(self
            .conn()?
            .query_row(&sql, [session_id], channel_row)
            .optional()?)
    }

    // ---- threads ----

    pub fn create_thread(&self, t: &NewThread<'_>) -> Result<SessionRow> {
        let now = now_ms();
        self.conn()?.execute(
            "INSERT INTO sessions (id, source, model, external_ref, channel_id, prev_session_id,
               segment_reason, branched_from, created_at, updated_at, agent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9,
               NULLIF(COALESCE(?10, (SELECT agent_id FROM channels WHERE id = ?5)), 'tern'))",
            params![
                t.id,
                t.source,
                t.model,
                t.external_ref,
                t.channel_id,
                t.prev_session_id,
                t.segment_reason,
                t.branched_from,
                now,
                t.agent_id,
            ],
        )?;
        if let Some(prev) = t.prev_session_id {
            // A new segment has seen what the one before it saw.
            self.conn()?.execute(
                "UPDATE sessions SET posts_seen = (SELECT posts_seen FROM sessions WHERE id = ?2)
                 WHERE id = ?1",
                params![t.id, prev],
            )?;
        }
        Ok(SessionRow {
            id: t.id.into(),
            source: t.source.into(),
            external_ref: t.external_ref.map(Into::into),
            model: t.model.map(Into::into),
            created_at: now,
            updated_at: now,
            channel_id: Some(t.channel_id.into()),
            prev_session_id: t.prev_session_id.map(Into::into),
            segment_reason: t.segment_reason.map(Into::into),
            branched_from: t.branched_from.map(Into::into),
            title: None,
            agent_id: match t.agent_id {
                Some(a) => Some(a.to_owned()),
                None => self.channel(t.channel_id)?.and_then(|c| c.agent_id),
            }
            .filter(|a| a != crate::DEFAULT_AGENT),
            moved_from: None,
        })
    }

    /// Threads of a channel, newest first, without sub-agent and routine
    /// sessions (those are private to their parent or schedule).
    pub fn channel_sessions(&self, channel_id: &str, limit: usize) -> Result<Vec<SessionRow>> {
        let sql = format!(
            "SELECT {SESSION_COLS} FROM sessions
             WHERE channel_id = ?1 AND source NOT IN ('subagent', 'cron')
             ORDER BY updated_at DESC, rowid DESC LIMIT ?2"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![channel_id, limit as i64], session_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `(messages, runs)` in a session.
    pub fn session_counts(&self, session_id: &str) -> Result<(i64, i64)> {
        Ok(self.conn()?.query_row(
            "SELECT (SELECT count(*) FROM messages WHERE session_id = ?1),
                    (SELECT count(*) FROM runs WHERE session_id = ?1)",
            [session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// When the session last had a message (its input or a step's), or
    /// when it was created. Posts from elsewhere don't count.
    pub fn session_last_activity(&self, session_id: &str) -> Result<i64> {
        Ok(self.conn()?.query_row(
            "SELECT COALESCE((SELECT max(created_at) FROM messages WHERE session_id = ?1),
                             (SELECT created_at FROM sessions WHERE id = ?1), 0)",
            [session_id],
            |r| r.get(0),
        )?)
    }

    /// The session's messages with ids below `before` (all if `None`),
    /// newest `limit` of them, oldest first.
    pub fn session_messages_page(
        &self,
        session_id: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<crate::MessageRow>> {
        let all = self.session_messages(session_id)?;
        let kept: Vec<_> = all
            .into_iter()
            .filter(|m| before.is_none_or(|b| m.id < b))
            .collect();
        let skip = kept.len().saturating_sub(limit);
        Ok(kept.into_iter().skip(skip).collect())
    }

    /// Full-text search over the messages of `channel_id`'s threads (not
    /// sub-agents' or routines'), except `exclude_session`.
    pub fn search_channel_messages(
        &self,
        channel_id: &str,
        query: &str,
        limit: usize,
        exclude_session: Option<&str>,
    ) -> Result<Vec<MessageHit>> {
        let Some(q) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, s.title, m.role, m.created_at,
                    snippet(messages_fts, 0, '[', ']', ' … ', 24)
             FROM messages_fts f
             JOIN messages m ON m.id = f.rowid
             JOIN sessions s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1 AND s.channel_id = ?2
               AND s.source NOT IN ('subagent', 'cron') AND m.session_id IS NOT ?4
             ORDER BY bm25(messages_fts) LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![q, channel_id, limit as i64, exclude_session], |r| {
            Ok(MessageHit {
                message_id: r.get(0)?,
                session_id: r.get(1)?,
                session_title: r.get(2)?,
                role: r.get(3)?,
                created_at: r.get(4)?,
                snippet: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- posts ----

    /// Write a post. With a `dedupe_key` already used, nothing is written
    /// and the earlier post comes back with `false`.
    ///
    /// A new `needs_you` post outside Home also gets a `pointer` in Home
    /// (`points_to` it, key `pointer:<id>`) in the same transaction, so
    /// whatever writes one (`channel_post`, a sub-agent, later the outbox)
    /// puts it where the user acts (docs/channels-design.md §2.3).
    pub fn add_post(&self, p: &NewPost<'_>) -> Result<(PostRow, bool)> {
        // Who it @mentions, and (an agent's) how far down a wake chain it is.
        let mentions = self.mentioned_agents(p.text)?;
        let mentions = (!mentions.is_empty())
            .then(|| crate::members_repo::ids_to_json(mentions.iter().map(|a| a.id.as_str())));
        let depth = match p.run_id {
            Some(run) if p.author_kind != "user" => self.run_wake_depth(run)?,
            _ => 0,
        };
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let insert = |tx: &rusqlite::Transaction<'_>, p: &NewPost<'_>| -> rusqlite::Result<usize> {
            tx.execute(
                "INSERT INTO posts (channel_id, session_id, run_id, author_kind, author_id, author, kind,
                   text, reply_to, points_to, dedupe_key, created_at, tainted, mentions, wake_depth)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT (dedupe_key) DO NOTHING",
                params![
                    p.channel_id,
                    p.session_id,
                    p.run_id,
                    p.author_kind,
                    p.author_id,
                    p.author,
                    p.kind,
                    p.text,
                    p.reply_to,
                    p.points_to,
                    p.dedupe_key,
                    p.created_at.unwrap_or(now),
                    p.tainted,
                    mentions,
                    depth,
                ],
            )
        };
        let n = insert(&tx, p)?;
        let row = if n > 0 {
            let sql = format!("SELECT {POST_COLS} FROM posts WHERE id = ?1");
            tx.query_row(&sql, [tx.last_insert_rowid()], post_row)?
        } else {
            let sql = format!("SELECT {POST_COLS} FROM posts WHERE dedupe_key = ?1");
            tx.query_row(&sql, [p.dedupe_key], post_row)?
        };
        if n > 0 {
            tx.execute(
                "UPDATE channels SET updated_at = ?2 WHERE id = ?1",
                params![p.channel_id, now],
            )?;
            if p.kind == "needs_you" && p.channel_id != HOME {
                let text: String = p.text.chars().take(POINTER_CHARS).collect();
                let key = format!("pointer:{}", row.id);
                insert(
                    &tx,
                    &NewPost {
                        channel_id: HOME,
                        session_id: None,
                        run_id: p.run_id,
                        kind: "pointer",
                        text: &text,
                        reply_to: None,
                        points_to: Some(row.id),
                        dedupe_key: Some(&key),
                        ..p.clone()
                    },
                )?;
            }
        }
        tx.commit()?;
        Ok((row, n > 0))
    }

    /// The pointer in Home to post `id`, if it has one.
    pub fn pointer_to(&self, id: i64) -> Result<Option<PostRow>> {
        self.post_by_key(&format!("pointer:{id}"))
    }

    /// Move thread `session_id` into channel `target` (a topic channel),
    /// in one transaction:
    /// - a stream it was a segment of closes around it: its successor
    ///   continues its predecessor. Its own `prev_session_id` stays, so the
    ///   journaled carry step of its first run still replays;
    /// - if it was its channel's main thread (Home: the current segment),
    ///   a main channel gets a fresh empty segment `fresh_id` after its
    ///   predecessor, a topic channel none (the next send makes one); a
    ///   gateway chat's current segment is replaced the same way, and the
    ///   moved thread no longer belongs to the chat;
    /// - it and its sub-agents' sessions join `target`, and its digest
    ///   cursor starts at the newest post, so it isn't shown the channel's
    ///   past as news.
    pub fn move_thread(&self, session_id: &str, target: &str, fresh_id: &str) -> Result<Moved> {
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let sql = format!("SELECT {SESSION_COLS} FROM sessions WHERE id = ?1");
        let s = tx.query_row(&sql, [session_id], session_row)?;
        let from = s.channel_id.clone().unwrap_or_else(|| HOME.into());
        let (kind, main): (String, Option<String>) = tx.query_row(
            "SELECT kind, main_session_id FROM channels WHERE id = ?1",
            [&from],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute(
            "UPDATE sessions SET prev_session_id = ?2
             WHERE prev_session_id = ?1 AND channel_id = ?3 AND id <> ?1",
            params![s.id, s.prev_session_id, from],
        )?;
        let chat_head = match &s.external_ref {
            Some(r) => {
                let latest: Option<String> = tx
                    .query_row(
                        "SELECT id FROM sessions WHERE source = ?1 AND external_ref = ?2
                         ORDER BY updated_at DESC, rowid DESC LIMIT 1",
                        params![s.source, r],
                        |row| row.get(0),
                    )
                    .optional()?;
                latest.as_deref() == Some(s.id.as_str())
            }
            None => false,
        };
        let was_main = main.as_deref() == Some(s.id.as_str());
        let mut fresh = None;
        if (was_main && kind == "main") || chat_head {
            tx.execute(
                "INSERT INTO sessions (id, source, model, external_ref, channel_id, prev_session_id,
                   segment_reason, posts_seen, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'moved', ?7, ?8, ?8)",
                params![
                    fresh_id,
                    s.source,
                    s.model,
                    s.external_ref,
                    from,
                    s.prev_session_id,
                    self_posts_seen(&tx, &s.id)?,
                    now
                ],
            )?;
            fresh = Some(fresh_id.to_owned());
        }
        if was_main {
            let next = if kind == "main" { fresh.clone() } else { None };
            tx.execute(
                "UPDATE channels SET main_session_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![from, next, now],
            )?;
        }
        tx.execute(
            "UPDATE sessions SET channel_id = ?2, external_ref = NULL, moved_from = ?3, moved_at = ?4,
               posts_seen = (SELECT COALESCE(max(id), 0) FROM posts)
             WHERE id = ?1",
            params![s.id, target, from, now],
        )?;
        // Its sub-agents' sessions, any depth, follow it.
        tx.execute(
            "WITH RECURSIVE tree(id) AS (
               SELECT id FROM runs WHERE session_id = ?1
               UNION SELECT r.id FROM runs r JOIN tree t ON r.parent_run_id = t.id
             )
             UPDATE sessions SET channel_id = ?2
             WHERE id IN (SELECT session_id FROM runs WHERE id IN tree) AND id <> ?1",
            params![s.id, target],
        )?;
        tx.execute(
            "UPDATE channels SET updated_at = ?2 WHERE id = ?1",
            params![target, now],
        )?;
        tx.commit()?;
        Ok(Moved {
            from,
            from_main: kind == "main",
            was_main,
            fresh,
        })
    }

    /// Why some run in the session is tainted, if one is.
    pub fn session_taint(&self, session_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT taint FROM runs WHERE session_id = ?1 AND taint IS NOT NULL
                 ORDER BY started_at LIMIT 1",
                [session_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Mark a `needs_you` post or a pointer done, with its counterpart.
    /// False if there's no such open item.
    pub fn resolve_post(&self, id: i64) -> Result<bool> {
        let now = now_ms();
        let Some(p) = self.post(id)? else {
            return Ok(false);
        };
        if !matches!(p.kind.as_str(), "needs_you" | "pointer") || p.resolved_at.is_some() {
            return Ok(false);
        }
        let conn = self.conn()?;
        conn.execute(
            "UPDATE posts SET resolved_at = ?2
             WHERE resolved_at IS NULL AND (id = ?1 OR id = ?3 OR points_to = ?1)",
            params![id, now, p.points_to],
        )?;
        Ok(true)
    }

    /// Open `needs_you` posts and pointers in a channel.
    pub fn open_needs_you(&self, channel_id: &str) -> Result<usize> {
        let n: i64 = self.conn()?.query_row(
            "SELECT count(*) FROM posts
             WHERE channel_id = ?1 AND kind IN ('needs_you', 'pointer') AND resolved_at IS NULL",
            [channel_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Whether a sub-agent run may post (spawned with `"post": true`).
    pub fn run_can_post(&self, run_id: &str) -> Result<bool> {
        Ok(self
            .conn()?
            .query_row("SELECT can_post FROM runs WHERE id = ?1", [run_id], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
            .is_some_and(|v| v != 0))
    }

    pub fn set_run_can_post(&self, run_id: &str) -> Result<()> {
        self.conn()?
            .execute("UPDATE runs SET can_post = 1 WHERE id = ?1", [run_id])?;
        Ok(())
    }

    /// Rewrite a post the daemon keeps current (a delegation's pointer).
    pub fn update_post_text(&self, id: i64, text: &str, tainted: bool) -> Result<()> {
        self.conn()?.execute(
            "UPDATE posts SET text = ?2, tainted = ?3 WHERE id = ?1",
            params![id, text, tainted],
        )?;
        Ok(())
    }

    /// The side thread of `channel` branched from `branched_from`
    /// (`post:<id>`), newest first.
    pub fn thread_branched_from(
        &self,
        channel_id: &str,
        branched_from: &str,
    ) -> Result<Option<SessionRow>> {
        let sql = format!(
            "SELECT {SESSION_COLS} FROM sessions WHERE channel_id = ?1 AND branched_from = ?2
             ORDER BY created_at DESC LIMIT 1"
        );
        Ok(self
            .conn()?
            .query_row(&sql, params![channel_id, branched_from], session_row)
            .optional()?)
    }

    pub fn post(&self, id: i64) -> Result<Option<PostRow>> {
        let sql = format!("SELECT {POST_COLS} FROM posts WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], post_row).optional()?)
    }

    pub fn post_by_key(&self, key: &str) -> Result<Option<PostRow>> {
        let sql = format!("SELECT {POST_COLS} FROM posts WHERE dedupe_key = ?1");
        Ok(self.conn()?.query_row(&sql, [key], post_row).optional()?)
    }

    /// A page of a channel's posts, newest first: those with ids below
    /// `before` (all if `None`).
    pub fn posts(
        &self,
        channel_id: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<PostRow>> {
        let sql = format!(
            "SELECT {POST_COLS} FROM posts WHERE channel_id = ?1 AND id < ?2
             ORDER BY id DESC LIMIT ?3"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![channel_id, before.unwrap_or(i64::MAX), limit as i64],
            post_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Posts of a channel created in `[from, to)` (unix ms), oldest first.
    pub fn posts_between(&self, channel_id: &str, from: i64, to: i64) -> Result<Vec<PostRow>> {
        let sql = format!(
            "SELECT {POST_COLS} FROM posts WHERE channel_id = ?1 AND created_at >= ?2 AND created_at < ?3
             ORDER BY id"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![channel_id, from, to], post_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn last_post_id(&self, channel_id: &str) -> Result<Option<i64>> {
        Ok(self.conn()?.query_row(
            "SELECT max(id) FROM posts WHERE channel_id = ?1",
            [channel_id],
            |r| r.get(0),
        )?)
    }

    /// Mark mail `ids` consumed by `run_id` without a step: messages the
    /// user queued for a run that finished before reading them, handed to
    /// the run started (or found working) for them.
    pub fn hand_over_mail(&self, ids: &[i64], run_id: &str) -> Result<()> {
        let conn = self.conn()?;
        for id in ids {
            conn.execute(
                "UPDATE agent_mail SET consumed_run_id = ?2, consumed_step = -1
                 WHERE id = ?1 AND consumed_step IS NULL",
                params![id, run_id],
            )?;
        }
        Ok(())
    }

    /// The newest post this session's agent has been shown (§3).
    pub fn posts_seen(&self, session_id: &str) -> Result<i64> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT posts_seen FROM sessions WHERE id = ?1",
                [session_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// Move the session's digest cursor forward to `post` (never back).
    pub fn advance_posts_seen(&self, session_id: &str, post: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE sessions SET posts_seen = max(posts_seen, ?2) WHERE id = ?1",
            params![session_id, post],
        )?;
        Ok(())
    }

    /// Posts of `channel_id` after `after` that came from elsewhere than
    /// `session_id`: the newest `limit`, newest first, and how many there
    /// are in all.
    pub fn unseen_posts(
        &self,
        channel_id: &str,
        session_id: &str,
        after: i64,
        limit: usize,
    ) -> Result<(Vec<PostRow>, usize)> {
        let conn = self.conn()?;
        let total: i64 = conn.query_row(
            &format!("SELECT count(*) FROM posts WHERE channel_id = ?1 AND id > ?3 AND session_id IS NOT ?2 {NOT_WAKER}"),
            params![channel_id, session_id, after],
            |r| r.get(0),
        )?;
        let sql = format!(
            "SELECT {POST_COLS} FROM posts WHERE channel_id = ?1 AND id > ?3 AND session_id IS NOT ?2
             {NOT_WAKER} ORDER BY id DESC LIMIT ?4"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![channel_id, session_id, after, limit as i64],
            post_row,
        )?;
        Ok((rows.collect::<rusqlite::Result<_>>()?, total as usize))
    }

    /// Full-text search over a channel's posts, best first, among those
    /// with ids below `before`.
    pub fn search_posts(
        &self,
        channel_id: &str,
        query: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<PostRow>> {
        let Some(q) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let cols = POST_COLS
            .split(", ")
            .map(|c| format!("p.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {cols} FROM posts_fts f JOIN posts p ON p.id = f.rowid
             WHERE posts_fts MATCH ?1 AND p.channel_id = ?2 AND p.id < ?3
             ORDER BY bm25(posts_fts) LIMIT ?4"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![q, channel_id, before.unwrap_or(i64::MAX), limit as i64],
            post_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Posts a run wrote with `channel_post` (keys `post:<run>:…`).
    pub fn run_post_count(&self, run_id: &str) -> Result<u32> {
        Ok(self.conn()?.query_row(
            "SELECT count(*) FROM posts WHERE run_id = ?1 AND dedupe_key LIKE 'post:%'",
            [run_id],
            |r| r.get(0),
        )?)
    }

    pub fn set_schedule_channel(&self, schedule_id: &str, channel_id: &str) -> Result<bool> {
        Ok(self.conn()?.execute(
            "UPDATE schedules SET channel_id = ?2 WHERE id = ?1",
            params![schedule_id, channel_id],
        )? > 0)
    }

    /// Top-level runs (and sub-agents that post) that finished at or after `since` (unix ms), for
    /// writing answer posts a crash may have skipped.
    pub fn top_runs_finished_since(&self, since: i64) -> Result<Vec<crate::RunRow>> {
        let sql = format!(
            "SELECT {} FROM runs WHERE (parent_run_id IS NULL OR can_post = 1) AND finished_at >= ?1
             ORDER BY finished_at",
            crate::repo::RUN_COLS
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([since], crate::repo::run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewMessage, NewRun};

    fn topic(s: &Store, slug: &str) -> ChannelRow {
        s.create_channel(&ChannelRow {
            id: format!("c-{slug}"),
            slug: slug.into(),
            title: slug.into(),
            kind: "topic".into(),
            created_by: "user".into(),
            ..ChannelRow::default()
        })
        .unwrap()
    }

    fn post<'a>(channel: &'a str, text: &'a str, key: Option<&'a str>) -> NewPost<'a> {
        NewPost {
            channel_id: channel,
            author_kind: "user",
            author_id: "owner",
            author: "you",
            kind: "message",
            text,
            dedupe_key: key,
            ..NewPost::default()
        }
    }

    #[test]
    fn home_exists_and_new_sessions_are_in_it() {
        let s = Store::open_in_memory().unwrap();
        let home = s.channel(HOME).unwrap().unwrap();
        assert_eq!((home.slug.as_str(), home.kind.as_str()), ("home", "main"));
        let sess = s.create_session("a", "api", None).unwrap();
        assert_eq!(sess.channel_id.as_deref(), Some(HOME));
        assert_eq!(
            s.session("a").unwrap().unwrap().channel_id.as_deref(),
            Some(HOME)
        );
    }

    #[test]
    fn channels_list_home_first_and_hide_archived() {
        let s = Store::open_in_memory().unwrap();
        let b = topic(&s, "b");
        topic(&s, "a");
        let slugs = |all| -> Vec<String> {
            s.channels(all)
                .unwrap()
                .into_iter()
                .map(|c| c.slug)
                .collect()
        };
        assert_eq!(slugs(false), ["home", "a", "b"]);
        assert!(
            s.update_channel(&b.id, Some("Bee"), Some(Some("be brief")), Some(true))
                .unwrap()
        );
        assert_eq!(slugs(false), ["home", "a"]);
        assert_eq!(slugs(true), ["home", "a", "b"]);
        let b = s.channel("b").unwrap().unwrap();
        assert_eq!(
            (b.title.as_str(), b.instructions.as_deref()),
            ("Bee", Some("be brief"))
        );
        assert!(b.archived_at.is_some());
        assert!(!s.update_channel("nope", None, None, None).unwrap());
    }

    #[test]
    fn posts_dedupe_and_page_newest_first() {
        let s = Store::open_in_memory().unwrap();
        let c = topic(&s, "fin");
        let (first, created) = s.add_post(&post(&c.id, "one", Some("k1"))).unwrap();
        assert!(created);
        let (again, created) = s.add_post(&post(&c.id, "different", Some("k1"))).unwrap();
        assert!(!created);
        assert_eq!((again.id, again.text.as_str()), (first.id, "one"));
        for i in 0..4 {
            s.add_post(&post(&c.id, &format!("p{i}"), None)).unwrap();
        }
        let page = s.posts(&c.id, None, 2).unwrap();
        assert_eq!(
            page.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(),
            ["p3", "p2"]
        );
        let older = s.posts(&c.id, Some(page[1].id), 10).unwrap();
        assert_eq!(older.len(), 3);
        assert_eq!(s.last_post_id(&c.id).unwrap(), Some(page[0].id));
        assert_eq!(s.last_post_id(HOME).unwrap(), None);
        let fts: i64 = s
            .conn()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM posts_fts WHERE posts_fts MATCH 'p2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts, 1);
    }

    #[test]
    fn needs_you_outside_home_points_from_home_until_resolved() {
        let s = Store::open_in_memory().unwrap();
        let c = topic(&s, "fin");
        let ask = |key: &'static str| NewPost {
            kind: "needs_you",
            author_kind: "agent",
            author_id: "tern",
            author: "tern",
            ..post(&c.id, "Approve paying Acme?", Some(key))
        };
        let (p, _) = s.add_post(&ask("k1")).unwrap();
        // Again (a replay): still one pointer.
        s.add_post(&ask("k1")).unwrap();
        let ptr = s.pointer_to(p.id).unwrap().unwrap();
        assert_eq!(
            (
                ptr.channel_id.as_str(),
                ptr.kind.as_str(),
                ptr.points_to,
                ptr.author_id.as_str()
            ),
            (HOME, "pointer", Some(p.id), "tern")
        );
        assert_eq!(s.posts(HOME, None, 10).unwrap().len(), 1);
        assert_eq!(
            (
                s.open_needs_you(&c.id).unwrap(),
                s.open_needs_you(HOME).unwrap()
            ),
            (1, 1)
        );
        // Resolving the pointer closes the post too.
        assert!(s.resolve_post(ptr.id).unwrap());
        assert!(!s.resolve_post(ptr.id).unwrap());
        assert_eq!(
            (
                s.open_needs_you(&c.id).unwrap(),
                s.open_needs_you(HOME).unwrap()
            ),
            (0, 0)
        );
        assert!(s.post(p.id).unwrap().unwrap().resolved_at.is_some());
        // Ordinary posts and Home's own needs_you get no pointer.
        let (plain, _) = s.add_post(&post(&c.id, "fyi", None)).unwrap();
        assert!(s.pointer_to(plain.id).unwrap().is_none());
        assert!(!s.resolve_post(plain.id).unwrap());
        let (home, _) = s
            .add_post(&NewPost {
                kind: "needs_you",
                ..post(HOME, "in Home already", None)
            })
            .unwrap();
        assert!(s.pointer_to(home.id).unwrap().is_none());
    }

    fn seg(s: &Store, id: &'static str, prev: Option<&'static str>) {
        s.create_thread(&NewThread {
            id,
            source: "api",
            channel_id: HOME,
            prev_session_id: prev,
            segment_reason: prev.map(|_| "idle"),
            ..NewThread::default()
        })
        .unwrap();
    }

    fn prev(s: &Store, id: &str) -> Option<String> {
        s.session(id).unwrap().unwrap().prev_session_id
    }

    #[test]
    fn moving_a_middle_segment_closes_the_stream_around_it() {
        let s = Store::open_in_memory().unwrap();
        let c = topic(&s, "launch");
        seg(&s, "a", None);
        seg(&s, "b", Some("a"));
        seg(&s, "c", Some("b"));
        s.set_main_session(HOME, "c").unwrap();
        let m = s.move_thread("b", &c.id, "fresh").unwrap();
        assert_eq!(
            (m.from.as_str(), m.from_main, m.was_main, m.fresh),
            (HOME, true, false, None)
        );
        assert_eq!(prev(&s, "c").as_deref(), Some("a"), "c continues a");
        // b keeps its own link, so its journaled carry step replays.
        assert_eq!(prev(&s, "b").as_deref(), Some("a"));
        let b = s.session("b").unwrap().unwrap();
        assert_eq!(
            (b.channel_id, b.moved_from),
            (Some(c.id.clone()), Some(HOME.into()))
        );
        assert_eq!(
            s.channel(HOME).unwrap().unwrap().main_session_id.as_deref(),
            Some("c")
        );
    }

    #[test]
    fn moving_the_current_segment_starts_a_fresh_one_after_its_predecessor() {
        let s = Store::open_in_memory().unwrap();
        let c = topic(&s, "launch");
        seg(&s, "a", None);
        seg(&s, "b", Some("a"));
        s.set_main_session(HOME, "b").unwrap();
        s.advance_posts_seen("b", 0).unwrap();
        let m = s.move_thread("b", &c.id, "fresh").unwrap();
        assert_eq!((m.was_main, m.fresh.as_deref()), (true, Some("fresh")));
        let fresh = s.session("fresh").unwrap().unwrap();
        assert_eq!(
            (
                fresh.prev_session_id.as_deref(),
                fresh.segment_reason.as_deref(),
                fresh.channel_id.as_deref()
            ),
            (Some("a"), Some("moved"), Some(HOME))
        );
        assert_eq!(
            s.channel(HOME).unwrap().unwrap().main_session_id.as_deref(),
            Some("fresh")
        );

        // A topic channel's main thread leaves no fresh one: the next send
        // makes it.
        let other = topic(&s, "ops");
        s.create_thread(&NewThread {
            id: "t",
            source: "api",
            channel_id: &other.id,
            ..NewThread::default()
        })
        .unwrap();
        s.set_main_session(&other.id, "t").unwrap();
        let m = s.move_thread("t", &c.id, "unused").unwrap();
        assert_eq!((m.from_main, m.was_main, m.fresh), (false, true, None));
        assert_eq!(s.channel(&other.id).unwrap().unwrap().main_session_id, None);
    }

    #[test]
    fn a_chats_current_segment_is_replaced_and_its_sub_agents_follow() {
        let s = Store::open_in_memory().unwrap();
        let c = topic(&s, "launch");
        s.create_thread(&NewThread {
            id: "tg",
            source: "telegram",
            channel_id: HOME,
            external_ref: Some("555"),
            ..NewThread::default()
        })
        .unwrap();
        let msg = NewMessage {
            role: "user".into(),
            content_json: "[]".into(),
            text: "hi".into(),
            summary_of: None,
        };
        let run = |id: &'static str, session: &'static str, parent: Option<&'static str>| NewRun {
            id,
            session_id: session,
            trigger: "user",
            model: "mock:m",
            system_prompt: "",
            parent_run_id: parent,
            ..NewRun::default()
        };
        s.create_run(&run("r", "tg", None), &msg).unwrap();
        s.create_session("child", "subagent", None).unwrap();
        s.create_run(&run("rc", "child", Some("r")), &msg).unwrap();
        let m = s.move_thread("tg", &c.id, "tg2").unwrap();
        assert_eq!(m.fresh.as_deref(), Some("tg2"));
        let now = s.latest_session_for("telegram", "555").unwrap().unwrap();
        assert_eq!(now.id, "tg2", "the chat goes on in a fresh segment");
        assert_eq!(s.session("tg").unwrap().unwrap().external_ref, None);
        assert_eq!(s.session("child").unwrap().unwrap().channel_id, Some(c.id));
    }

    #[test]
    fn a_sub_agent_session_takes_its_parent_channel() {
        let s = Store::open_in_memory().unwrap();
        let c = topic(&s, "launch");
        s.create_thread(&NewThread {
            id: "main",
            source: "api",
            channel_id: &c.id,
            ..NewThread::default()
        })
        .unwrap();
        let msg = NewMessage {
            role: "user".into(),
            content_json: "[]".into(),
            text: "hi".into(),
            summary_of: None,
        };
        let run = |id: &'static str, session: &'static str, parent: Option<&'static str>| NewRun {
            id,
            session_id: session,
            trigger: "user",
            model: "mock:m",
            system_prompt: "",
            parent_run_id: parent,
            ..NewRun::default()
        };
        s.create_run(&run("r", "main", None), &msg).unwrap();
        s.create_session("child", "subagent", None).unwrap();
        s.create_run(&run("rc", "child", Some("r")), &msg).unwrap();
        assert_eq!(s.session("child").unwrap().unwrap().channel_id, Some(c.id));
    }

    #[test]
    fn channel_search_skips_other_channels_and_the_current_thread() {
        let s = Store::open_in_memory().unwrap();
        let c = topic(&s, "x");
        let msg = |text: &str| NewMessage {
            role: "user".into(),
            content_json: "[]".into(),
            text: text.into(),
            summary_of: None,
        };
        for (sess, chan, run) in [
            ("h1", HOME, "r1"),
            ("h2", HOME, "r2"),
            ("t1", c.id.as_str(), "r3"),
        ] {
            s.create_thread(&NewThread {
                id: sess,
                source: "api",
                channel_id: chan,
                ..NewThread::default()
            })
            .unwrap();
            s.create_run(
                &NewRun {
                    id: run,
                    session_id: sess,
                    trigger: "user",
                    model: "mock:m",
                    system_prompt: "",
                    ..NewRun::default()
                },
                &msg("the Northwind invoice"),
            )
            .unwrap();
        }
        let hits = s
            .search_channel_messages(HOME, "northwind", 10, Some("h2"))
            .unwrap();
        assert_eq!(
            hits.iter()
                .map(|h| h.session_id.as_str())
                .collect::<Vec<_>>(),
            ["h1"]
        );
    }
}
