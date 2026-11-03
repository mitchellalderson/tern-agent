//! Typed access to sessions, runs, steps, and messages.
//!
//! Values that the domain layer serializes (step inputs and outputs, message
//! content) cross this boundary as JSON strings.

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store};

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub source: String,
    pub external_ref: Option<String>,
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub id: String,
    pub session_id: String,
    pub status: String,
    pub trigger: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub input_message_id: Option<i64>,
    pub started_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    pub run_id: String,
    pub idx: u32,
    pub kind: String,
    pub input_hash: String,
    pub input_json: String,
    pub output_json: Option<String>,
    pub status: String,
    pub usage_json: Option<String>,
    pub error: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    pub role: String,
    pub content_json: String,
    pub text: String,
    /// Set on compaction summaries: the highest message id they replace.
    pub summary_of: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub id: i64,
    pub session_id: String,
    pub run_id: Option<String>,
    pub step_idx: Option<u32>,
    pub role: String,
    pub content_json: String,
    pub text: String,
    pub created_at: i64,
    pub summary_of: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRow {
    pub id: i64,
    pub content: String,
    pub tags: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageHit {
    pub message_id: i64,
    pub session_id: String,
    pub session_title: Option<String>,
    pub role: String,
    pub created_at: i64,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub session: SessionRow,
    pub messages: i64,
    pub runs: i64,
}

/// Values for a new run row.
#[derive(Debug, Clone)]
pub struct NewRun<'a> {
    pub id: &'a str,
    pub session_id: &'a str,
    pub trigger: &'a str,
    pub model: &'a str,
    pub system_prompt: &'a str,
}

const SESSION_COLS: &str = "id, title, source, external_ref, model, created_at, updated_at";
const RUN_COLS: &str = "id, session_id, status, trigger, model, system_prompt, input_message_id, \
                        started_at, updated_at, finished_at, error";
const STEP_COLS: &str = "run_id, idx, kind, input_hash, input_json, output_json, status, \
                         usage_json, error, started_at, finished_at";
const MESSAGE_COLS: &str =
    "id, session_id, run_id, step_idx, role, content_json, text, created_at, summary_of";

fn session_row(r: &Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: r.get(0)?,
        title: r.get(1)?,
        source: r.get(2)?,
        external_ref: r.get(3)?,
        model: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

fn run_row(r: &Row<'_>) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        status: r.get(2)?,
        trigger: r.get(3)?,
        model: r.get(4)?,
        system_prompt: r.get(5)?,
        input_message_id: r.get(6)?,
        started_at: r.get(7)?,
        updated_at: r.get(8)?,
        finished_at: r.get(9)?,
        error: r.get(10)?,
    })
}

fn step_row(r: &Row<'_>) -> rusqlite::Result<StepRow> {
    Ok(StepRow {
        run_id: r.get(0)?,
        idx: r.get(1)?,
        kind: r.get(2)?,
        input_hash: r.get(3)?,
        input_json: r.get(4)?,
        output_json: r.get(5)?,
        status: r.get(6)?,
        usage_json: r.get(7)?,
        error: r.get(8)?,
        started_at: r.get(9)?,
        finished_at: r.get(10)?,
    })
}

fn message_row(r: &Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        run_id: r.get(2)?,
        step_idx: r.get(3)?,
        role: r.get(4)?,
        content_json: r.get(5)?,
        text: r.get(6)?,
        created_at: r.get(7)?,
        summary_of: r.get(8)?,
    })
}

impl Store {
    // ---- sessions ----

    pub fn create_session(
        &self,
        id: &str,
        source: &str,
        model: Option<&str>,
    ) -> Result<SessionRow> {
        let now = now_ms();
        self.conn()?.execute(
            "INSERT INTO sessions (id, source, model, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, source, model, now],
        )?;
        Ok(SessionRow {
            id: id.into(),
            title: None,
            source: source.into(),
            external_ref: None,
            model: model.map(Into::into),
            created_at: now,
            updated_at: now,
        })
    }

    pub fn session(&self, id: &str) -> Result<Option<SessionRow>> {
        let sql = format!("SELECT {SESSION_COLS} FROM sessions WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], session_row).optional()?)
    }

    pub fn set_session_model(&self, id: &str, model: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE sessions SET model = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, model, now_ms()],
        )?;
        Ok(())
    }

    // ---- runs ----

    /// Create a running run and its input message atomically.
    pub fn create_run(&self, run: &NewRun<'_>, input: &NewMessage) -> Result<RunRow> {
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO runs (id, session_id, status, trigger, model, system_prompt, started_at, updated_at)
             VALUES (?1, ?2, 'running', ?3, ?4, ?5, ?6, ?6)",
            params![run.id, run.session_id, run.trigger, run.model, run.system_prompt, now],
        )?;
        tx.execute(
            "INSERT INTO messages (session_id, run_id, step_idx, role, content_json, text, created_at)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
            params![run.session_id, run.id, input.role, input.content_json, input.text, now],
        )?;
        let msg_id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE runs SET input_message_id = ?2 WHERE id = ?1",
            params![run.id, msg_id],
        )?;
        let title: String = input.text.chars().take(80).collect();
        tx.execute(
            "UPDATE sessions SET updated_at = ?2, title = COALESCE(title, ?3) WHERE id = ?1",
            params![run.session_id, now, title.trim()],
        )?;
        let sql = format!("SELECT {RUN_COLS} FROM runs WHERE id = ?1");
        let row = tx.query_row(&sql, [run.id], run_row)?;
        tx.commit()?;
        Ok(row)
    }

    pub fn run(&self, id: &str) -> Result<Option<RunRow>> {
        let sql = format!("SELECT {RUN_COLS} FROM runs WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], run_row).optional()?)
    }

    pub fn runs_with_status(&self, status: &str) -> Result<Vec<RunRow>> {
        let sql = format!("SELECT {RUN_COLS} FROM runs WHERE status = ?1 ORDER BY started_at");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([status], run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn recent_runs(&self, limit: usize) -> Result<Vec<RunRow>> {
        let sql = format!("SELECT {RUN_COLS} FROM runs ORDER BY started_at DESC LIMIT ?1");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([limit as i64], run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Set a run's status. Terminal statuses also set `finished_at`.
    pub fn set_run_status(&self, id: &str, status: &str, error: Option<&str>) -> Result<()> {
        let now = now_ms();
        let finished = (status != "running").then_some(now);
        self.conn()?.execute(
            "UPDATE runs SET status = ?2, error = ?3, updated_at = ?4, finished_at = ?5 WHERE id = ?1",
            params![id, status, error, now, finished],
        )?;
        Ok(())
    }

    // ---- steps ----

    pub fn step(&self, run_id: &str, idx: u32) -> Result<Option<StepRow>> {
        let sql = format!("SELECT {STEP_COLS} FROM steps WHERE run_id = ?1 AND idx = ?2");
        Ok(self
            .conn()?
            .query_row(&sql, params![run_id, idx], step_row)
            .optional()?)
    }

    pub fn steps(&self, run_id: &str) -> Result<Vec<StepRow>> {
        let sql = format!("SELECT {STEP_COLS} FROM steps WHERE run_id = ?1 ORDER BY idx");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([run_id], step_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Insert (or reset) a step as `started`.
    pub fn start_step(
        &self,
        run_id: &str,
        idx: u32,
        kind: &str,
        input_hash: &str,
        input_json: &str,
    ) -> Result<()> {
        let now = now_ms();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO steps (run_id, idx, kind, input_hash, input_json, status, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'started', ?6)
             ON CONFLICT (run_id, idx) DO UPDATE SET
               kind = excluded.kind, input_hash = excluded.input_hash,
               input_json = excluded.input_json, status = 'started',
               output_json = NULL, usage_json = NULL, error = NULL,
               started_at = excluded.started_at, finished_at = NULL",
            params![run_id, idx, kind, input_hash, input_json, now],
        )?;
        conn.execute(
            "UPDATE runs SET updated_at = ?2 WHERE id = ?1",
            params![run_id, now],
        )?;
        Ok(())
    }

    /// Mark a step completed and append its derived messages, atomically.
    pub fn complete_step(
        &self,
        run_id: &str,
        idx: u32,
        output_json: &str,
        usage_json: Option<&str>,
        messages: &[NewMessage],
    ) -> Result<()> {
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE steps SET status = 'completed', output_json = ?3, usage_json = ?4,
               error = NULL, finished_at = ?5
             WHERE run_id = ?1 AND idx = ?2",
            params![run_id, idx, output_json, usage_json, now],
        )?;
        let session_id: String =
            tx.query_row("SELECT session_id FROM runs WHERE id = ?1", [run_id], |r| {
                r.get(0)
            })?;
        for m in messages {
            tx.execute(
                "INSERT INTO messages (session_id, run_id, step_idx, role, content_json, text, created_at, summary_of)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![session_id, run_id, idx, m.role, m.content_json, m.text, now, m.summary_of],
            )?;
        }
        tx.execute(
            "UPDATE runs SET updated_at = ?2 WHERE id = ?1",
            params![run_id, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn fail_step(&self, run_id: &str, idx: u32, error: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE steps SET status = 'failed', error = ?3, finished_at = ?4
             WHERE run_id = ?1 AND idx = ?2",
            params![run_id, idx, error, now_ms()],
        )?;
        Ok(())
    }

    /// Delete steps `idx..` of a run and the messages they appended.
    pub fn truncate_steps(&self, run_id: &str, idx: u32) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM messages WHERE run_id = ?1 AND step_idx >= ?2",
            params![run_id, idx],
        )?;
        tx.execute(
            "DELETE FROM steps WHERE run_id = ?1 AND idx >= ?2",
            params![run_id, idx],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ---- approval allowlist ----

    pub fn allowlist(&self, tool: &str) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT pattern FROM approval_allowlist WHERE tool = ?1 ORDER BY id")?;
        let rows = stmt.query_map([tool], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_allowlist(&self, tool: &str, pattern: &str) -> Result<()> {
        let conn = self.conn()?;
        let exists: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM approval_allowlist WHERE tool = ?1 AND pattern = ?2)",
            params![tool, pattern],
            |r| r.get(0),
        )?;
        if !exists {
            conn.execute(
                "INSERT INTO approval_allowlist (tool, pattern, created_at) VALUES (?1, ?2, ?3)",
                params![tool, pattern, now_ms()],
            )?;
        }
        Ok(())
    }

    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let sql = format!(
            "SELECT {cols},
               (SELECT count(*) FROM messages m WHERE m.session_id = s.id),
               (SELECT count(*) FROM runs r WHERE r.session_id = s.id)
             FROM sessions s ORDER BY updated_at DESC LIMIT ?1",
            cols = SESSION_COLS
                .split(", ")
                .map(|c| format!("s.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(SessionSummary {
                session: session_row(r)?,
                messages: r.get(7)?,
                runs: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn runs_for_session(&self, session_id: &str) -> Result<Vec<RunRow>> {
        let sql = format!("SELECT {RUN_COLS} FROM runs WHERE session_id = ?1 ORDER BY started_at");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([session_id], run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- memories ----

    pub fn save_memory(&self, content: &str, tags: &[String]) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO memories (content, tags, created_at) VALUES (?1, ?2, ?3)",
            params![content, tags.join(" "), now_ms()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// BM25-ranked full-text search. Returns nothing for queries with no
    /// searchable words.
    pub fn search_memories(&self, query: &str, limit: usize) -> Result<Vec<MemoryRow>> {
        let Some(q) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.content, COALESCE(m.tags, ''), m.created_at
             FROM memories_fts f JOIN memories m ON m.id = f.rowid
             WHERE memories_fts MATCH ?1
             ORDER BY bm25(memories_fts) LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![q, limit as i64], |r| {
            Ok(MemoryRow {
                id: r.get(0)?,
                content: r.get(1)?,
                tags: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Search messages in every session except `exclude_session`.
    pub fn search_messages(
        &self,
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
             WHERE messages_fts MATCH ?1 AND m.session_id IS NOT ?3
             ORDER BY bm25(messages_fts) LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![q, limit as i64, exclude_session], |r| {
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

    // ---- messages ----

    /// The conversation as step `before_idx` of `run` saw it: every earlier
    /// session message up to and including the run's input, plus messages
    /// appended by this run's steps before `before_idx`.
    pub fn run_context(&self, run: &RunRow, before_idx: u32) -> Result<Vec<MessageRow>> {
        let sql = format!(
            "SELECT {MESSAGE_COLS} FROM messages
             WHERE session_id = ?1
               AND (id <= ?2 OR (run_id = ?3 AND step_idx < ?4))
             ORDER BY id"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let input_id = run.input_message_id.unwrap_or(i64::MAX);
        let rows = stmt.query_map(
            params![run.session_id, input_id, run.id, before_idx],
            message_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn session_messages(&self, session_id: &str) -> Result<Vec<MessageRow>> {
        let sql = format!("SELECT {MESSAGE_COLS} FROM messages WHERE session_id = ?1 ORDER BY id");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([session_id], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Turn free text into a safe FTS5 query: each word quoted, OR-ed together
/// (BM25 ranks documents matching more words higher).
pub fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\""))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(text: &str) -> NewMessage {
        NewMessage {
            role: "user".into(),
            content_json: "[]".into(),
            text: text.into(),
            summary_of: None,
        }
    }

    fn setup() -> (Store, RunRow) {
        let s = Store::open_in_memory().unwrap();
        s.create_session("s", "cli", Some("mock:m")).unwrap();
        let run = s
            .create_run(
                &NewRun {
                    id: "r",
                    session_id: "s",
                    trigger: "user",
                    model: "mock:m",
                    system_prompt: "sys",
                },
                &msg("hello"),
            )
            .unwrap();
        (s, run)
    }

    #[test]
    fn create_run_links_input_message() {
        let (s, run) = setup();
        assert_eq!(run.status, "running");
        let ctx = s.run_context(&run, 0).unwrap();
        assert_eq!(ctx.len(), 1);
        assert_eq!(ctx[0].text, "hello");
        assert_eq!(Some(ctx[0].id), run.input_message_id);
    }

    #[test]
    fn step_lifecycle_and_context_window() {
        let (s, run) = setup();
        s.start_step("r", 0, "llm", "h0", "{}").unwrap();
        assert_eq!(s.step("r", 0).unwrap().unwrap().status, "started");
        s.complete_step("r", 0, "\"out\"", Some("{}"), &[msg("a0")])
            .unwrap();
        s.start_step("r", 1, "tool", "h1", "{}").unwrap();
        s.complete_step("r", 1, "\"out\"", None, &[msg("t1")])
            .unwrap();

        let texts = |idx| -> Vec<String> {
            s.run_context(&run, idx)
                .unwrap()
                .into_iter()
                .map(|m| m.text)
                .collect()
        };
        assert_eq!(texts(0), ["hello"]);
        assert_eq!(texts(1), ["hello", "a0"]);
        assert_eq!(texts(2), ["hello", "a0", "t1"]);

        s.truncate_steps("r", 1).unwrap();
        assert!(s.step("r", 1).unwrap().is_none());
        assert_eq!(texts(5), ["hello", "a0"]);
    }

    #[test]
    fn restarting_a_step_clears_previous_outcome() {
        let (s, _) = setup();
        s.start_step("r", 0, "tool", "h", "{}").unwrap();
        s.fail_step("r", 0, "boom").unwrap();
        let failed = s.step("r", 0).unwrap().unwrap();
        assert_eq!(
            (failed.status.as_str(), failed.error.as_deref()),
            ("failed", Some("boom"))
        );
        s.start_step("r", 0, "tool", "h", "{}").unwrap();
        let again = s.step("r", 0).unwrap().unwrap();
        assert_eq!(again.status, "started");
        assert_eq!(again.error, None);
    }

    #[test]
    fn later_runs_do_not_leak_into_earlier_context() {
        let (s, run) = setup();
        s.create_run(
            &NewRun {
                id: "r2",
                session_id: "s",
                trigger: "user",
                model: "mock:m",
                system_prompt: "sys",
            },
            &msg("second"),
        )
        .unwrap();
        let ctx = s.run_context(&run, 10).unwrap();
        assert_eq!(ctx.len(), 1);
    }

    #[test]
    fn memory_search_ranks_and_tolerates_syntax() {
        let (s, _) = setup();
        s.save_memory("User prefers dark roast coffee", &["prefs".into()])
            .unwrap();
        s.save_memory("User's dog is named Biscuit", &["pets".into()])
            .unwrap();
        s.save_memory("coffee coffee shop on 5th street", &[])
            .unwrap();
        let hits = s
            .search_memories("what coffee does the user like?", 10)
            .unwrap();
        assert_eq!(hits[0].content, "User prefers dark roast coffee");
        assert_eq!(
            hits.len(),
            3,
            "OR semantics: 'user' matches the dog memory too"
        );
        assert_eq!(
            s.search_memories("pets", 10).unwrap()[0].content,
            "User's dog is named Biscuit"
        );
        assert!(s.search_memories("\"(*^", 10).unwrap().is_empty());
        assert!(s.search_memories("NEAR AND OR", 10).is_ok());
    }

    #[test]
    fn message_search_returns_snippets() {
        let (s, _) = setup();
        assert!(s.search_messages("hello", 5, Some("s")).unwrap().is_empty());
        let hits = s.search_messages("hello", 5, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s");
        assert!(hits[0].snippet.contains("[hello]"), "{}", hits[0].snippet);
        assert_eq!(hits[0].session_title.as_deref(), Some("hello"));
    }

    #[test]
    fn sessions_are_listed_with_counts() {
        let (s, _) = setup();
        let list = s.list_sessions(10).unwrap();
        assert_eq!((list[0].messages, list[0].runs), (1, 1));
    }

    #[test]
    fn run_status_transitions() {
        let (s, _) = setup();
        assert_eq!(s.runs_with_status("running").unwrap().len(), 1);
        s.set_run_status("r", "completed", None).unwrap();
        let r = s.run("r").unwrap().unwrap();
        assert_eq!(r.status, "completed");
        assert!(r.finished_at.is_some());
    }
}
