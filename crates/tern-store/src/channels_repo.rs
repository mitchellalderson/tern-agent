//! Channels, their threads, and posts (docs/channels-design.md §2).

use rusqlite::{OptionalExtension, Row, params};

use crate::repo::{SESSION_COLS, session_row};
use crate::{MessageHit, Result, SessionRow, Store, fts_query, now_ms};

/// Home's id (and slug). Created by the migration; never archived.
pub const HOME: &str = "home";

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
}

const CHANNEL_COLS: &str = "id, slug, title, kind, instructions, main_session_id, created_by, \
    archived_at, created_at, updated_at";
const POST_COLS: &str = "id, channel_id, session_id, run_id, author_kind, author_id, author, kind, \
    text, reply_to, points_to, tainted, dedupe_key, created_at";

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
    })
}

impl Store {
    // ---- channels ----

    pub fn create_channel(&self, c: &ChannelRow) -> Result<ChannelRow> {
        let now = now_ms();
        self.conn()?.execute(
            "INSERT INTO channels (id, slug, title, kind, instructions, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![c.id, c.slug, c.title, c.kind, c.instructions, c.created_by, now],
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
               segment_reason, branched_from, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                t.id,
                t.source,
                t.model,
                t.external_ref,
                t.channel_id,
                t.prev_session_id,
                t.segment_reason,
                t.branched_from,
                now
            ],
        )?;
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

    /// Side threads of a channel created in `[from, to)` (unix ms), oldest
    /// first.
    pub fn side_threads_between(
        &self,
        channel_id: &str,
        from: i64,
        to: i64,
    ) -> Result<Vec<SessionRow>> {
        let sql = format!(
            "SELECT {SESSION_COLS} FROM sessions
             WHERE channel_id = ?1 AND branched_from IS NOT NULL AND created_at >= ?2 AND created_at < ?3
             ORDER BY created_at"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![channel_id, from, to], session_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
    pub fn add_post(&self, p: &NewPost<'_>) -> Result<(PostRow, bool)> {
        let now = now_ms();
        let conn = self.conn()?;
        let n = conn.execute(
            "INSERT INTO posts (channel_id, session_id, run_id, author_kind, author_id, author, kind,
               text, reply_to, points_to, dedupe_key, created_at, tainted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
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
                now,
                p.tainted,
            ],
        )?;
        let sql = format!("SELECT {POST_COLS} FROM posts WHERE id = ?1");
        let row = if n > 0 {
            conn.query_row(&sql, [conn.last_insert_rowid()], post_row)?
        } else {
            let sql = format!("SELECT {POST_COLS} FROM posts WHERE dedupe_key = ?1");
            conn.query_row(&sql, [p.dedupe_key], post_row)?
        };
        if n > 0 {
            conn.execute(
                "UPDATE channels SET updated_at = ?2 WHERE id = ?1",
                params![p.channel_id, now],
            )?;
        }
        Ok((row, n > 0))
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

    /// `(post id, replies)` for the posts in `ids` that have replies other
    /// than the agent's answer to them (a side thread).
    pub fn reply_counts(&self, ids: &[i64]) -> Result<Vec<(i64, i64)>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT count(*) FROM posts WHERE reply_to = ?1 AND kind <> 'answer'")?;
        let mut out = Vec::new();
        for id in ids {
            let n: i64 = stmt.query_row([id], |r| r.get(0))?;
            if n > 0 {
                out.push((*id, n));
            }
        }
        Ok(out)
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

    pub fn set_schedule_channel(&self, schedule_id: &str, channel_id: &str) -> Result<bool> {
        Ok(self.conn()?.execute(
            "UPDATE schedules SET channel_id = ?2 WHERE id = ?1",
            params![schedule_id, channel_id],
        )? > 0)
    }

    /// Top-level runs that finished at or after `since` (unix ms), for
    /// writing answer posts a crash may have skipped.
    pub fn top_runs_finished_since(&self, since: i64) -> Result<Vec<crate::RunRow>> {
        let sql = format!(
            "SELECT {} FROM runs WHERE parent_run_id IS NULL AND finished_at >= ?1 ORDER BY finished_at",
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
