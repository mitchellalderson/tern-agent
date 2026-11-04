//! Webhook routines' secrets and deliveries (T1c). The routine itself is a
//! `schedules` row with `trigger_kind = 'webhook'`; its events go through
//! `routine_events` like a watch's, so dedup and run starts are shared.

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store, now_ms};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRow {
    pub routine_id: String,
    /// `hmac` or `bearer`.
    pub auth: String,
    /// `None` once revoked.
    pub secret: Option<String>,
    pub created_at: i64,
    pub rotated_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub last_delivery_at: Option<i64>,
}

const COLS: &str = "routine_id, auth, secret, created_at, rotated_at, revoked_at, last_delivery_at";

fn webhook_row(r: &Row<'_>) -> rusqlite::Result<WebhookRow> {
    Ok(WebhookRow {
        routine_id: r.get(0)?,
        auth: r.get(1)?,
        secret: r.get(2)?,
        created_at: r.get(3)?,
        rotated_at: r.get(4)?,
        revoked_at: r.get(5)?,
        last_delivery_at: r.get(6)?,
    })
}

impl Store {
    /// Give routine `id` a new secret (and auth kind), replacing any old
    /// one at once: a create, a rotation, or turning a revoked hook back on.
    pub fn set_webhook_secret(&self, id: &str, auth: &str, secret: &str) -> Result<()> {
        let conn = self.conn()?;
        let existed: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM routine_webhooks WHERE routine_id = ?1)",
            [id],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO routine_webhooks (routine_id, auth, secret, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(routine_id) DO UPDATE SET auth = ?2, secret = ?3, rotated_at = ?4,
               revoked_at = NULL",
            params![id, auth, secret, now_ms()],
        )?;
        let (kind, verb) = if existed {
            ("webhook.rotate", "rotated the secret of")
        } else {
            ("webhook.create", "created a secret for")
        };
        let e = crate::LedgerEntry::new(kind, id, format!("{verb} webhook routine {id} ({auth})"))
            .routine(id)
            .detail(serde_json::json!({ "auth": auth }));
        crate::ledger_repo::append_on(&conn, &e)?;
        Ok(())
    }

    pub fn webhook(&self, id: &str) -> Result<Option<WebhookRow>> {
        let sql = format!("SELECT {COLS} FROM routine_webhooks WHERE routine_id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], webhook_row).optional()?)
    }

    pub fn webhooks(&self) -> Result<Vec<WebhookRow>> {
        let sql = format!("SELECT {COLS} FROM routine_webhooks ORDER BY created_at");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], webhook_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Forget routine `id`'s secret: its URL answers 404 until rotated.
    /// False if it had none.
    pub fn revoke_webhook(&self, id: &str) -> Result<bool> {
        let conn = self.conn()?;
        let revoked = conn.execute(
            "UPDATE routine_webhooks SET secret = NULL, revoked_at = ?2
             WHERE routine_id = ?1 AND secret IS NOT NULL",
            params![id, now_ms()],
        )? > 0;
        if revoked {
            let e = crate::LedgerEntry::new(
                "webhook.revoke",
                id,
                format!("revoked the secret of webhook routine {id}"),
            )
            .routine(id);
            crate::ledger_repo::append_on(&conn, &e)?;
        }
        Ok(revoked)
    }

    /// Whether routine `id` has seen event `key`.
    pub fn has_routine_event(&self, id: &str, key: &str) -> Result<bool> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT 1 FROM routine_events WHERE routine_id = ?1 AND event_key = ?2",
                params![id, key],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Record one delivery to routine `id` as an event (`pending`, or
    /// `gated` when its gate dropped it), with `check`. False when the key
    /// was seen before: a re-delivery or a replay, which starts nothing.
    pub fn record_webhook_event(
        &self,
        id: &str,
        key: &str,
        payload: &str,
        state: &str,
        check: &str,
    ) -> Result<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let now = now_ms();
        let added = tx.execute(
            "INSERT OR IGNORE INTO routine_events (routine_id, event_key, payload, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, key, payload, state, now],
        )? > 0;
        tx.execute(
            "UPDATE routine_webhooks SET last_delivery_at = ?2 WHERE routine_id = ?1",
            params![id, now],
        )?;
        if added {
            tx.execute(
                "UPDATE schedules SET last_check = ?2, last_check_at = ?3 WHERE id = ?1",
                params![id, check, now],
            )?;
        }
        tx.commit()?;
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use crate::{ScheduleRow, Store};

    fn routine(store: &Store) {
        store.create_session("s", "cron", None).unwrap();
        store
            .create_schedule(&ScheduleRow {
                id: "r".into(),
                prompt: "p".into(),
                session_id: "s".into(),
                enabled: true,
                next_run_at: i64::MAX,
                trigger: "webhook".into(),
                ..ScheduleRow::default()
            })
            .unwrap();
    }

    #[test]
    fn secrets_rotate_and_revoke() {
        let store = Store::open_in_memory().unwrap();
        routine(&store);
        assert_eq!(store.webhook("r").unwrap(), None);
        store.set_webhook_secret("r", "hmac", "one").unwrap();
        let w = store.webhook("r").unwrap().unwrap();
        assert_eq!((w.secret.as_deref(), w.rotated_at), (Some("one"), None));
        store.set_webhook_secret("r", "bearer", "two").unwrap();
        let w = store.webhook("r").unwrap().unwrap();
        assert_eq!(
            (w.auth.as_str(), w.secret.as_deref()),
            ("bearer", Some("two"))
        );
        assert!(w.rotated_at.is_some());
        assert!(store.revoke_webhook("r").unwrap());
        assert!(!store.revoke_webhook("r").unwrap());
        let w = store.webhook("r").unwrap().unwrap();
        assert_eq!(w.secret, None);
        assert!(w.revoked_at.is_some());
        // Deleting the routine takes its secret with it.
        store.set_webhook_secret("r", "hmac", "three").unwrap();
        assert!(store.delete_schedule("r").unwrap());
        assert!(store.webhooks().unwrap().is_empty());
    }

    #[test]
    fn deliveries_are_deduplicated_by_key() {
        let store = Store::open_in_memory().unwrap();
        routine(&store);
        store.set_webhook_secret("r", "hmac", "k").unwrap();
        assert!(
            store
                .record_webhook_event("r", "h:1", "{}", "pending", "1 delivery")
                .unwrap()
        );
        assert!(
            !store
                .record_webhook_event("r", "h:1", "{}", "pending", "again")
                .unwrap()
        );
        assert!(
            store
                .record_webhook_event("r", "h:2", "", "gated", "gate: dropped")
                .unwrap()
        );
        assert!(store.has_routine_event("r", "h:2").unwrap());
        assert!(!store.has_routine_event("r", "h:3").unwrap());
        let s = store.schedule("r").unwrap().unwrap();
        assert_eq!(s.last_check.as_deref(), Some("gate: dropped"));
        assert_eq!(store.routines_with_waiting_events().unwrap(), vec!["r"]);
        assert_eq!(store.next_routine_events("r").unwrap()[0].key, "h:1");
        assert!(
            store
                .webhook("r")
                .unwrap()
                .unwrap()
                .last_delivery_at
                .is_some()
        );
    }
}
