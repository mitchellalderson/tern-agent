//! Resolving tool path arguments against the workspace.
//!
//! Relative paths resolve inside the workspace and may not escape it (via
//! `..` or symlinks). Absolute paths are allowed; tools that write require
//! approval when the target is outside the workspace.

use std::path::{Component, Path, PathBuf};

use tern_core::home::expand_tilde;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Canonical path (symlinks in existing ancestors resolved).
    pub path: PathBuf,
    pub inside_workspace: bool,
}

/// Paths the file tools must never touch, whatever the approval.
#[derive(Debug, Clone, Default)]
pub struct Guard {
    /// No writes anywhere under these (tern's own data directory).
    pub no_write: Vec<PathBuf>,
    /// No reads of these files (secrets, the database).
    pub no_read: Vec<PathBuf>,
}

impl Guard {
    /// For a data directory: protect all of it from writes, and its
    /// secrets and database from reads.
    pub fn for_data_dir(dir: &Path) -> Self {
        let dir = canonicalize_lenient(dir);
        let no_read = ["secrets.toml", "tern.db", "tern.db-wal", "tern.db-shm"]
            .iter()
            .map(|f| dir.join(f))
            .collect();
        Self {
            no_write: vec![dir],
            no_read,
        }
    }

    /// Whether `path` (canonical) may be read by file tools. Also used per
    /// file by `search_files`, so a directory search can't reach a secret.
    pub fn allows_read(&self, path: &Path) -> bool {
        !self.no_read.iter().any(|p| p == path)
    }

    pub fn check_read(&self, r: &Resolved) -> Result<(), String> {
        if !self.allows_read(&r.path) {
            return Err(format!("{} is off limits to file tools", r.path.display()));
        }
        Ok(())
    }

    pub fn check_write(&self, r: &Resolved) -> Result<(), String> {
        if let Some(p) = self.no_write.iter().find(|p| r.path.starts_with(p)) {
            return Err(format!(
                "{} is inside tern's data directory ({}), which file tools can't modify",
                r.path.display(),
                p.display()
            ));
        }
        // Git reads `.git/config` and hooks as code (`core.fsmonitor`,
        // `core.pager`), so a file tool writing there is a way to run commands.
        if r.path.components().any(|c| c.as_os_str() == ".git") {
            return Err(format!(
                "{} is git metadata; file tools can't modify it",
                r.path.display()
            ));
        }
        Ok(())
    }
}

pub fn resolve(workspace: &Path, raw: &str) -> Result<Resolved, String> {
    if raw.is_empty() {
        return Err("path is empty".into());
    }
    let ws = canonicalize_lenient(workspace);
    let given = expand_tilde(Path::new(raw));
    let relative = given.is_relative();
    let joined = if relative { ws.join(&given) } else { given };
    let normalized = normalize(&joined);
    if relative && !normalized.starts_with(&ws) {
        return Err(format!("{raw:?} escapes the workspace"));
    }
    // A dangling symlink would look like a new file inside the workspace,
    // but writing to it creates its target, wherever that is.
    if std::fs::symlink_metadata(&normalized).is_ok_and(|m| m.file_type().is_symlink())
        && std::fs::metadata(&normalized).is_err()
    {
        return Err(format!(
            "{raw:?} is a symlink to a path that doesn't exist; refusing to follow it"
        ));
    }
    let path = canonicalize_lenient(&normalized);
    let inside_workspace = path.starts_with(&ws);
    if relative && !inside_workspace {
        return Err(format!("{raw:?} resolves outside the workspace (symlink)"));
    }
    Ok(Resolved {
        path,
        inside_workspace,
    })
}

/// Lexically remove `.` and `..` components.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalize the longest existing ancestor and append the rest, so paths
/// that don't exist yet (write targets) still get symlinks resolved.
fn canonicalize_lenient(p: &Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&existing) {
            let mut out = canon;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (
            existing.file_name().map(|n| n.to_os_string()),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_stay_inside() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let r = resolve(ws, "a/b/../c.txt").unwrap();
        assert!(r.inside_workspace);
        assert!(r.path.ends_with("a/c.txt"));
        assert!(resolve(ws, "../outside").is_err());
        assert!(resolve(ws, "a/../../x").is_err());
        assert!(resolve(ws, "").is_err());
    }

    #[test]
    fn absolute_paths_are_classified() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        let inside = resolve(&ws, ws.join("f").to_str().unwrap()).unwrap();
        assert!(inside.inside_workspace);
        let outside = resolve(&ws, tmp.path().join("other").to_str().unwrap()).unwrap();
        assert!(!outside.inside_workspace);
    }

    #[test]
    fn guard_protects_the_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join(".tern");
        std::fs::create_dir(&data).unwrap();
        let g = Guard::for_data_dir(&data);
        let ws = tmp.path();
        let secrets = resolve(ws, data.join("secrets.toml").to_str().unwrap()).unwrap();
        assert!(g.check_read(&secrets).is_err());
        let skill = resolve(ws, data.join("skills/x/SKILL.md").to_str().unwrap()).unwrap();
        assert!(g.check_read(&skill).is_ok(), "skills stay readable");
        assert!(g.check_write(&skill).is_err());
        let connector = resolve(ws, data.join("connectors/x.luau").to_str().unwrap()).unwrap();
        assert!(
            g.check_write(&connector)
                .unwrap_err()
                .contains("data directory")
        );
        assert!(g.check_write(&resolve(ws, "ok.txt").unwrap()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlinks_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("outside/new.txt"), ws.join("x")).unwrap();
        let err = resolve(&ws, "x").unwrap_err();
        assert!(err.contains("doesn't exist"), "{err}");
        assert!(resolve(&ws, ws.join("x").to_str().unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escapes_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        std::os::unix::fs::symlink(tmp.path(), ws.join("link")).unwrap();
        assert!(resolve(&ws, "link/secret").is_err());
    }
}
