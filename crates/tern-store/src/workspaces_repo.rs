//! Workspaces as data (docs/alpha-workspaces.md §3): the folders, their
//! read-only extra roots, the bindings (session, channel, agent, routine)
//! and the resolution order. Settings live here, never in the folder: the
//! agent can write there.

use rusqlite::{OptionalExtension, Row, params};

use crate::repo::now_ms;
use crate::{Result, Store};

pub const DEFAULT_WORKSPACE: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub id: String,
    pub name: String,
    pub path: String,
    pub dev: Option<i64>,
    pub ino: Option<i64>,
    pub fs_flags: Option<String>,
    /// `ok`, `missing`, `replaced`, `denied`, `invalid`.
    pub status: String,
    pub status_detail: Option<String>,
    pub network: Option<String>,
    pub settings: Option<String>,
    pub created_by: String,
    pub archived_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

const WS_COLS: &str = "id, name, path, dev, ino, fs_flags, status, status_detail, network, settings, \
    created_by, archived_at, created_at, updated_at";

fn ws_row(r: &Row<'_>) -> rusqlite::Result<WorkspaceRow> {
    Ok(WorkspaceRow {
        id: r.get(0)?,
        name: r.get(1)?,
        path: r.get(2)?,
        dev: r.get(3)?,
        ino: r.get(4)?,
        fs_flags: r.get(5)?,
        status: r.get(6)?,
        status_detail: r.get(7)?,
        network: r.get(8)?,
        settings: r.get(9)?,
        created_by: r.get(10)?,
        archived_at: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
    })
}

/// A read-only extra folder (or single file) of a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRoot {
    pub workspace_id: String,
    pub path: String,
    pub dev: Option<i64>,
    pub ino: Option<i64>,
    /// `user` or `git_dir`.
    pub kind: String,
    pub is_file: bool,
}

#[derive(Debug, Clone)]
pub struct NewWorkspace<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub path: &'a str,
    pub dev: Option<i64>,
    pub ino: Option<i64>,
    pub fs_flags: Option<&'a str>,
    pub created_by: &'a str,
}

/// Where a run's folder came from, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSource {
    Session,
    Channel,
    Agent,
    Default,
}

impl WorkspaceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Channel => "channel",
            Self::Agent => "agent",
            Self::Default => "default",
        }
    }
}

/// What uses a workspace (the impact of archiving it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceUse {
    pub channels: Vec<String>,
    pub sessions: u32,
    pub agents: Vec<String>,
    pub schedules: Vec<String>,
}

impl WorkspaceUse {
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
            && self.sessions == 0
            && self.agents.is_empty()
            && self.schedules.is_empty()
    }
}

/// A binding target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bind<'a> {
    Session(&'a str),
    Channel(&'a str),
    Agent(&'a str),
}

impl Store {
    pub fn workspace(&self, id: &str) -> Result<Option<WorkspaceRow>> {
        let sql = format!("SELECT {WS_COLS} FROM workspaces WHERE id = ?1");
        Ok(self.conn()?.query_row(&sql, [id], ws_row).optional()?)
    }

    pub fn workspaces(&self, include_archived: bool) -> Result<Vec<WorkspaceRow>> {
        let sql = format!(
            "SELECT {WS_COLS} FROM workspaces {} ORDER BY (id = 'default') DESC, name, id",
            if include_archived {
                ""
            } else {
                "WHERE archived_at IS NULL"
            }
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], ws_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn workspace_by_path(&self, path: &str) -> Result<Option<WorkspaceRow>> {
        let sql =
            format!("SELECT {WS_COLS} FROM workspaces WHERE path = ?1 AND archived_at IS NULL");
        Ok(self.conn()?.query_row(&sql, [path], ws_row).optional()?)
    }

    pub fn create_workspace(&self, w: &NewWorkspace<'_>) -> Result<WorkspaceRow> {
        let now = now_ms();
        self.conn()?.execute(
            "INSERT INTO workspaces (id, name, path, dev, ino, fs_flags, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![w.id, w.name, w.path, w.dev, w.ino, w.fs_flags, w.created_by, now],
        )?;
        self.workspace(w.id)?
            .ok_or_else(|| crate::StoreError::NotFound(format!("workspace {}", w.id)))
    }

    /// The `default` row: created on first use, and kept at the configured
    /// path (`workspace` in `config.toml`) with its current identity.
    pub fn ensure_default_workspace(
        &self,
        path: &str,
        dev: Option<i64>,
        ino: Option<i64>,
    ) -> Result<WorkspaceRow> {
        let now = now_ms();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO workspaces (id, name, path, dev, ino, status, created_by, created_at, updated_at)
             VALUES ('default', 'Tern workspace', ?1, ?2, ?3, 'ok', 'default', ?4, ?4)
             ON CONFLICT(id) DO UPDATE SET path = ?1, dev = ?2, ino = ?3, status = 'ok',
               status_detail = NULL, archived_at = NULL, updated_at = ?4",
            params![path, dev, ino, now],
        )?;
        drop(conn);
        self.workspace(DEFAULT_WORKSPACE)?
            .ok_or_else(|| crate::StoreError::NotFound("workspace default".into()))
    }

    pub fn rename_workspace(&self, id: &str, name: &str) -> Result<bool> {
        Ok(self.conn()?.execute(
            "UPDATE workspaces SET name = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, name, now_ms()],
        )? > 0)
    }

    pub fn set_workspace_status(&self, id: &str, status: &str, detail: Option<&str>) -> Result<()> {
        self.conn()?.execute(
            "UPDATE workspaces SET status = ?2, status_detail = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, status, detail, now_ms()],
        )?;
        Ok(())
    }

    /// Point a workspace at a new place (Locate, or the identity-matched
    /// rename), back to `ok`.
    pub fn set_workspace_path(
        &self,
        id: &str,
        path: &str,
        dev: Option<i64>,
        ino: Option<i64>,
    ) -> Result<()> {
        self.conn()?.execute(
            "UPDATE workspaces SET path = ?2, dev = ?3, ino = ?4, status = 'ok', status_detail = NULL,
               updated_at = ?5 WHERE id = ?1",
            params![id, path, dev, ino, now_ms()],
        )?;
        Ok(())
    }

    /// `network`: `None` inherits, `Some("off")` is stricter than global.
    pub fn set_workspace_network(&self, id: &str, network: Option<&str>) -> Result<()> {
        self.conn()?.execute(
            "UPDATE workspaces SET network = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, network, now_ms()],
        )?;
        Ok(())
    }

    /// Archive (never delete: history keeps the id). The default can't be.
    pub fn archive_workspace(&self, id: &str) -> Result<bool> {
        if id == DEFAULT_WORKSPACE {
            return Ok(false);
        }
        Ok(self.conn()?.execute(
            "UPDATE workspaces SET archived_at = ?2, updated_at = ?2 WHERE id = ?1 AND archived_at IS NULL",
            params![id, now_ms()],
        )? > 0)
    }

    /// What is bound to `id`.
    pub fn workspace_use(&self, id: &str) -> Result<WorkspaceUse> {
        let conn = self.conn()?;
        let list = |sql: &str| -> rusqlite::Result<Vec<String>> {
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map([id], |r| r.get::<_, String>(0))?;
            rows.collect()
        };
        Ok(WorkspaceUse {
            channels: list(
                "SELECT slug FROM channels WHERE workspace_id = ?1 AND archived_at IS NULL ORDER BY slug",
            )?,
            sessions: conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE workspace_id = ?1",
                [id],
                |r| r.get(0),
            )?,
            agents: list("SELECT id FROM agents WHERE workspace_id = ?1 ORDER BY id")?,
            schedules: list("SELECT id FROM schedules WHERE workspace_id = ?1 ORDER BY id")?,
        })
    }

    /// Move every binding of `from` to `to` (`--reassign-to`).
    pub fn reassign_workspace(&self, from: &str, to: &str) -> Result<()> {
        let conn = self.conn()?;
        for table in ["channels", "sessions", "agents", "schedules"] {
            conn.execute(
                &format!("UPDATE {table} SET workspace_id = ?2 WHERE workspace_id = ?1"),
                params![from, to],
            )?;
        }
        Ok(())
    }

    // ---- read-only roots ----

    pub fn workspace_roots(&self, id: &str) -> Result<Vec<WorkspaceRoot>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT workspace_id, path, dev, ino, kind, is_file FROM workspace_roots
             WHERE workspace_id = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map([id], |r| {
            Ok(WorkspaceRoot {
                workspace_id: r.get(0)?,
                path: r.get(1)?,
                dev: r.get(2)?,
                ino: r.get(3)?,
                kind: r.get(4)?,
                is_file: r.get::<_, i64>(5)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_workspace_root(&self, r: &WorkspaceRoot) -> Result<bool> {
        Ok(self.conn()?.execute(
            "INSERT OR IGNORE INTO workspace_roots (workspace_id, path, dev, ino, kind, is_file)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                r.workspace_id,
                r.path,
                r.dev,
                r.ino,
                r.kind,
                r.is_file as i64
            ],
        )? > 0)
    }

    pub fn remove_workspace_root(&self, id: &str, path: &str) -> Result<bool> {
        Ok(self.conn()?.execute(
            "DELETE FROM workspace_roots WHERE workspace_id = ?1 AND path = ?2",
            params![id, path],
        )? > 0)
    }

    // ---- bindings ----

    /// Bind `target` to workspace `ws` (`None` clears: inherit).
    pub fn bind_workspace(&self, target: Bind<'_>, ws: Option<&str>) -> Result<bool> {
        let (table, id) = match target {
            Bind::Session(i) => ("sessions", i),
            Bind::Channel(i) => ("channels", i),
            Bind::Agent(i) => ("agents", i),
        };
        Ok(self.conn()?.execute(
            &format!("UPDATE {table} SET workspace_id = ?2 WHERE id = ?1"),
            params![id, ws],
        )? > 0)
    }

    /// The workspace `target` is bound to itself (not inherited).
    pub fn bound_workspace(&self, target: Bind<'_>) -> Result<Option<String>> {
        let (table, id) = match target {
            Bind::Session(i) => ("sessions", i),
            Bind::Channel(i) => ("channels", i),
            Bind::Agent(i) => ("agents", i),
        };
        let sql = format!("SELECT workspace_id FROM {table} WHERE id = ?1");
        Ok(self
            .conn()?
            .query_row(&sql, [id], |r| r.get::<_, Option<String>>(0))
            .optional()?
            .flatten())
    }

    /// Which workspace a run in `session_id` gets, and why: the session's own
    /// (a Home topic), else its channel's, else its agent's, else the
    /// default. A binding to an archived workspace is skipped.
    pub fn resolve_workspace(&self, session_id: &str) -> Result<(String, WorkspaceSource)> {
        let Some(session) = self.session(session_id)? else {
            return Ok((DEFAULT_WORKSPACE.into(), WorkspaceSource::Default));
        };
        let live = |id: Option<String>| -> Result<Option<String>> {
            match id {
                Some(id) => Ok(self
                    .workspace(&id)?
                    .filter(|w| w.archived_at.is_none())
                    .map(|w| w.id)),
                None => Ok(None),
            }
        };
        if let Some(id) = live(self.bound_workspace(Bind::Session(session_id))?)? {
            return Ok((id, WorkspaceSource::Session));
        }
        if let Some(c) = session.channel_id.as_deref()
            && let Some(id) = live(self.bound_workspace(Bind::Channel(c))?)?
        {
            return Ok((id, WorkspaceSource::Channel));
        }
        if let Some(id) = live(self.bound_workspace(Bind::Agent(session.agent()))?)? {
            return Ok((id, WorkspaceSource::Agent));
        }
        Ok((DEFAULT_WORKSPACE.into(), WorkspaceSource::Default))
    }

    // ---- runs ----

    /// Freeze the run's workspace (before it is driven).
    pub fn set_run_workspace(
        &self,
        run_id: &str,
        id: &str,
        root: &str,
        subdir: Option<&str>,
    ) -> Result<()> {
        self.conn()?.execute(
            "UPDATE runs SET workspace_id = ?2, workspace_root = ?3, workspace_subdir = ?4 WHERE id = ?1",
            params![run_id, id, root, subdir],
        )?;
        Ok(())
    }

    // ---- routines ----

    /// Bind a routine to a workspace (and the subdirectory it was narrowed to).
    pub fn set_schedule_workspace(
        &self,
        schedule_id: &str,
        ws: Option<&str>,
        subdir: Option<&str>,
    ) -> Result<bool> {
        Ok(self.conn()?.execute(
            "UPDATE schedules SET workspace_id = ?2, workspace_subdir = ?3 WHERE id = ?1",
            params![schedule_id, ws, subdir],
        )? > 0)
    }

    /// A routine created by `run_id` takes that run's workspace and subdir.
    pub fn inherit_schedule_workspace(&self, schedule_id: &str, run_id: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE schedules SET (workspace_id, workspace_subdir) =
               (SELECT workspace_id, workspace_subdir FROM runs WHERE id = ?2)
             WHERE id = ?1",
            params![schedule_id, run_id],
        )?;
        Ok(())
    }

    pub fn schedule_workspace(
        &self,
        schedule_id: &str,
    ) -> Result<Option<(String, Option<String>)>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT workspace_id, workspace_subdir FROM schedules WHERE id = ?1",
                [schedule_id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?
            .and_then(|(w, s)| w.map(|w| (w, s))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn ws<'a>(id: &'a str, path: &'a str) -> NewWorkspace<'a> {
        NewWorkspace {
            id,
            name: id,
            path,
            dev: Some(1),
            ino: Some(2),
            fs_flags: None,
            created_by: "user",
        }
    }

    #[test]
    fn default_workspace_follows_the_configured_path() {
        let s = store();
        let d = s.ensure_default_workspace("/a", Some(1), Some(2)).unwrap();
        assert_eq!(
            (d.id.as_str(), d.path.as_str(), d.created_by.as_str()),
            ("default", "/a", "default")
        );
        let d = s.ensure_default_workspace("/b", Some(1), Some(3)).unwrap();
        assert_eq!(d.path, "/b");
        assert_eq!(s.workspaces(false).unwrap().len(), 1);
        assert!(!s.archive_workspace("default").unwrap());
    }

    #[test]
    fn a_path_has_one_live_workspace() {
        let s = store();
        s.create_workspace(&ws("ws_1", "/p")).unwrap();
        assert!(s.create_workspace(&ws("ws_2", "/p")).is_err());
        assert!(s.archive_workspace("ws_1").unwrap());
        s.create_workspace(&ws("ws_2", "/p")).unwrap();
        assert_eq!(s.workspace_by_path("/p").unwrap().unwrap().id, "ws_2");
    }

    #[test]
    fn roots_add_list_remove() {
        let s = store();
        s.create_workspace(&ws("ws_1", "/p")).unwrap();
        let root = WorkspaceRoot {
            workspace_id: "ws_1".into(),
            path: "/docs".into(),
            dev: None,
            ino: None,
            kind: "user".into(),
            is_file: false,
        };
        assert!(s.add_workspace_root(&root).unwrap());
        assert!(!s.add_workspace_root(&root).unwrap(), "idempotent");
        assert_eq!(s.workspace_roots("ws_1").unwrap(), [root]);
        assert!(s.remove_workspace_root("ws_1", "/docs").unwrap());
        assert!(s.workspace_roots("ws_1").unwrap().is_empty());
    }

    #[test]
    fn resolution_is_session_then_channel_then_agent_then_default() {
        let s = store();
        s.ensure_default_workspace("/d", None, None).unwrap();
        let sess = s.create_session("s1", "cli", None).unwrap();
        let channel = sess.channel_id.clone().unwrap_or_else(|| "home".into());
        for (id, path) in [("ws_a", "/a"), ("ws_b", "/b"), ("ws_c", "/c")] {
            s.create_workspace(&ws(id, path)).unwrap();
        }
        let now = |s: &Store| s.resolve_workspace("s1").unwrap();
        assert_eq!(now(&s), ("default".into(), WorkspaceSource::Default));
        let agent = sess.agent().to_owned();
        assert!(s.bind_workspace(Bind::Agent(&agent), Some("ws_a")).unwrap());
        assert_eq!(now(&s), ("ws_a".into(), WorkspaceSource::Agent));
        assert!(
            s.bind_workspace(Bind::Channel(&channel), Some("ws_b"))
                .unwrap()
        );
        assert_eq!(
            now(&s),
            ("ws_b".into(), WorkspaceSource::Channel),
            "channel beats agent"
        );
        assert!(s.bind_workspace(Bind::Session("s1"), Some("ws_c")).unwrap());
        assert_eq!(now(&s), ("ws_c".into(), WorkspaceSource::Session));
        // An archived binding is skipped, not followed.
        s.archive_workspace("ws_c").unwrap();
        assert_eq!(now(&s), ("ws_b".into(), WorkspaceSource::Channel));
        s.bind_workspace(Bind::Channel(&channel), None).unwrap();
        assert_eq!(now(&s), ("ws_a".into(), WorkspaceSource::Agent));
        let used = s.workspace_use("ws_a").unwrap();
        assert_eq!(used.agents, std::slice::from_ref(&agent));
        s.reassign_workspace("ws_a", "ws_b").unwrap();
        assert_eq!(now(&s), ("ws_b".into(), WorkspaceSource::Agent));
    }

    #[test]
    fn an_unmigrated_looking_install_resolves_to_default() {
        // Nothing bound anywhere: the migration changed no behaviour.
        let s = store();
        s.create_session("s1", "cli", None).unwrap();
        assert_eq!(
            s.resolve_workspace("s1").unwrap(),
            ("default".into(), WorkspaceSource::Default)
        );
        assert_eq!(s.resolve_workspace("nope").unwrap().0, "default");
    }

    #[test]
    fn runs_freeze_their_folder_and_children_and_routines_inherit_it() {
        let s = store();
        s.create_session("s", "cli", Some("mock:m")).unwrap();
        let msg = crate::NewMessage {
            role: "user".into(),
            content_json: "[]".into(),
            text: "hi".into(),
            summary_of: None,
        };
        let run = |id: &str, parent: Option<&str>| {
            s.create_run(
                &crate::NewRun {
                    id,
                    session_id: "s",
                    trigger: "user",
                    model: "mock:m",
                    system_prompt: "sys",
                    parent_run_id: parent,
                    ..crate::NewRun::default()
                },
                &msg,
            )
            .unwrap()
        };
        assert_eq!(run("r0", None).workspace_root, None, "unfrozen until set");
        s.set_run_workspace("r0", "ws_a", "/a", Some("sub"))
            .unwrap();
        let r = s.run("r0").unwrap().unwrap();
        assert_eq!(
            (
                r.workspace_id.as_deref(),
                r.workspace_root.as_deref(),
                r.workspace_subdir.as_deref()
            ),
            (Some("ws_a"), Some("/a"), Some("sub"))
        );
        // A sub-agent takes its parent's.
        let child = run("r1", Some("r0"));
        assert_eq!(child.workspace_root.as_deref(), Some("/a"));
        assert_eq!(child.workspace_subdir.as_deref(), Some("sub"));
        // A routine made by a run keeps that run's folder and subdir.
        s.create_workspace(&ws("ws_a", "/a")).unwrap();
        s.create_session("cron", "cron", None).unwrap();
        s.create_schedule(&crate::ScheduleRow {
            id: "sch".into(),
            cron: "0 8 * * *".into(),
            prompt: "brief".into(),
            session_id: "cron".into(),
            enabled: true,
            next_run_at: 0,
            ..crate::ScheduleRow::default()
        })
        .unwrap();
        assert_eq!(s.schedule_workspace("sch").unwrap(), None);
        s.inherit_schedule_workspace("sch", "r0").unwrap();
        assert_eq!(
            s.schedule_workspace("sch").unwrap(),
            Some(("ws_a".into(), Some("sub".into())))
        );
        assert_eq!(s.workspace_use("ws_a").unwrap().schedules, ["sch"]);
    }

    #[test]
    fn a_new_segment_keeps_the_folder_of_the_one_it_continues() {
        let s = store();
        s.create_workspace(&ws("ws_a", "/a")).unwrap();
        let first = s
            .create_thread(&crate::NewThread {
                id: "t1",
                source: "api",
                channel_id: "home",
                ..crate::NewThread::default()
            })
            .unwrap();
        s.bind_workspace(Bind::Session(&first.id), Some("ws_a"))
            .unwrap();
        s.create_thread(&crate::NewThread {
            id: "t2",
            source: "api",
            channel_id: "home",
            prev_session_id: Some("t1"),
            segment_reason: Some("idle"),
            ..crate::NewThread::default()
        })
        .unwrap();
        assert_eq!(
            s.bound_workspace(Bind::Session("t2")).unwrap().as_deref(),
            Some("ws_a")
        );
    }
}
