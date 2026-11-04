//! Channel suggestions (docs/channels-design.md C5): `channel_suggest`
//! records one, its card is a post, and the user accepts or declines.

use rusqlite::{OptionalExtension, Row, params};

use crate::members_repo::{ids_from_json, ids_to_json};
use crate::{Result, Store, now_ms};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SuggestionRow {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub why: String,
    /// Schedule ids accepting would move to the new channel.
    pub routines: Vec<String>,
    pub run_id: Option<String>,
    /// The suggesting run's conversation (its tree's root session).
    pub session_id: Option<String>,
    pub post_id: Option<i64>,
    /// `pending`, `accepted` or `declined`.
    pub state: String,
    /// The channel accepting created.
    pub channel_id: Option<String>,
    pub created_at: i64,
    pub decided_at: Option<i64>,
}

/// Values for a new suggestion.
#[derive(Debug, Clone, Default)]
pub struct NewSuggestion<'a> {
    pub slug: &'a str,
    pub title: &'a str,
    pub why: &'a str,
    pub routines: &'a [String],
    pub run_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
}

const COLS: &str = "id, slug, title, why, routines, run_id, session_id, post_id, state, channel_id, \
    created_at, decided_at";

fn row(r: &Row<'_>) -> rusqlite::Result<SuggestionRow> {
    Ok(SuggestionRow {
        id: r.get(0)?,
        slug: r.get(1)?,
        title: r.get(2)?,
        why: r.get(3)?,
        routines: r
            .get::<_, Option<String>>(4)?
            .map(|s| ids_from_json(&s))
            .unwrap_or_default(),
        run_id: r.get(5)?,
        session_id: r.get(6)?,
        post_id: r.get(7)?,
        state: r.get(8)?,
        channel_id: r.get(9)?,
        created_at: r.get(10)?,
        decided_at: r.get(11)?,
    })
}

impl Store {
    pub fn create_suggestion(&self, s: &NewSuggestion<'_>) -> Result<SuggestionRow> {
        let conn = self.conn()?;
        let routines =
            (!s.routines.is_empty()).then(|| ids_to_json(s.routines.iter().map(String::as_str)));
        conn.execute(
            "INSERT INTO channel_suggestions (slug, title, why, routines, run_id, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![s.slug, s.title, s.why, routines, s.run_id, s.session_id, now_ms()],
        )?;
        let id = conn.last_insert_rowid();
        let sql = format!("SELECT {COLS} FROM channel_suggestions WHERE id = ?1");
        Ok(conn.query_row(&sql, [id], row)?)
    }

    pub fn suggestion(&self, id: i64) -> Result<Option<SuggestionRow>> {
        let sql = format!("SELECT {COLS} FROM channel_suggestions WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], row).optional()?)
    }

    /// The suggestion whose card is post `post_id`.
    pub fn suggestion_of_post(&self, post_id: i64) -> Result<Option<SuggestionRow>> {
        let sql = format!("SELECT {COLS} FROM channel_suggestions WHERE post_id = ?1");
        Ok(self.conn()?.query_row(&sql, [post_id], row).optional()?)
    }

    pub fn set_suggestion_post(&self, id: i64, post_id: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE channel_suggestions SET post_id = ?2 WHERE id = ?1",
            params![id, post_id],
        )?;
        Ok(())
    }

    /// Decide a pending suggestion: `accepted` (with the channel created)
    /// or `declined`. False if it was already decided.
    pub fn decide_suggestion(
        &self,
        id: i64,
        state: &str,
        channel_id: Option<&str>,
    ) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE channel_suggestions SET state = ?2, channel_id = ?3, decided_at = ?4
             WHERE id = ?1 AND state = 'pending'",
            params![id, state, channel_id, now_ms()],
        )?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_suggestion_is_decided_once() {
        let s = Store::open_in_memory().unwrap();
        let routines = vec!["sch1".to_owned()];
        let row = s
            .create_suggestion(&NewSuggestion {
                slug: "finances",
                title: "Finances",
                why: "Invoices keep coming up.",
                routines: &routines,
                run_id: Some("r"),
                session_id: Some("s"),
            })
            .unwrap();
        assert_eq!(
            (row.state.as_str(), row.routines.clone()),
            ("pending", routines)
        );
        s.set_suggestion_post(row.id, 7).unwrap();
        assert_eq!(s.suggestion_of_post(7).unwrap().unwrap().id, row.id);
        assert!(s.decide_suggestion(row.id, "accepted", Some("c1")).unwrap());
        assert!(
            !s.decide_suggestion(row.id, "declined", None).unwrap(),
            "once"
        );
        let row = s.suggestion(row.id).unwrap().unwrap();
        assert_eq!(
            (row.state.as_str(), row.channel_id.as_deref()),
            ("accepted", Some("c1"))
        );
        assert!(row.decided_at.is_some());
    }
}
