//! Secret resolution: environment variables first, then `secrets.toml`.
//!
//! Secret values never appear in `Debug` output. `secrets.toml` must not be
//! readable by group or others; loading refuses looser permissions.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

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
        Ok(Self { file })
    }

    pub fn get(&self, name: &str) -> Option<String> {
        self.lookup(name).map(|(v, _)| v)
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
