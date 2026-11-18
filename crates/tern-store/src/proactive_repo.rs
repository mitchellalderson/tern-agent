//! Proactive help (docs/innbot-learnings.md): one generic dismissible card
//! every feature writes (heartbeat, routine fixes, routine suggestions, the
//! away brief), the "Allow once" answers P7 learns from, and the host's own
//! small state (last heartbeat day, last seen).
//!
//! Every card is capped by the caller through [`Store::cards_since`]: the
//! cap is a count of cards of a kind (and subject) in a window, so a feature
//! can never post more than its limit, switch or no switch.

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store, now_ms};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CardRow {
    pub id: i64,
    pub kind: String,
    pub subject: String,
    pub agent_id: String,
    pub title: String,
    pub body: String,
    /// `None`: the card can only be dismissed.
    pub accept_label: Option<String>,
    /// What accepting tells the agent, as a message from the user.
    pub accept_message: Option<String>,
    /// `pending`, `accepted` or `dismissed`.
    pub state: String,
    pub post_id: Option<i64>,
    pub created_at: i64,
    pub decided_at: Option<i64>,
    /// Where the card is posted, when not the agent's DM.
    pub channel_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct NewCard<'a> {
    pub kind: &'a str,
    pub subject: &'a str,
    pub agent_id: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub accept_label: Option<&'a str>,
    pub accept_message: Option<&'a str>,
    /// Post it here instead of in the agent's DM.
    pub channel_id: Option<&'a str>,
}

const COLS: &str = "id, kind, subject, agent_id, title, body, accept_label, accept_message, state, \
    post_id, created_at, decided_at, channel_id";

fn row(r: &Row<'_>) -> rusqlite::Result<CardRow> {
    Ok(CardRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        subject: r.get(2)?,
        agent_id: r.get(3)?,
        title: r.get(4)?,
        body: r.get(5)?,
        accept_label: r.get(6)?,
        accept_message: r.get(7)?,
        state: r.get(8)?,
        post_id: r.get(9)?,
        created_at: r.get(10)?,
        decided_at: r.get(11)?,
        channel_id: r.get(12)?,
    })
}

impl Store {
    /// Record a card, unless `max` cards of `kind` (and `subject`, when
    /// `per_subject`) were already made since `since` (ms): `None` then.
    /// The count and the insert share one connection.
    pub fn create_card_capped(
        &self,
        c: &NewCard<'_>,
        since: i64,
        max: u32,
        per_subject: bool,
    ) -> Result<Option<CardRow>> {
        let conn = self.conn()?;
        let made: i64 = conn.query_row(
            "SELECT count(*) FROM proactive_cards
             WHERE kind = ?1 AND created_at >= ?2 AND (?3 = 0 OR subject = ?4)",
            params![c.kind, since, per_subject, c.subject],
            |r| r.get(0),
        )?;
        if made >= i64::from(max) {
            return Ok(None);
        }
        conn.execute(
            "INSERT INTO proactive_cards
               (kind, subject, agent_id, title, body, accept_label, accept_message, created_at, channel_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                c.kind,
                c.subject,
                c.agent_id,
                c.title,
                c.body,
                c.accept_label,
                c.accept_message,
                now_ms(),
                c.channel_id
            ],
        )?;
        let sql = format!("SELECT {COLS} FROM proactive_cards WHERE id = ?1");
        Ok(Some(conn.query_row(
            &sql,
            [conn.last_insert_rowid()],
            row,
        )?))
    }

    pub fn card(&self, id: i64) -> Result<Option<CardRow>> {
        let sql = format!("SELECT {COLS} FROM proactive_cards WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], row).optional()?)
    }

    /// The card whose post is `post_id` (or whose Home pointer is).
    pub fn card_of_post(&self, post_id: i64) -> Result<Option<CardRow>> {
        let sql = format!("SELECT {COLS} FROM proactive_cards WHERE post_id = ?1");
        Ok(self.conn()?.query_row(&sql, [post_id], row).optional()?)
    }

    /// The pending card of `kind` about `subject`, if any.
    pub fn pending_card_for(&self, kind: &str, subject: &str) -> Result<Option<CardRow>> {
        let sql = format!(
            "SELECT {COLS} FROM proactive_cards
             WHERE kind = ?1 AND subject = ?2 AND state = 'pending' ORDER BY id DESC LIMIT 1"
        );
        Ok(self
            .conn()?
            .query_row(&sql, params![kind, subject], row)
            .optional()?)
    }

    pub fn set_card_post(&self, id: i64, post_id: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE proactive_cards SET post_id = ?2 WHERE id = ?1",
            params![id, post_id],
        )?;
        Ok(())
    }

    /// Cards, newest first; `state` filters.
    pub fn cards(&self, state: Option<&str>, limit: u32) -> Result<Vec<CardRow>> {
        let sql = format!(
            "SELECT {COLS} FROM proactive_cards WHERE (?1 IS NULL OR state = ?1)
             ORDER BY id DESC LIMIT ?2"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![state, limit], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Cards of `kind` made since `since` (ms), optionally for one subject.
    pub fn cards_since(&self, kind: &str, subject: Option<&str>, since: i64) -> Result<u32> {
        Ok(self.conn()?.query_row(
            "SELECT count(*) FROM proactive_cards
             WHERE kind = ?1 AND created_at >= ?2 AND (?3 IS NULL OR subject = ?3)",
            params![kind, since, subject],
            |r| r.get(0),
        )?)
    }

    /// Fill in a card reserved before its content was known (a routine fix
    /// waits for the agent's diagnosis).
    pub fn fill_card(
        &self,
        id: i64,
        title: &str,
        body: &str,
        accept_label: Option<&str>,
        accept_message: Option<&str>,
    ) -> Result<()> {
        self.conn()?.execute(
            "UPDATE proactive_cards SET title = ?2, body = ?3, accept_label = ?4, accept_message = ?5
             WHERE id = ?1",
            params![id, title, body, accept_label, accept_message],
        )?;
        Ok(())
    }

    /// What the user asked in the top-level runs `agent` started since
    /// `since` (ms), other than `except`, newest first (P1).
    pub fn recent_user_asks(
        &self,
        agent: &str,
        since: i64,
        except: &str,
        limit: u32,
    ) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.text FROM runs r
               JOIN messages m ON m.id = r.input_message_id
               JOIN sessions s ON s.id = r.session_id
             WHERE r.trigger = 'user' AND r.parent_run_id IS NULL AND r.started_at >= ?1
               AND r.id != ?2 AND r.taint IS NULL AND r.input_taint IS NULL
               AND coalesce(s.agent_id, 'tern') = ?3
             ORDER BY r.started_at DESC LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![since, except, agent, limit], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The text of the message that started `run_id`.
    pub fn run_input_text(&self, run_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT m.text FROM runs r JOIN messages m ON m.id = r.input_message_id
                 WHERE r.id = ?1",
                [run_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Decide a pending card: `accepted` or `dismissed`. False if it was
    /// already decided.
    pub fn decide_card(&self, id: i64, state: &str) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE proactive_cards SET state = ?2, decided_at = ?3
             WHERE id = ?1 AND state = 'pending'",
            params![id, state, now_ms()],
        )?;
        Ok(n > 0)
    }

    // ---- P7: "Allow once" answers ----

    /// The user answered "Allow once" for `agent`, `tool` and `target`.
    pub fn record_approval_tap(&self, agent: &str, tool: &str, target: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO approval_taps (agent_id, tool, target, at) VALUES (?1, ?2, ?3, ?4)",
            params![agent, tool, target, now_ms()],
        )?;
        // Nothing reads taps older than a few weeks; keep the table small.
        conn.execute(
            "DELETE FROM approval_taps WHERE at < ?1",
            [now_ms() - 60 * 24 * 3_600_000],
        )?;
        Ok(())
    }

    /// How many times since `since` (ms) the user said "Allow once" to
    /// exactly this.
    pub fn approval_taps_since(
        &self,
        agent: &str,
        tool: &str,
        target: &str,
        since: i64,
    ) -> Result<u32> {
        Ok(self.conn()?.query_row(
            "SELECT count(*) FROM approval_taps
             WHERE agent_id = ?1 AND tool = ?2 AND target = ?3 AND at >= ?4",
            params![agent, tool, target, since],
            |r| r.get(0),
        )?)
    }

    /// Forget the taps for a target (its grant was made, or revoked).
    pub fn clear_approval_taps(&self, agent: &str, tool: &str, target: &str) -> Result<()> {
        self.conn()?.execute(
            "DELETE FROM approval_taps WHERE agent_id = ?1 AND tool = ?2 AND target = ?3",
            params![agent, tool, target],
        )?;
        Ok(())
    }

    // ---- the host's own state ----

    pub fn proactive_state(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT value FROM proactive_state WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_proactive_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO proactive_state (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            params![key, value, now_ms()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn card(kind: &'static str, subject: &'static str) -> NewCard<'static> {
        NewCard {
            kind,
            subject,
            agent_id: "tern",
            title: "t",
            ..NewCard::default()
        }
    }

    #[test]
    fn a_card_is_capped_per_subject_and_decided_once() {
        let s = Store::open_in_memory().unwrap();
        let week = now_ms() - 7 * 24 * 3_600_000;
        let a = s
            .create_card_capped(&card("routine_fix", "r1"), week, 1, true)
            .unwrap()
            .unwrap();
        assert!(
            s.create_card_capped(&card("routine_fix", "r1"), week, 1, true)
                .unwrap()
                .is_none(),
            "one per routine per week"
        );
        assert!(
            s.create_card_capped(&card("routine_fix", "r2"), week, 1, true)
                .unwrap()
                .is_some(),
            "another routine has its own"
        );
        assert!(s.decide_card(a.id, "dismissed").unwrap());
        assert!(!s.decide_card(a.id, "accepted").unwrap(), "once");
        assert_eq!(s.card(a.id).unwrap().unwrap().state, "dismissed");
        assert_eq!(s.cards_since("routine_fix", Some("r1"), week).unwrap(), 1);
        assert_eq!(s.cards(Some("pending"), 10).unwrap().len(), 1);
    }

    #[test]
    fn taps_count_per_target_and_clear() {
        let s = Store::open_in_memory().unwrap();
        for _ in 0..3 {
            s.record_approval_tap("tern", "shell", "ls *").unwrap();
        }
        s.record_approval_tap("tern", "shell", "rm *").unwrap();
        s.record_approval_tap("other", "shell", "ls *").unwrap();
        let since = now_ms() - 1000;
        assert_eq!(
            s.approval_taps_since("tern", "shell", "ls *", since)
                .unwrap(),
            3
        );
        assert_eq!(
            s.approval_taps_since("tern", "shell", "ls *", now_ms() + 1000)
                .unwrap(),
            0
        );
        s.clear_approval_taps("tern", "shell", "ls *").unwrap();
        assert_eq!(
            s.approval_taps_since("tern", "shell", "ls *", since)
                .unwrap(),
            0
        );
    }

    #[test]
    fn state_is_a_key_value_store() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.proactive_state("k").unwrap(), None);
        s.set_proactive_state("k", "a").unwrap();
        s.set_proactive_state("k", "b").unwrap();
        assert_eq!(s.proactive_state("k").unwrap().as_deref(), Some("b"));
    }
}
