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
    /// Where the daemon's API socket appears: `tern.sock` in the data
    /// directory (possibly a symlink; see [`TernHome::socket_bind_path`]).
    pub fn socket_path(&self) -> PathBuf {
        self.root.join("tern.sock")
    }

    /// Where the socket actually lives. Unix socket paths are limited to
    /// about 104 bytes (macOS) or 108 (Linux), so a long data directory gets
    /// its socket in a private `/tmp/tern-<uid>/` directory instead, with
    /// `tern.sock` as a symlink to it.
    pub fn socket_bind_path(&self) -> PathBuf {
        let p = self.socket_path();
        if p.as_os_str().len() <= MAX_SOCKET_PATH {
            return p;
        }
        // Stable per data directory, so restarts reuse the same path.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in self.root.as_os_str().as_encoded_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
        short_socket_dir().join(format!("{:016x}.sock", h))
    }

    /// The path clients should connect to: the symlink's target if
    /// `tern.sock` itself is too long to use.
    pub fn socket_connect_path(&self) -> PathBuf {
        let p = self.socket_path();
        if p.as_os_str().len() <= MAX_SOCKET_PATH {
            return p;
        }
        std::fs::read_link(&p).unwrap_or(p)
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

/// Longest socket path we bind directly (below both platforms' limits).
const MAX_SOCKET_PATH: usize = 100;

#[cfg(unix)]
fn short_socket_dir() -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    // Owner uid of the user's home, without libc: the daemon runs as them.
    let uid = user_home()
        .and_then(|h| std::fs::metadata(h).ok())
        .map_or(0, |m| m.uid());
    PathBuf::from(format!("/tmp/tern-{uid}"))
}

#[cfg(not(unix))]
fn short_socket_dir() -> PathBuf {
    std::env::temp_dir().join("tern")
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
    fn long_homes_get_a_short_socket_path() {
        let short = TernHome::new("/x/.tern");
        assert_eq!(short.socket_bind_path(), short.socket_path());
        let long = TernHome::new(format!("/{}/.tern", "d".repeat(120)));
        let bind = long.socket_bind_path();
        assert!(
            bind.as_os_str().len() <= MAX_SOCKET_PATH,
            "{}",
            bind.display()
        );
        assert_eq!(bind, long.socket_bind_path(), "stable");
        assert_ne!(
            bind,
            TernHome::new(format!("/{}/.tern", "e".repeat(120))).socket_bind_path()
        );
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
