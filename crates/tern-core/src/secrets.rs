//! Secret resolution: environment variables first, then `secrets.toml`.
//!
//! Secret values never appear in `Debug` output. `secrets.toml` must not be
//! readable by group or others; loading refuses looser permissions.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("{path} has permissions {mode:o}; it must be 0600 (run `chmod 600 {path}`)")]
    Permissions { path: PathBuf, mode: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSource {
    Env,
    File,
}

#[derive(Default, Clone)]
pub struct Secrets {
    file: BTreeMap<String, String>,
    /// Every value handed out by [`Secrets::get`], shared by all clones, so
    /// redaction covers secrets first used after startup (env-only ones a
    /// connector or MCP server reads later).
    seen: Arc<Mutex<BTreeSet<String>>>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("file_keys", &self.file.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Secrets {
    /// Load `secrets.toml` from `path`. A missing file is not an error.
    pub fn load(path: &Path) -> Result<Self, SecretError> {
        let io_err = |source| SecretError::Io {
            path: path.to_path_buf(),
            source,
        };
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(io_err(e)),
        };
        check_permissions(path, &meta)?;
        let text = std::fs::read_to_string(path).map_err(io_err)?;
        Self::parse(&text, path)
    }

    fn parse(text: &str, path: &Path) -> Result<Self, SecretError> {
        let table: toml::Table = toml::from_str(text).map_err(|e| SecretError::Parse {
            path: path.to_path_buf(),
            // toml errors quote the offending line, which may contain a secret.
            message: e.message().to_owned(),
        })?;
        let mut file = BTreeMap::new();
        for (k, v) in table {
            match v {
                toml::Value::String(s) => {
                    file.insert(k, s);
                }
                _ => {
                    return Err(SecretError::Parse {
                        path: path.to_path_buf(),
                        message: format!("value of {k:?} must be a string"),
                    });
                }
            }
        }
        Ok(Self {
            file,
            seen: Arc::default(),
        })
    }

    pub fn get(&self, name: &str) -> Option<String> {
        let value = self.lookup(name).map(|(v, _)| v)?;
        if let Ok(mut seen) = self.seen.lock() {
            seen.insert(value.clone());
        }
        Some(value)
    }

    /// Values to scrub from anything persisted or logged: everything in
    /// `secrets.toml` plus every secret read so far. Longest first, so a
    /// secret containing another is replaced whole. Very short values are
    /// skipped to avoid redacting common substrings.
    pub fn redaction_values(&self) -> Vec<String> {
        let mut all: BTreeSet<String> = self.file.values().cloned().collect();
        if let Ok(seen) = self.seen.lock() {
            all.extend(seen.iter().cloned());
        }
        let mut v: Vec<String> = all.into_iter().filter(|s| s.len() >= 4).collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.len()));
        v
    }

    /// `text` with every known secret value replaced by `[REDACTED]`.
    pub fn redact(&self, text: &str) -> String {
        redact_with(&self.redaction_values(), text)
    }

    /// Every value in `secrets.toml` (for redaction).
    pub fn file_values(&self) -> Vec<String> {
        self.file.values().cloned().collect()
    }

    pub fn source(&self, name: &str) -> Option<SecretSource> {
        self.lookup(name).map(|(_, s)| s)
    }

    fn lookup(&self, name: &str) -> Option<(String, SecretSource)> {
        if let Some(v) = std::env::var(name).ok().filter(|v| !v.is_empty()) {
            return Some((v, SecretSource::Env));
        }
        self.file.get(name).map(|v| (v.clone(), SecretSource::File))
    }
}

pub const REDACTED: &str = "[REDACTED]";

/// Replace each of `values` (longest first) in `text`.
pub fn redact_with(values: &[String], text: &str) -> String {
    let mut out = text.to_owned();
    for v in values {
        if out.contains(v.as_str()) {
            out = out.replace(v.as_str(), REDACTED);
        }
    }
    out
}

#[cfg(unix)]
fn check_permissions(path: &Path, meta: &std::fs::Metadata) -> Result<(), SecretError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(SecretError::Permissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_path: &Path, _meta: &std::fs::Metadata) -> Result<(), SecretError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str, mode: u32) -> PathBuf {
        let p = dir.join("secrets.toml");
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
        p
    }

    #[test]
    fn values_read_later_are_redacted_by_every_clone() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "FILE_ONE = \"file-secret-1\"\n", 0o600);
        let s = Secrets::load(&p).unwrap();
        let clone = s.clone();
        assert_eq!(
            s.redact("x file-secret-1 y"),
            "x [REDACTED] y",
            "file values always"
        );
        // An env-only secret becomes known to every clone once read.
        let path_value = std::env::var("PATH").unwrap();
        assert!(!clone.redact(&path_value).contains("[REDACTED]"));
        assert_eq!(s.get("PATH").as_deref(), Some(path_value.as_str()));
        assert_eq!(clone.redact(&format!("p={path_value}")), "p=[REDACTED]");
    }

    #[test]
    fn missing_file_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Secrets::load(&tmp.path().join("secrets.toml")).unwrap();
        assert_eq!(s.get("TERN_TEST_SURELY_UNSET"), None);
    }

    #[test]
    fn reads_file_values() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "TERN_TEST_FILE_ONLY = \"abc\"\n", 0o600);
        let s = Secrets::load(&p).unwrap();
        assert_eq!(s.get("TERN_TEST_FILE_ONLY").as_deref(), Some("abc"));
        assert_eq!(s.source("TERN_TEST_FILE_ONLY"), Some(SecretSource::File));
        assert!(!format!("{s:?}").contains("abc"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_loose_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "K = \"v\"\n", 0o644);
        let err = Secrets::load(&p).unwrap_err();
        assert!(
            matches!(err, SecretError::Permissions { mode: 0o644, .. }),
            "{err}"
        );
    }

    #[test]
    fn rejects_non_string_values_without_echoing_them() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "K = 12345\n", 0o600);
        let err = Secrets::load(&p).unwrap_err().to_string();
        assert!(err.contains("\"K\""), "{err}");
        assert!(!err.contains("12345"), "{err}");
    }
}
