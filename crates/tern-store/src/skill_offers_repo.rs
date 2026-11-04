//! Skill capture offers (T5b): a long, clean run tern offers to save as a
//! skill. One row per run; the user accepts or dismisses it.

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store, now_ms};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillOfferRow {
    pub id: i64,
    pub run_id: String,
    pub session_id: String,
    pub tool_calls: u32,
    pub title: Option<String>,
    /// `pending`, `accepted` or `dismissed`.
    pub state: String,
    /// The draft's name once accepted.
    pub skill: Option<String>,
    pub created_at: i64,
    pub decided_at: Option<i64>,
}

const COLS: &str =
    "id, run_id, session_id, tool_calls, title, state, skill, created_at, decided_at";

fn row(r: &Row<'_>) -> rusqlite::Result<SkillOfferRow> {
    Ok(SkillOfferRow {
        id: r.get(0)?,
        run_id: r.get(1)?,
        session_id: r.get(2)?,
        tool_calls: r.get(3)?,
        title: r.get(4)?,
        state: r.get(5)?,
        skill: r.get(6)?,
        created_at: r.get(7)?,
        decided_at: r.get(8)?,
    })
}

impl Store {
    /// Offer `run_id` as a skill. `None` if it was offered before (whatever
    /// became of that offer) or its conversation already has an offer
    /// waiting, so a long conversation isn't asked again and again.
    pub fn create_skill_offer(
        &self,
        run_id: &str,
        session_id: &str,
        tool_calls: u32,
        title: Option<&str>,
    ) -> Result<Option<SkillOfferRow>> {
        let conn = self.conn()?;
        let waiting: i64 = conn.query_row(
            "SELECT count(*) FROM skill_offers WHERE session_id = ?1 AND state = 'pending'",
            [session_id],
            |r| r.get(0),
        )?;
        if waiting > 0 {
            return Ok(None);
        }
        let n = conn.execute(
            "INSERT OR IGNORE INTO skill_offers (run_id, session_id, tool_calls, title, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, session_id, tool_calls, title, now_ms()],
        )?;
        if n == 0 {
            return Ok(None);
        }
        let sql = format!("SELECT {COLS} FROM skill_offers WHERE id = ?1");
        Ok(Some(conn.query_row(
            &sql,
            [conn.last_insert_rowid()],
            row,
        )?))
    }

    pub fn skill_offer(&self, id: i64) -> Result<Option<SkillOfferRow>> {
        let sql = format!("SELECT {COLS} FROM skill_offers WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], row).optional()?)
    }

    /// Offers, newest first. `state` filters (`pending`, ...); pending ones
    /// older than `not_before` (ms) have lapsed and are left out.
    pub fn skill_offers(&self, state: Option<&str>, not_before: i64) -> Result<Vec<SkillOfferRow>> {
        let sql = format!(
            "SELECT {COLS} FROM skill_offers
             WHERE (?1 IS NULL OR state = ?1) AND (state != 'pending' OR created_at >= ?2)
             ORDER BY id DESC LIMIT 200"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![state, not_before], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Decide a pending offer: `accepted` (with the draft's name) or
    /// `dismissed`. False if it was already decided.
    pub fn decide_skill_offer(&self, id: i64, state: &str, skill: Option<&str>) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE skill_offers SET state = ?2, skill = ?3, decided_at = ?4
             WHERE id = ?1 AND state = 'pending'",
            params![id, state, skill, now_ms()],
        )?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn seed(s: &Store, session: &str) {
        s.conn()
            .unwrap()
            .execute(
                "INSERT INTO sessions (id, source, created_at, updated_at) VALUES (?1, 'api', 0, 0)",
                [session],
            )
            .unwrap();
    }

    #[test]
    fn a_run_is_offered_once_and_decided_once() {
        let s = Store::open_in_memory().unwrap();
        seed(&s, "s1");
        let o = s
            .create_skill_offer("r1", "s1", 9, Some("Weekly digest"))
            .unwrap()
            .unwrap();
        assert_eq!((o.state.as_str(), o.tool_calls), ("pending", 9));
        // Same run again, and another run while one is waiting: no offer.
        assert!(s.create_skill_offer("r1", "s1", 9, None).unwrap().is_none());
        assert!(
            s.create_skill_offer("r2", "s1", 12, None)
                .unwrap()
                .is_none()
        );
        assert!(
            s.decide_skill_offer(o.id, "accepted", Some("digest"))
                .unwrap()
        );
        assert!(!s.decide_skill_offer(o.id, "dismissed", None).unwrap());
        let o = s.skill_offer(o.id).unwrap().unwrap();
        assert_eq!(
            (o.state.as_str(), o.skill.as_deref()),
            ("accepted", Some("digest"))
        );
        assert!(o.decided_at.is_some());
        // Decided, a later run in the conversation can be offered; the
        // decided run never is.
        assert!(s.create_skill_offer("r1", "s1", 9, None).unwrap().is_none());
        assert!(
            s.create_skill_offer("r2", "s1", 12, None)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn pending_offers_lapse_but_decided_ones_stay() {
        let s = Store::open_in_memory().unwrap();
        seed(&s, "s1");
        seed(&s, "s2");
        let a = s.create_skill_offer("r1", "s1", 9, None).unwrap().unwrap();
        s.create_skill_offer("r2", "s2", 9, None).unwrap().unwrap();
        s.decide_skill_offer(a.id, "dismissed", None).unwrap();
        let far = now_ms() + 10_000;
        let listed = s.skill_offers(None, far).unwrap();
        assert_eq!(listed.len(), 1, "the pending one lapsed");
        assert_eq!(listed[0].state, "dismissed");
        assert_eq!(s.skill_offers(Some("pending"), 0).unwrap().len(), 1);
    }
}
