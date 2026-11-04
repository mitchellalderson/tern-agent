//! Connections: several accounts per connector or MCP server (tooling spec
//! §6.8). Credentials are never here; they are secrets named after the
//! connection (`conn/<target>/<slug>/…`).

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store, now_ms};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionRow {
    pub id: String,
    /// `connector` or `mcp`.
    pub kind: String,
    pub target: String,
    pub slug: String,
    pub label: Option<String>,
    pub account: Option<String>,
    /// `oauth`, `key` or `none`.
    pub auth_method: String,
    /// JSON, never a secret.
    pub auth: Option<String>,
    pub is_default: bool,
    /// `ok`, `needs_reauth` or `error`.
    pub status: String,
    pub last_error: Option<String>,
    pub owner_member: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// A connection to add.
#[derive(Debug, Clone, Default)]
pub struct NewConnection {
    pub kind: String,
    pub target: String,
    pub slug: String,
    pub label: Option<String>,
    pub account: Option<String>,
    pub auth_method: String,
    pub auth: Option<String>,
}

const COLS: &str = "id, kind, target, slug, label, account, auth_method, auth, is_default, status, \
                    last_error, owner_member, created_at, last_used_at";

fn row(r: &Row<'_>) -> rusqlite::Result<ConnectionRow> {
    Ok(ConnectionRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        target: r.get(2)?,
        slug: r.get(3)?,
        label: r.get(4)?,
        account: r.get(5)?,
        auth_method: r.get(6)?,
        auth: r.get(7)?,
        is_default: r.get(8)?,
        status: r.get(9)?,
        last_error: r.get(10)?,
        owner_member: r.get(11)?,
        created_at: r.get(12)?,
        last_used_at: r.get(13)?,
    })
}

impl Store {
    /// A target's connections, default first, then by when they were added.
    pub fn connections(&self, kind: &str, target: &str) -> Result<Vec<ConnectionRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLS} FROM connections WHERE kind = ?1 AND target = ?2
             ORDER BY is_default DESC, created_at, slug"
        ))?;
        let rows = stmt.query_map(params![kind, target], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every connection, by kind, target, then default first.
    pub fn all_connections(&self) -> Result<Vec<ConnectionRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLS} FROM connections ORDER BY kind, target, is_default DESC, created_at, slug"
        ))?;
        let rows = stmt.query_map([], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn connection(
        &self,
        kind: &str,
        target: &str,
        slug: &str,
    ) -> Result<Option<ConnectionRow>> {
        Ok(self
            .conn()?
            .query_row(
                &format!(
                    "SELECT {COLS} FROM connections WHERE kind = ?1 AND target = ?2 AND slug = ?3"
                ),
                params![kind, target, slug],
                row,
            )
            .optional()?)
    }

    /// Add a connection. The first one of a target becomes its default.
    pub fn add_connection(&self, c: &NewConnection) -> Result<ConnectionRow> {
        // Readable and unique: a removed and re-added slug gets a new id.
        let id = format!("{}:{}:{}:{}", c.kind, c.target, c.slug, now_ms());
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let first: bool = tx.query_row(
            "SELECT NOT EXISTS (SELECT 1 FROM connections WHERE kind = ?1 AND target = ?2)",
            params![c.kind, c.target],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO connections (id, kind, target, slug, label, account, auth_method, auth,
                                      is_default, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                c.kind,
                c.target,
                c.slug,
                c.label,
                c.account,
                c.auth_method,
                c.auth,
                first,
                now_ms()
            ],
        )?;
        let r = tx.query_row(
            &format!("SELECT {COLS} FROM connections WHERE id = ?1"),
            [&id],
            row,
        )?;
        crate::ledger_repo::append_on(&tx, &connection_entry("connection.add", "added", &r))?;
        tx.commit()?;
        Ok(r)
    }

    /// Update what a sign-in learned: the account, auth details and status.
    pub fn update_connection_auth(
        &self,
        id: &str,
        account: Option<&str>,
        auth: Option<&str>,
    ) -> Result<()> {
        self.conn()?.execute(
            "UPDATE connections SET account = COALESCE(?2, account), auth = COALESCE(?3, auth),
                    status = 'ok', last_error = NULL WHERE id = ?1",
            params![id, account, auth],
        )?;
        Ok(())
    }

    pub fn set_connection_status(&self, id: &str, status: &str, error: Option<&str>) -> Result<()> {
        self.conn()?.execute(
            "UPDATE connections SET status = ?2, last_error = ?3 WHERE id = ?1",
            params![id, status, error],
        )?;
        Ok(())
    }

    pub fn rename_connection(&self, id: &str, label: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE connections SET label = ?2 WHERE id = ?1",
            params![id, label],
        )?;
        Ok(())
    }

    /// Make `id` its target's default.
    pub fn set_default_connection(&self, id: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE connections SET is_default = (id = ?1)
             WHERE (kind, target) = (SELECT kind, target FROM connections WHERE id = ?1)",
            [id],
        )?;
        let r = tx
            .query_row(
                &format!("SELECT {COLS} FROM connections WHERE id = ?1"),
                [id],
                row,
            )
            .optional()?;
        if let Some(r) = r {
            let e = connection_entry("connection.default", "is now the default:", &r);
            crate::ledger_repo::append_on(&tx, &e)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove a connection; if it was the default, the oldest other one
    /// becomes the default.
    pub fn remove_connection(&self, id: &str) -> Result<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let removed = tx
            .query_row(
                &format!("SELECT {COLS} FROM connections WHERE id = ?1"),
                [id],
                row,
            )
            .optional()?;
        let gone: Option<(String, String, bool)> = tx
            .query_row(
                "DELETE FROM connections WHERE id = ?1 RETURNING kind, target, is_default",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some(r) = &removed {
            let e = connection_entry("connection.remove", "removed", r);
            crate::ledger_repo::append_on(&tx, &e)?;
        }
        if let Some((kind, target, true)) = &gone {
            tx.execute(
                "UPDATE connections SET is_default = 1 WHERE id = (
                   SELECT id FROM connections WHERE kind = ?1 AND target = ?2
                   ORDER BY created_at, slug LIMIT 1)",
                params![kind, target],
            )?;
        }
        tx.commit()?;
        Ok(gone.is_some())
    }

    /// Note a use, at most once a minute per connection.
    pub fn touch_connection(&self, id: &str) -> Result<()> {
        let now = now_ms();
        self.conn()?.execute(
            "UPDATE connections SET last_used_at = ?2
             WHERE id = ?1 AND (last_used_at IS NULL OR last_used_at < ?3)",
            params![id, now, now - 60_000],
        )?;
        Ok(())
    }

    /// Delete a whole kv namespace (a removed connection's cache and cursors).
    pub fn kv_clear(&self, namespace: &str) -> Result<usize> {
        Ok(self
            .conn()?
            .execute("DELETE FROM kv WHERE namespace = ?1", [namespace])?)
    }

    /// Delete standing grants whose pattern ends with `suffix` for tools
    /// starting with `tool_prefix` (a removed connection's grants).
    pub fn remove_allowlist_matching(&self, tool_prefix: &str, suffix: &str) -> Result<usize> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT id, tool, pattern FROM approval_allowlist")?;
        let ids: Vec<i64> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .filter_map(std::result::Result::ok)
            .filter(|(_, t, p)| t.starts_with(tool_prefix) && p.ends_with(suffix))
            .map(|(id, _, _)| id)
            .collect();
        if !ids.is_empty() {
            let e = crate::LedgerEntry::new(
                "grant.remove",
                tool_prefix,
                format!(
                    "removed {} grant(s) of {tool_prefix}* ending {suffix}",
                    ids.len()
                ),
            )
            .detail(serde_json::json!({ "suffix": suffix, "count": ids.len() }));
            crate::ledger_repo::append_on(&conn, &e)?;
        }
        for id in &ids {
            conn.execute("DELETE FROM approval_allowlist WHERE id = ?1", [id])?;
        }
        Ok(ids.len())
    }
}

/// `connection.add` and friends: `added gmail · work (me@x.com)`.
fn connection_entry(kind: &str, verb: &str, r: &ConnectionRow) -> crate::LedgerEntry {
    let account = r
        .account
        .as_deref()
        .map(|a| format!(" ({a})"))
        .unwrap_or_default();
    let mut e = crate::LedgerEntry::new(
        kind,
        &r.target,
        format!("{verb} {} · {}{account}", r.target, r.slug),
    )
    .detail(serde_json::json!({
        "kind": r.kind, "slug": r.slug, "account": r.account, "auth_method": r.auth_method,
    }));
    if r.kind == "connector" {
        e = e.connector(&r.target);
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(target: &str, slug: &str) -> NewConnection {
        NewConnection {
            kind: "connector".into(),
            target: target.into(),
            slug: slug.into(),
            auth_method: "oauth".into(),
            ..NewConnection::default()
        }
    }

    #[test]
    fn first_is_default_and_removal_moves_it() {
        let s = Store::open_in_memory().unwrap();
        let work = s.add_connection(&new("gmail", "work")).unwrap();
        let home = s.add_connection(&new("gmail", "home")).unwrap();
        s.add_connection(&new("github", "work")).unwrap();
        assert!(work.is_default && !home.is_default);
        assert_eq!(work.owner_member, "owner");
        assert!(
            s.add_connection(&new("gmail", "work")).is_err(),
            "slugs are unique per target"
        );
        s.set_default_connection(&home.id).unwrap();
        let slugs: Vec<_> = s
            .connections("connector", "gmail")
            .unwrap()
            .into_iter()
            .map(|c| (c.slug, c.is_default))
            .collect();
        assert_eq!(slugs, [("home".into(), true), ("work".into(), false)]);
        assert!(s.remove_connection(&home.id).unwrap());
        let left = s.connections("connector", "gmail").unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].is_default && left[0].slug == "work");
        assert_eq!(
            s.connections("connector", "github").unwrap().len(),
            1,
            "other targets untouched"
        );
        s.set_connection_status(&work.id, "needs_reauth", Some("invalid_grant"))
            .unwrap();
        s.update_connection_auth(&work.id, Some("me@x.com"), None)
            .unwrap();
        let w = s.connection("connector", "gmail", "work").unwrap().unwrap();
        assert_eq!(
            (w.status.as_str(), w.account.as_deref()),
            ("ok", Some("me@x.com"))
        );
    }

    #[test]
    fn kv_and_grants_clear_by_connection() {
        let s = Store::open_in_memory().unwrap();
        s.kv_set("lua:gmail:work", "cursor", "1").unwrap();
        s.kv_set("lua:gmail", "cursor", "2").unwrap();
        assert_eq!(s.kv_clear("lua:gmail:work").unwrap(), 1);
        assert_eq!(
            s.kv_get("lua:gmail", "cursor").unwrap().as_deref(),
            Some("2")
        );
        s.add_allowlist("lua__gmail__read", "host:api.x.com@work")
            .unwrap();
        s.add_allowlist("lua__gmail__read", "host:api.x.com@home")
            .unwrap();
        assert_eq!(
            s.remove_allowlist_matching("lua__gmail__", "@work")
                .unwrap(),
            1
        );
        assert_eq!(
            s.allowlist("lua__gmail__read").unwrap(),
            ["host:api.x.com@home"]
        );
    }
}
