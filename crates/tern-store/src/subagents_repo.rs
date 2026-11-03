//! Sub-agent runs and the mail between runs of one tree
//! (docs/subagents-design.md §1.2).

use rusqlite::{OptionalExtension, Row, params};

use crate::repo::{RUN_COLS, run_row};
use crate::{Result, RunRow, Store, now_ms};

/// A message to a run, consumed by one of its journal steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailRow {
    pub id: i64,
    pub to_run_id: String,
    /// `None` = the user.
    pub from_run_id: Option<String>,
    /// `finished`, `question`, `answer` or `message`.
    pub kind: String,
    /// JSON.
    pub body: String,
    pub dedupe_key: String,
    pub created_at: i64,
    pub consumed_run_id: Option<String>,
    pub consumed_step: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct NewMail<'a> {
    pub to_run_id: &'a str,
    pub from_run_id: Option<&'a str>,
    pub kind: &'a str,
    pub body: &'a str,
    pub dedupe_key: &'a str,
}

const MAIL_COLS: &str = "id, to_run_id, from_run_id, kind, body, dedupe_key, created_at, \
                         consumed_run_id, consumed_step";

fn mail_row(r: &Row<'_>) -> rusqlite::Result<MailRow> {
    Ok(MailRow {
        id: r.get(0)?,
        to_run_id: r.get(1)?,
        from_run_id: r.get(2)?,
        kind: r.get(3)?,
        body: r.get(4)?,
        dedupe_key: r.get(5)?,
        created_at: r.get(6)?,
        consumed_run_id: r.get(7)?,
        consumed_step: r.get(8)?,
    })
}

impl Store {
    /// Runs started by `run_id`'s `agent_spawn` calls, oldest first.
    pub fn children(&self, run_id: &str) -> Result<Vec<RunRow>> {
        let sql =
            format!("SELECT {RUN_COLS} FROM runs WHERE parent_run_id = ?1 ORDER BY started_at, id");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([run_id], run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The child `run_id` started with tool call `call_id`, if any.
    pub fn child_by_call(&self, run_id: &str, call_id: &str) -> Result<Option<RunRow>> {
        let sql =
            format!("SELECT {RUN_COLS} FROM runs WHERE parent_run_id = ?1 AND parent_call_id = ?2");
        Ok(self
            .conn()?
            .query_row(&sql, params![run_id, call_id], run_row)
            .optional()?)
    }

    /// The top of `run_id`'s tree (itself for a top-level run).
    pub fn root_run(&self, run_id: &str) -> Result<String> {
        let conn = self.conn()?;
        let mut id = run_id.to_owned();
        // Bounded, in case of a (corrupt) cycle.
        for _ in 0..64 {
            let parent: Option<String> = conn
                .query_row("SELECT parent_run_id FROM runs WHERE id = ?1", [&id], |r| {
                    r.get(0)
                })
                .optional()?
                .flatten();
            match parent {
                Some(p) => id = p,
                None => break,
            }
        }
        Ok(id)
    }

    /// Sub-agent runs with `status`, across every tree, oldest first.
    pub fn child_runs_with_status(&self, status: &str) -> Result<Vec<RunRow>> {
        let sql = format!(
            "SELECT {RUN_COLS} FROM runs WHERE parent_run_id IS NOT NULL AND status = ?1
             ORDER BY started_at, id"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([status], run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Sub-agent runs that have no mail with dedupe key `<prefix><run id>`
    /// (e.g. `finished:`), for reconciling after a crash.
    pub fn child_runs_without_mail(&self, prefix: &str) -> Result<Vec<RunRow>> {
        let cols = RUN_COLS
            .split(", ")
            .map(|c| format!("r.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {cols} FROM runs r WHERE r.parent_run_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM agent_mail m WHERE m.dedupe_key = ?1 || r.id)
             ORDER BY r.started_at, r.id"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([prefix], run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Insert mail unless its `dedupe_key` exists. Returns whether it was new.
    pub fn post_mail(&self, mail: &NewMail<'_>) -> Result<bool> {
        let n = self.conn()?.execute(
            "INSERT INTO agent_mail (to_run_id, from_run_id, kind, body, dedupe_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (dedupe_key) DO NOTHING",
            params![
                mail.to_run_id,
                mail.from_run_id,
                mail.kind,
                mail.body,
                mail.dedupe_key,
                now_ms()
            ],
        )?;
        Ok(n > 0)
    }

    /// Unconsumed mail to `run_id`, oldest first.
    pub fn pending_mail(&self, run_id: &str) -> Result<Vec<MailRow>> {
        let sql = format!(
            "SELECT {MAIL_COLS} FROM agent_mail WHERE to_run_id = ?1 AND consumed_step IS NULL
             ORDER BY id"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([run_id], mail_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mail_by_key(&self, dedupe_key: &str) -> Result<Option<MailRow>> {
        let sql = format!("SELECT {MAIL_COLS} FROM agent_mail WHERE dedupe_key = ?1");
        Ok(self
            .conn()?
            .query_row(&sql, [dedupe_key], mail_row)
            .optional()?)
    }

    /// Token usage summed over a run's journaled steps:
    /// (input, output, cache_read, cache_write).
    pub fn run_usage(&self, run_id: &str) -> Result<(u64, u64, u64, u64)> {
        let row = self.conn()?.query_row(
            "SELECT
               COALESCE(SUM(json_extract(usage_json, '$.input_tokens')), 0),
               COALESCE(SUM(json_extract(usage_json, '$.output_tokens')), 0),
               COALESCE(SUM(json_extract(usage_json, '$.cache_read_tokens')), 0),
               COALESCE(SUM(json_extract(usage_json, '$.cache_write_tokens')), 0)
             FROM steps WHERE run_id = ?1 AND usage_json IS NOT NULL",
            [run_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )?;
        Ok((row.0 as u64, row.1 as u64, row.2 as u64, row.3 as u64))
    }

    /// Token usage summed over a session's runs and every run below them
    /// (their sub-agents, at any depth): (input, output, cache_read,
    /// cache_write).
    pub fn session_tree_usage(&self, session_id: &str) -> Result<(u64, u64, u64, u64)> {
        let row = self.conn()?.query_row(
            "WITH RECURSIVE tree(id) AS (
               SELECT id FROM runs WHERE session_id = ?1
               UNION
               SELECT r.id FROM runs r JOIN tree t ON r.parent_run_id = t.id
             )
             SELECT
               COALESCE(SUM(json_extract(usage_json, '$.input_tokens')), 0),
               COALESCE(SUM(json_extract(usage_json, '$.output_tokens')), 0),
               COALESCE(SUM(json_extract(usage_json, '$.cache_read_tokens')), 0),
               COALESCE(SUM(json_extract(usage_json, '$.cache_write_tokens')), 0)
             FROM steps WHERE run_id IN (SELECT id FROM tree) AND usage_json IS NOT NULL",
            [session_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )?;
        Ok((row.0 as u64, row.1 as u64, row.2 as u64, row.3 as u64))
    }

    /// The plain text of message `id`.
    pub fn message_text(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row("SELECT text FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Number of a run's steps of `kind` (e.g. `llm` = model turns).
    pub fn step_count(&self, run_id: &str, kind: &str) -> Result<u32> {
        Ok(self.conn()?.query_row(
            "SELECT count(*) FROM steps WHERE run_id = ?1 AND kind = ?2",
            params![run_id, kind],
            |r| r.get(0),
        )?)
    }

    /// The newest journal step of a run: (kind, status, input JSON).
    pub fn last_step(&self, run_id: &str) -> Result<Option<(String, String, String)>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT kind, status, input_json FROM steps WHERE run_id = ?1
                 ORDER BY idx DESC LIMIT 1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewMessage, NewRun};

    fn msg() -> NewMessage {
        NewMessage {
            role: "user".into(),
            content_json: "[]".into(),
            text: "task".into(),
            summary_of: None,
        }
    }

    fn setup() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.create_session("s", "cli", None).unwrap();
        s.create_run(
            &NewRun {
                id: "p",
                session_id: "s",
                trigger: "user",
                model: "mock:m",
                system_prompt: "sys",
                ..NewRun::default()
            },
            &msg(),
        )
        .unwrap();
        for (id, call, queued) in [("c1", "call_1", false), ("c2", "call_2", true)] {
            s.create_session(&format!("s{id}"), "subagent", None)
                .unwrap();
            s.create_run(
                &NewRun {
                    id,
                    session_id: &format!("s{id}"),
                    trigger: "subagent",
                    model: "mock:m",
                    system_prompt: "sys",
                    parent_run_id: Some("p"),
                    parent_call_id: Some(call),
                    label: Some(id),
                    depth: 1,
                    tool_scope: Some(r#"["read_file"]"#),
                    budget_tokens: Some(1000),
                    max_iterations: Some(3),
                    queued,
                },
                &msg(),
            )
            .unwrap();
        }
        s
    }

    #[test]
    fn children_are_found_by_parent_and_call() {
        let s = setup();
        let kids = s.children("p").unwrap();
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0].depth, 1);
        assert_eq!(kids[0].tool_scope.as_deref(), Some(r#"["read_file"]"#));
        assert_eq!(kids[0].budget_tokens, Some(1000));
        assert_eq!(kids[0].max_iterations, Some(3));
        assert_eq!(kids[1].status, "queued");
        assert_eq!(s.child_by_call("p", "call_2").unwrap().unwrap().id, "c2");
        assert!(s.child_by_call("p", "call_9").unwrap().is_none());
        assert_eq!(s.root_run("c1").unwrap(), "p");
        assert_eq!(s.root_run("p").unwrap(), "p");
        assert_eq!(s.child_runs_with_status("queued").unwrap().len(), 1);
    }

    #[test]
    fn a_call_id_spawns_at_most_one_child_and_labels_are_unique() {
        let s = setup();
        s.create_session("sx", "subagent", None).unwrap();
        let dup = |call: &str, label: &str| {
            s.create_run(
                &NewRun {
                    id: "c3",
                    session_id: "sx",
                    parent_run_id: Some("p"),
                    parent_call_id: Some(call),
                    label: Some(label),
                    ..NewRun::default()
                },
                &msg(),
            )
        };
        assert!(dup("call_1", "other").is_err());
        assert!(dup("call_3", "c1").is_err());
    }

    #[test]
    fn mail_is_deduplicated_consumed_by_a_step_and_returned_on_truncation() {
        let s = setup();
        let mail = |key: &'static str| NewMail {
            to_run_id: "p",
            from_run_id: Some("c1"),
            kind: "finished",
            body: "{}",
            dedupe_key: key,
        };
        assert!(s.post_mail(&mail("finished:c1")).unwrap());
        assert!(!s.post_mail(&mail("finished:c1")).unwrap());
        let pending = s.pending_mail("p").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            s.child_runs_without_mail("finished:")
                .unwrap()
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["c2"]
        );

        s.start_step("p", 0, "inbox", "h", "{}").unwrap();
        s.complete_step_consuming("p", 0, "null", None, &[], &[pending[0].id])
            .unwrap();
        assert!(s.pending_mail("p").unwrap().is_empty());
        let m = s.mail_by_key("finished:c1").unwrap().unwrap();
        assert_eq!(
            (m.consumed_run_id.as_deref(), m.consumed_step),
            (Some("p"), Some(0))
        );

        s.truncate_steps("p", 0).unwrap();
        assert_eq!(s.pending_mail("p").unwrap().len(), 1);
    }

    #[test]
    fn subagent_sessions_are_listed_only_on_request_and_usage_sums_trees() {
        let s = setup();
        let ids = |l: Vec<crate::SessionSummary>| {
            let mut v: Vec<String> = l.into_iter().map(|x| x.session.id).collect();
            v.sort();
            v
        };
        assert_eq!(ids(s.list_sessions(10).unwrap()), ["s"]);
        assert_eq!(
            ids(s.list_sessions_from(10, Some("subagent")).unwrap()),
            ["sc1", "sc2"]
        );
        let usage = r#"{"input_tokens":10,"output_tokens":2}"#;
        for run in ["p", "c1"] {
            s.start_step(run, 0, "llm", "h", "{}").unwrap();
            s.complete_step(run, 0, "null", Some(usage), &[]).unwrap();
        }
        assert_eq!(s.session_usage("s").unwrap(), (10, 2, 0, 0));
        assert_eq!(s.session_tree_usage("s").unwrap(), (20, 4, 0, 0));
        let input = s.run("c1").unwrap().unwrap().input_message_id.unwrap();
        assert_eq!(s.message_text(input).unwrap().as_deref(), Some("task"));
    }
}
