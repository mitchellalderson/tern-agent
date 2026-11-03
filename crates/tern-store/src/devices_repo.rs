//! Paired remote devices and their one-time pairing codes.

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store, now_ms};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: String,
    pub name: String,
    pub platform: Option<String>,
    /// `owner`, `chat` or `read`.
    pub scope: String,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
    pub last_ip: Option<String>,
    pub revoked_at: Option<i64>,
}

/// What a device pairing code grants when it's redeemed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceGrant {
    pub scope: String,
    pub name: Option<String>,
}

const DEVICE_COLS: &str =
    "id, name, platform, scope, created_at, last_seen_at, last_ip, revoked_at";

fn device_row(r: &Row<'_>) -> rusqlite::Result<DeviceRow> {
    Ok(DeviceRow {
        id: r.get(0)?,
        name: r.get(1)?,
        platform: r.get(2)?,
        scope: r.get(3)?,
        created_at: r.get(4)?,
        last_seen_at: r.get(5)?,
        last_ip: r.get(6)?,
        revoked_at: r.get(7)?,
    })
}

/// `last_seen_at` is written at most this often per device.
const SEEN_EVERY_MS: i64 = 60_000;

impl Store {
    pub fn create_device_pairing_code(
        &self,
        code: &str,
        grant: &DeviceGrant,
        ttl_ms: i64,
    ) -> Result<()> {
        let now = now_ms();
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM device_pairing_codes WHERE expires_at < ?1",
            [now],
        )?;
        conn.execute(
            "INSERT INTO device_pairing_codes (code, scope, name, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![code, grant.scope, grant.name, now + ttl_ms],
        )?;
        Ok(())
    }

    /// Consume a device pairing code, returning what it grants if it existed
    /// and had not expired.
    pub fn consume_device_pairing_code(&self, code: &str) -> Result<Option<DeviceGrant>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "DELETE FROM device_pairing_codes WHERE code = ?1 AND expires_at >= ?2
                 RETURNING scope, name",
                params![code, now_ms()],
                |r| {
                    Ok(DeviceGrant {
                        scope: r.get(0)?,
                        name: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn create_device(&self, d: &DeviceRow, token_hash: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO devices (id, name, platform, scope, token_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![d.id, d.name, d.platform, d.scope, token_hash, d.created_at],
        )?;
        Ok(())
    }

    /// The device holding this token, if it isn't revoked.
    pub fn device_by_token_hash(&self, token_hash: &str) -> Result<Option<DeviceRow>> {
        let sql = format!(
            "SELECT {DEVICE_COLS} FROM devices WHERE token_hash = ?1 AND revoked_at IS NULL"
        );
        Ok(self
            .conn()?
            .query_row(&sql, [token_hash], device_row)
            .optional()?)
    }

    pub fn device(&self, id: &str) -> Result<Option<DeviceRow>> {
        let sql = format!("SELECT {DEVICE_COLS} FROM devices WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], device_row).optional()?)
    }

    /// Every device, newest first, revoked ones included.
    pub fn devices(&self) -> Result<Vec<DeviceRow>> {
        let sql = format!("SELECT {DEVICE_COLS} FROM devices ORDER BY created_at DESC");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], device_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record that a device was seen, at most once a minute.
    pub fn touch_device(&self, id: &str, ip: Option<&str>) -> Result<()> {
        let now = now_ms();
        self.conn()?.execute(
            "UPDATE devices SET last_seen_at = ?2, last_ip = ?3
             WHERE id = ?1 AND (last_seen_at IS NULL OR last_seen_at < ?4)",
            params![id, now, ip, now - SEEN_EVERY_MS],
        )?;
        Ok(())
    }

    /// Revoke a device: true if it existed and wasn't already revoked.
    pub fn revoke_device(&self, id: &str) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE devices SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![id, now_ms()],
        )?;
        Ok(n > 0)
    }

    pub fn rename_device(&self, id: &str, name: &str) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE devices SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(scope: &str) -> DeviceGrant {
        DeviceGrant {
            scope: scope.into(),
            name: Some("phone".into()),
        }
    }

    #[test]
    fn device_codes_are_single_use_expire_and_carry_the_grant() {
        let s = Store::open_in_memory().unwrap();
        s.create_device_pairing_code("ABC", &grant("chat"), 60_000)
            .unwrap();
        s.create_device_pairing_code("OLD", &grant("owner"), -1)
            .unwrap();
        assert_eq!(
            s.consume_device_pairing_code("ABC").unwrap(),
            Some(grant("chat"))
        );
        assert_eq!(s.consume_device_pairing_code("ABC").unwrap(), None);
        assert_eq!(s.consume_device_pairing_code("OLD").unwrap(), None);
        // Telegram codes and device codes don't mix.
        s.create_pairing_code("TG", 60_000).unwrap();
        assert_eq!(s.consume_device_pairing_code("TG").unwrap(), None);
    }

    #[test]
    fn devices_lookup_touch_rename_and_revoke() {
        let s = Store::open_in_memory().unwrap();
        let d = DeviceRow {
            id: "dev1".into(),
            name: "phone".into(),
            platform: Some("ios".into()),
            scope: "chat".into(),
            created_at: now_ms(),
            last_seen_at: None,
            last_ip: None,
            revoked_at: None,
        };
        s.create_device(&d, "hash1").unwrap();
        assert_eq!(s.device_by_token_hash("hash1").unwrap(), Some(d.clone()));
        assert_eq!(s.device_by_token_hash("nope").unwrap(), None);

        s.touch_device("dev1", Some("10.0.0.2")).unwrap();
        let seen = s.device("dev1").unwrap().unwrap();
        assert_eq!(seen.last_ip.as_deref(), Some("10.0.0.2"));
        assert!(seen.last_seen_at.is_some());

        assert!(s.rename_device("dev1", "work phone").unwrap());
        assert!(s.revoke_device("dev1").unwrap());
        assert!(!s.revoke_device("dev1").unwrap());
        assert_eq!(s.device_by_token_hash("hash1").unwrap(), None);
        let all = s.devices().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "work phone");
        assert!(all[0].revoked_at.is_some());
    }
}
