//! Location and layout of the tern data directory (`~/.tern`, or `$TERN_HOME`).

use std::io;
use std::path::{Path, PathBuf};

pub const HOME_ENV: &str = "TERN_HOME";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TernHome {
    root: PathBuf,
}

impl TernHome {
    /// Resolve from `$TERN_HOME`, falling back to `~/.tern`.
    pub fn resolve() -> io::Result<Self> {
        if let Some(dir) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::new(expand_tilde(Path::new(&dir))));
        }
        let home = user_home().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "cannot determine home directory; set TERN_HOME",
            )
        })?;
        Ok(Self::new(home.join(".tern")))
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }
    pub fn secrets_path(&self) -> PathBuf {
        self.root.join("secrets.toml")
    }
    pub fn db_path(&self) -> PathBuf {
        self.root.join("tern.db")
    }
    pub fn persona_path(&self) -> PathBuf {
        self.root.join("persona.md")
    }
    pub fn user_path(&self) -> PathBuf {
        self.root.join("user.md")
    }
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }
    pub fn connectors_dir(&self) -> PathBuf {
        self.root.join("connectors")
    }
    pub fn drafts_dir(&self) -> PathBuf {
        self.connectors_dir().join("drafts")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn socket_path(&self) -> PathBuf {
        self.root.join("tern.sock")
    }
    pub fn pid_path(&self) -> PathBuf {
        self.root.join("tern.pid")
    }

    /// Create the directory skeleton. Never touches existing files.
    pub fn ensure_layout(&self) -> io::Result<()> {
        for dir in [
            self.root.clone(),
            self.skills_dir(),
            self.drafts_dir(),
            self.logs_dir(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

/// The current user's home directory, from `$HOME` (or `%USERPROFILE%`).
pub fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Expand a leading `~` or `~/` to the user's home directory.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    match user_home() {
        Some(home) => home.join(rest),
        None => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_paths() {
        let home = TernHome::new("/x/.tern");
        assert_eq!(home.db_path(), Path::new("/x/.tern/tern.db"));
        assert_eq!(home.drafts_dir(), Path::new("/x/.tern/connectors/drafts"));
    }

    #[test]
    fn tilde_expansion_leaves_other_paths_alone() {
        assert_eq!(expand_tilde(Path::new("/abs/p")), Path::new("/abs/p"));
        assert_eq!(expand_tilde(Path::new("rel/~p")), Path::new("rel/~p"));
        if let Some(h) = user_home() {
            assert_eq!(expand_tilde(Path::new("~/w")), h.join("w"));
            assert_eq!(expand_tilde(Path::new("~")), h);
        }
    }

    #[test]
    fn ensure_layout_creates_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = TernHome::new(tmp.path().join("h"));
        home.ensure_layout().unwrap();
        assert!(home.drafts_dir().is_dir());
        assert!(home.logs_dir().is_dir());
        assert!(home.skills_dir().is_dir());
    }
}
