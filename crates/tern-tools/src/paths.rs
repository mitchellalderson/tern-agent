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
    ///
    /// Identity, not spelling, decides: on a case-insensitive or
    /// Unicode-normalizing filesystem `.TERN/SECRETS.TOML` is the same file
    /// as `.tern/secrets.toml` though `canonicalize` keeps the typed case,
    /// and a hardlink is the same file under any name. So the path is also
    /// compared by `(dev, inode)` against the protected files.
    pub fn allows_read(&self, path: &Path) -> bool {
        if self.no_read.iter().any(|p| p == path) {
            return false;
        }
        match identity(path) {
            Some(id) => !self.no_read.iter().any(|p| identity(p) == Some(id)),
            None => true,
        }
    }

    /// Whether `path` is, or is under, a protected directory, by identity
    /// of the path and each of its ancestors.
    fn under_protected_dir(&self, path: &Path) -> Option<&Path> {
        if let Some(p) = self.no_write.iter().find(|p| path.starts_with(p)) {
            return Some(p);
        }
        let dirs: Vec<_> = self
            .no_write
            .iter()
            .filter_map(|d| identity(d).map(|i| (d, i)))
            .collect();
        for anc in path.ancestors() {
            if let Some(id) = identity(anc)
                && let Some((d, _)) = dirs.iter().find(|(_, i)| *i == id)
            {
                return Some(d);
            }
        }
        None
    }

    /// Whether `path` is a second name (hardlink) for a protected file.
    fn is_protected_hardlink(&self, path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let Ok(m) = std::fs::metadata(path) else {
                return false;
            };
            if !m.is_file() || m.nlink() < 2 {
                return false;
            }
            let id = (m.dev(), m.ino());
            if self.no_read.iter().any(|p| identity(p) == Some(id)) {
                return true;
            }
            // Any other file in the data directory (connectors, skills).
            let mut budget = 20_000usize;
            self.no_write
                .iter()
                .any(|d| tree_has_inode(d, id, 6, &mut budget))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            false
        }
    }

    pub fn check_read(&self, r: &Resolved) -> Result<(), String> {
        if !self.allows_read(&r.path) {
            return Err(format!("{} is off limits to file tools", r.path.display()));
        }
        Ok(())
    }

    pub fn check_write(&self, r: &Resolved) -> Result<(), String> {
        if let Some(p) = self.under_protected_dir(&r.path) {
            return Err(format!(
                "{} is inside tern's data directory ({}), which file tools can't modify",
                r.path.display(),
                p.display()
            ));
        }
        // Git reads `.git/config` and hooks as code (`core.fsmonitor`,
        // `core.pager`), so a file tool writing there is a way to run commands.
        if self.is_protected_hardlink(&r.path) {
            return Err(format!(
                "{} is a hardlink to a file in tern's data directory; file tools can't modify it",
                r.path.display()
            ));
        }
        if r.path
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case(".git"))
        {
            return Err(format!(
                "{} is git metadata; file tools can't modify it",
                r.path.display()
            ));
        }
        Ok(())
    }
}

/// `(dev, inode)` of what `path` names right now, following symlinks.
#[cfg(unix)]
fn identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn identity(_path: &Path) -> Option<(u64, u64)> {
    None
}

#[cfg(unix)]
fn tree_has_inode(dir: &Path, id: (u64, u64), depth: u32, budget: &mut usize) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    for e in rd.flatten() {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let Ok(m) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        if m.is_file() && (m.dev(), m.ino()) == id {
            return true;
        }
        if m.is_dir() && depth > 0 && tree_has_inode(&e.path(), id, depth - 1, budget) {
            return true;
        }
    }
    false
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

    fn data_guard() -> (tempfile::TempDir, PathBuf, Guard) {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join(".tern");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("secrets.toml"), "k = 1").unwrap();
        std::fs::create_dir(tmp.path().join("ws")).unwrap();
        let g = Guard::for_data_dir(&data);
        (tmp, data, g)
    }

    fn case_insensitive(p: &Path) -> bool {
        let probe = p.join("CaseProbe");
        std::fs::write(&probe, "").unwrap();
        let r = p.join("caseprobe").exists();
        std::fs::remove_file(&probe).unwrap();
        r
    }

    #[test]
    fn typed_case_variant_is_denied_where_fs_is_case_insensitive() {
        let (tmp, _data, g) = data_guard();
        if !case_insensitive(tmp.path()) {
            return; // case-sensitive fs: another spelling is another file
        }
        let ws = tmp.path().join("ws");
        let typed = tmp.path().join(".TERN/SECRETS.TOML");
        let r = resolve(&ws, typed.to_str().unwrap()).unwrap();
        assert!(g.check_read(&r).is_err(), "{}", r.path.display());
        assert!(g.check_write(&r).is_err());
        let sub = resolve(&ws, tmp.path().join(".Tern/new.txt").to_str().unwrap()).unwrap();
        assert!(g.check_write(&sub).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn hardlink_to_a_protected_file_is_denied() {
        let (tmp, data, g) = data_guard();
        let ws = tmp.path().join("ws");
        std::fs::hard_link(data.join("secrets.toml"), ws.join("innocent.txt")).unwrap();
        let r = resolve(&ws, "innocent.txt").unwrap();
        assert!(g.check_read(&r).is_err());
        assert!(g.check_write(&r).is_err());
        // A hardlink to a non-secret data file stays readable but not writable.
        std::fs::create_dir(data.join("skills")).unwrap();
        std::fs::write(data.join("skills/a.md"), "x").unwrap();
        std::fs::hard_link(data.join("skills/a.md"), ws.join("a.md")).unwrap();
        let r = resolve(&ws, "a.md").unwrap();
        assert!(g.check_read(&r).is_ok());
        assert!(g.check_write(&r).is_err());
        // An unrelated hardlinked file is fine.
        std::fs::write(ws.join("x"), "x").unwrap();
        std::fs::hard_link(ws.join("x"), ws.join("y")).unwrap();
        assert!(g.check_write(&resolve(&ws, "y").unwrap()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_chains_to_protected_files_are_denied() {
        let (tmp, data, g) = data_guard();
        let ws = tmp.path().join("ws");
        std::os::unix::fs::symlink(data.join("secrets.toml"), ws.join("l1")).unwrap();
        std::os::unix::fs::symlink(ws.join("l1"), ws.join("l2")).unwrap();
        let r = resolve(&ws, ws.join("l2").to_str().unwrap()).unwrap();
        assert!(g.check_read(&r).is_err());
        assert!(g.check_write(&r).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unicode_normalization_variants_are_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("d\u{e9}");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("secrets.toml"), "k").unwrap();
        let g = Guard::for_data_dir(&data);
        let nfd = tmp.path().join("de\u{301}/secrets.toml");
        let r = resolve(tmp.path(), nfd.to_str().unwrap()).unwrap();
        assert!(g.check_read(&r).is_err());
        assert!(g.check_write(&r).is_err());
    }

    #[test]
    fn git_metadata_is_matched_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let r = resolve(tmp.path(), ".GIT/config").unwrap();
        assert!(Guard::default().check_write(&r).is_err());
    }
}
