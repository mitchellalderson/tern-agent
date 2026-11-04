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
    /// Core facts the agent keeps (spec §7), injected like `user.md`.
    pub fn memory_path(&self) -> PathBuf {
        self.root.join("memory.md")
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
    ///
    /// A symlinked socket is only followed if it and its directory belong
    /// to us (see [`verify_private_socket`]).
    pub fn socket_connect_path(&self) -> Result<PathBuf, String> {
        let p = self.socket_path();
        if p.as_os_str().len() <= MAX_SOCKET_PATH {
            return Ok(p);
        }
        let target = std::fs::read_link(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        #[cfg(unix)]
        verify_private_socket(&target)?;
        Ok(target)
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
        self.seed_skills()
    }

    /// Install tern's own skills once each. `.installed` remembers which
    /// were installed, so a skill the user deleted doesn't come back.
    fn seed_skills(&self) -> io::Result<()> {
        let marker = self.skills_dir().join(".installed");
        let done = std::fs::read_to_string(&marker).unwrap_or_default();
        let mut add = String::new();
        for (name, text) in crate::defaults::SKILLS {
            if done.lines().any(|l| l == *name) {
                continue;
            }
            let dir = self.skills_dir().join(name);
            if !dir.exists() {
                std::fs::create_dir_all(&dir)?;
                std::fs::write(dir.join("SKILL.md"), text)?;
            }
            add.push_str(name);
            add.push('\n');
        }
        if !add.is_empty() {
            std::fs::write(&marker, format!("{done}{add}"))?;
        }
        Ok(())
    }
}

/// Longest socket path we bind directly (below both platforms' limits).
const MAX_SOCKET_PATH: usize = 100;

/// A private directory for the socket when the data directory's path is too
/// long: the per-user runtime dir (Linux), the per-user `$TMPDIR` (macOS),
/// or `/tmp/tern-<uid>` (created 0700 and checked before use).
#[cfg(unix)]
fn short_socket_dir() -> PathBuf {
    for var in ["XDG_RUNTIME_DIR", "TMPDIR"] {
        if let Some(dir) = std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|d| d.is_absolute())
        {
            let dir = dir.join("tern");
            // Room for "/<16 hex>.sock".
            if dir.as_os_str().len() + 22 <= MAX_SOCKET_PATH && is_private_dir_or_creatable(&dir) {
                return dir;
            }
        }
    }
    PathBuf::from(format!("/tmp/tern-{}", current_uid().unwrap_or(0)))
}

/// A per-user parent (mode 0700 or owned by us) under which `dir` can live.
#[cfg(unix)]
fn is_private_dir_or_creatable(dir: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Some(parent) = dir.parent() else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(parent) else {
        return false;
    };
    Some(meta.uid()) == current_uid() && meta.permissions().mode() & 0o077 == 0
}

/// The effective uid of this process, without libc: the owner of a file we
/// just created.
#[cfg(unix)]
pub fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    let probe = std::env::temp_dir().join(format!(
        ".tern-uid-{}-{}",
        std::process::id(),
        crate::agent::new_id()
    ));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .ok()?;
    let uid = file.metadata().ok().map(|m| m.uid());
    drop(file);
    let _ = std::fs::remove_file(&probe);
    uid
}

/// Check that a socket and its directory belong to us and that the
/// directory is private, so we never talk to (or approve things for) a
/// socket another local user planted.
#[cfg(unix)]
pub fn verify_private_socket(socket: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let me = current_uid().ok_or("cannot determine the current user")?;
    let dir = socket.parent().ok_or("socket has no directory")?;
    let d = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !d.is_dir() || d.uid() != me || d.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "{} must be a directory owned by you with mode 0700",
            dir.display()
        ));
    }
    let f = std::fs::symlink_metadata(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    if f.uid() != me {
        return Err(format!("{} is not owned by you", socket.display()));
    }
    Ok(())
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

    #[cfg(unix)]
    #[test]
    fn foreign_or_open_socket_dirs_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sock");
        std::fs::create_dir(&dir).unwrap();
        let sock = dir.join("x.sock");
        std::fs::write(&sock, "").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            verify_private_socket(&sock)
                .unwrap_err()
                .contains("mode 0700")
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(verify_private_socket(&sock).is_ok());
        assert!(current_uid().is_some());
    }

    #[test]
    fn ensure_layout_creates_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = TernHome::new(tmp.path().join("h"));
        home.ensure_layout().unwrap();
        assert!(home.drafts_dir().is_dir());
        assert!(home.logs_dir().is_dir());
        assert!(home.skills_dir().is_dir());
        // tern's own skills are installed once; one the user deleted stays
        // deleted.
        let skill = home.skills_dir().join("connect-a-service");
        let found = crate::skills::discover(&home.skills_dir());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "connect-a-service");
        assert!(found[0].description.contains("OAuth first"));
        std::fs::remove_dir_all(&skill).unwrap();
        home.ensure_layout().unwrap();
        assert!(!skill.exists());
        let found = crate::skills::discover(&home.skills_dir());
        assert!(found.is_empty(), "the marker file isn't a skill");
    }
}
