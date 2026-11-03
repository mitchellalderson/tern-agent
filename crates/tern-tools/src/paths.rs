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
