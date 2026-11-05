//! Workspaces in the runtime (docs/alpha-workspaces.md §3-§5, §10): the
//! default workspace, adding and checking folders, freezing a run's folder,
//! and what the OS does for commands in each.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use tern_core::models::Change;
use tern_core::workspace::validate::{
    ValidateCtx, Verdict, revalidate, validate_read_root, validate_workspace,
};
use tern_core::workspace::{FsFlags, WorkspaceScope};
use tern_store::{
    Bind, DEFAULT_WORKSPACE, NewWorkspace, WorkspaceRoot, WorkspaceRow, WorkspaceSource,
};

use crate::runtime::Runtime;

/// What a run needs of its folder, decided once when it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frozen {
    pub id: String,
    pub root: PathBuf,
    pub subdir: Option<String>,
}

/// The folder can't be used right now. The run is not started; the user is
/// told why and offered Locate / Choose another / Use default.
#[derive(Debug)]
pub struct WorkspaceError {
    pub name: String,
    /// `missing`, `replaced`, `invalid`, `denied`.
    pub status: String,
    pub detail: String,
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the folder {} is unavailable ({}): {}",
            self.name, self.status, self.detail
        )
    }
}

impl std::error::Error for WorkspaceError {}

/// A folder that can't be added: the words for the user.
#[derive(Debug)]
pub struct Refused(pub Verdict);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.refusal().unwrap_or("that folder can't be used"))
    }
}

impl std::error::Error for Refused {}

/// What the OS does for `shell` in a workspace, as the header chip shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellState {
    /// A verified OS sandbox: commands change files only in the folder.
    Sandboxed,
    /// The folder can't be written, so there is nothing for commands to change.
    ReadOnly,
    /// Docker, or the sandbox not confirmed yet.
    Unverified,
    /// No OS sandbox: a command can read or change anything the user can.
    Unbounded,
}

impl Runtime {
    /// What validation knows of this machine and this tern.
    pub fn validate_ctx(&self) -> ValidateCtx {
        let ctx = ValidateCtx::for_machine((*self.deny).clone());
        // Integration tests put workspaces in temp dirs; set by the harness
        // in the daemon's environment, never by a setting or the agent.
        if std::env::var_os("TERN_TEST_ALLOW_SCRATCH_WORKSPACES").is_some() {
            ctx.allowing_scratch()
        } else {
            ctx
        }
    }

    /// The `default` workspace row, at the configured path. The one folder
    /// tern may create (and recreate) on demand.
    pub fn ensure_default_workspace(&self) -> anyhow::Result<WorkspaceRow> {
        let dir = self.config.workspace_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating workspace {}", dir.display()))?;
        let canon = std::fs::canonicalize(&dir).unwrap_or(dir);
        let (dev, ino) = ident(&canon);
        Ok(self
            .store
            .ensure_default_workspace(&canon.to_string_lossy(), dev, ino)?)
    }

    /// Look a workspace up by id (`default`, `ws_...`) or by its folder: a
    /// path that is already a workspace is that one; a new one is added.
    pub fn resolve_ws_ref(&self, r: &str) -> anyhow::Result<WorkspaceRow> {
        let r = r.trim();
        if r.is_empty() {
            bail!("no workspace given");
        }
        if let Some(w) = self.store.workspace(r)?
            && w.archived_at.is_none()
        {
            return Ok(w);
        }
        if r.starts_with('/') || r.starts_with('~') {
            return Ok(self.add_workspace(r, None)?.0);
        }
        bail!("no workspace {r:?}: give its id or a folder path")
    }

    /// Add `path` as a workspace, if it passes validation; an existing one
    /// for the same folder is returned. The verdict carries any warnings.
    pub fn add_workspace(
        &self,
        path: &str,
        name: Option<&str>,
    ) -> anyhow::Result<(WorkspaceRow, Verdict)> {
        let verdict = validate_workspace(path, &self.validate_ctx());
        if !verdict.is_ok() {
            return Err(Refused(verdict).into());
        }
        let canon = verdict.canonical.clone().unwrap_or_default();
        if let Some(existing) = self.store.workspace_by_path(&canon)? {
            return Ok((existing, verdict));
        }
        let default_name = Path::new(&canon)
            .file_name()
            .map_or_else(|| canon.clone(), |n| n.to_string_lossy().into_owned());
        let id = format!("ws_{}", uuid::Uuid::now_v7().simple());
        let flags = serde_json::to_string(&verdict.flags)?;
        let row = self.store.create_workspace(&NewWorkspace {
            id: &id,
            name: name
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(&default_name),
            path: &canon,
            dev: verdict.dev,
            ino: verdict.ino,
            fs_flags: Some(&flags),
            created_by: "user",
        })?;
        self.changes_send(Change::WorkspacesChanged {});
        Ok((row, verdict))
    }

    /// Add a read-only extra folder or file to `workspace_id`.
    pub fn add_read_root(
        &self,
        workspace_id: &str,
        path: &str,
    ) -> anyhow::Result<(WorkspaceRoot, Verdict)> {
        if self.store.workspace(workspace_id)?.is_none() {
            bail!("no workspace {workspace_id}");
        }
        let verdict = validate_read_root(path, &self.validate_ctx());
        if !verdict.is_ok() {
            return Err(Refused(verdict).into());
        }
        let canon = verdict.canonical.clone().unwrap_or_default();
        let root = WorkspaceRoot {
            workspace_id: workspace_id.to_owned(),
            path: canon,
            dev: verdict.dev,
            ino: verdict.ino,
            kind: "user".into(),
            is_file: verdict.kind == "file",
        };
        self.store.add_workspace_root(&root)?;
        self.changes_send(Change::WorkspacesChanged {});
        Ok((root, verdict))
    }

    fn changes_send(&self, c: Change) {
        let _ = self.changes.send(c);
    }

    /// Re-check one stored workspace now (the check at the start of every
    /// run, and `tern workspace check`): its row gets the status.
    pub fn check_workspace(&self, id: &str) -> Result<Frozen, WorkspaceError> {
        let row = match self.store.workspace(id) {
            Ok(Some(r)) => r,
            _ => {
                return Err(WorkspaceError {
                    name: id.to_owned(),
                    status: "missing".into(),
                    detail: "this folder is no longer known to Tern".into(),
                });
            }
        };
        if id == DEFAULT_WORKSPACE {
            return self.check_default(&row);
        }
        match revalidate(&row.path, row.dev, row.ino, &self.validate_ctx()) {
            Ok(canon) => {
                if row.status != "ok" {
                    let _ = self.store.set_workspace_status(id, "ok", None);
                    self.changes_send(Change::WorkspacesChanged {});
                }
                Ok(Frozen {
                    id: row.id,
                    root: canon,
                    subdir: None,
                })
            }
            Err(u) => {
                if row.status != u.status || row.status_detail.as_deref() != Some(&u.detail) {
                    let _ = self
                        .store
                        .set_workspace_status(id, u.status, Some(&u.detail));
                    self.changes_send(Change::WorkspacesChanged {});
                }
                Err(WorkspaceError {
                    name: row.name,
                    status: u.status.into(),
                    detail: u.detail,
                })
            }
        }
    }

    /// The default is tern's own: created if missing, identity refreshed,
    /// and refused only if it overlaps tern's own state or is `/`.
    fn check_default(&self, row: &WorkspaceRow) -> Result<Frozen, WorkspaceError> {
        let fail = |detail: String, status: &str| WorkspaceError {
            name: row.name.clone(),
            status: status.into(),
            detail,
        };
        let dir = self.config.workspace_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| fail(format!("{}: {e}", dir.display()), "missing"))?;
        let canon = std::fs::canonicalize(&dir)
            .map_err(|e| fail(format!("{}: {e}", dir.display()), "missing"))?;
        if canon == Path::new("/")
            || self.deny.protects_path(&canon)
            || self.deny.dirs().iter().any(|d| d.starts_with(&canon))
        {
            return Err(fail(
                "the default workspace folder overlaps Tern's own data".into(),
                "invalid",
            ));
        }
        let (dev, ino) = ident(&canon);
        if row.path != canon.to_string_lossy()
            || row.dev != dev
            || row.ino != ino
            || row.status != "ok"
        {
            let _ = self
                .store
                .ensure_default_workspace(&canon.to_string_lossy(), dev, ino);
        }
        Ok(Frozen {
            id: row.id.clone(),
            root: canon,
            subdir: None,
        })
    }

    /// The folder a run in `session_id` will have, checked now: the session's
    /// own, else its channel's, else its agent's, else the default. A routine's
    /// own binding (`schedule`) is used as it is: it never falls back.
    pub fn freeze_workspace(
        &self,
        session_id: &str,
        schedule: Option<&str>,
    ) -> anyhow::Result<Frozen> {
        if let Some(sched) = schedule
            && let Some((id, sub)) = self.store.schedule_workspace(sched)?
        {
            let mut f = self.check_workspace(&id)?;
            if let Some(sub) = sub {
                // Still there, still inside.
                WorkspaceScope::open(
                    &f.id,
                    &f.root,
                    Some(Path::new(&sub)),
                    self.deny.clone(),
                    Vec::new(),
                    FsFlags::default(),
                )
                .map_err(|e| WorkspaceError {
                    name: f.id.clone(),
                    status: "missing".into(),
                    detail: format!("the subfolder {sub} isn't available: {}", e.message()),
                })?;
                f.subdir = Some(sub);
            }
            return Ok(f);
        }
        let (id, _src) = self.store.resolve_workspace(session_id)?;
        Ok(self.check_workspace(&id)?)
    }

    /// Where a conversation's folder comes from, for the header chip and `tern
    /// workspace get`.
    pub fn effective_workspace(
        &self,
        session_id: &str,
    ) -> anyhow::Result<(WorkspaceRow, WorkspaceSource)> {
        let (id, src) = self.store.resolve_workspace(session_id)?;
        let row = self
            .store
            .workspace(&id)?
            .context("the workspace is gone")?;
        Ok((row, src))
    }

    /// Bind (or clear, with `None`) the folder of a session, channel or agent.
    pub fn bind_workspace(
        &self,
        target: Bind<'_>,
        ws: Option<&WorkspaceRow>,
    ) -> anyhow::Result<()> {
        let changed = self
            .store
            .bind_workspace(target, ws.map(|w| w.id.as_str()))?;
        if !changed {
            bail!("nothing to bind the folder to");
        }
        self.changes_send(Change::WorkspacesChanged {});
        Ok(())
    }

    /// What the OS does for commands in `path`. `block` waits for the sandbox
    /// check (API reads of one workspace); otherwise an unconfirmed sandbox
    /// reads as `Unverified`.
    pub fn shell_state(&self, path: &Path, writable: bool, block: bool) -> ShellState {
        let backend = crate::runtime::shell_backend(&self.config, &self.home);
        let state = if backend.is_container() {
            ShellState::Unverified
        } else {
            let v = if block {
                backend.verify_now(path)
            } else {
                backend.verify(path)
            };
            if v.is_verified() {
                ShellState::Sandboxed
            } else if v.reason().is_some_and(|r| r.contains("still checking")) {
                ShellState::Unverified
            } else {
                ShellState::Unbounded
            }
        };
        if state == ShellState::Sandboxed && !writable {
            ShellState::ReadOnly
        } else {
            state
        }
    }
}

#[cfg(unix)]
fn ident(path: &Path) -> (Option<i64>, Option<i64>) {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(m) => (Some(m.dev() as i64), Some(m.ino() as i64)),
        Err(_) => (None, None),
    }
}

#[cfg(not(unix))]
fn ident(_: &Path) -> (Option<i64>, Option<i64>) {
    (None, None)
}
