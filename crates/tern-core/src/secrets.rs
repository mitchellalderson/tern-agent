//! Secret resolution: environment variables first, then `secrets.toml`.
//!
//! Secret values never appear in `Debug` output. `secrets.toml` must not be
//! readable by group or others; loading refuses looser permissions.
//!
//! [`Secrets::set`] and [`Secrets::remove`] edit the file in place (keeping
//! comments) and update every clone, so providers, connectors and MCP
//! servers see the change on their next use without a restart.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

pub mod keys;

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
    #[error(
        "invalid secret name {0:?}: use letters, digits and underscores, not starting with a digit"
    )]
    InvalidName(String),
    #[error("the value of {0} is empty")]
    EmptyValue(String),
    #[error("these secrets were not loaded from a file, so they can't be edited")]
    NoFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSource {
    Env,
    File,
}

/// What is known about one secret, without its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretStatus {
    pub name: String,
    /// Stored in `secrets.toml`.
    pub in_file: bool,
    /// Set in the environment, which takes precedence over the file.
    pub in_env: bool,
    /// What refers to it, e.g. `provider anthropic` or `connector hubspot`.
    pub used_by: Vec<String>,
}

impl SecretStatus {
    pub fn is_set(&self) -> bool {
        self.in_file || self.in_env
    }
}

#[derive(Default, Clone)]
pub struct Secrets {
    /// Where `secrets.toml` lives; `None` for secrets not loaded from a file.
    path: Option<PathBuf>,
    /// Shared by all clones, so edits reach everything holding one.
    file: Arc<RwLock<BTreeMap<String, String>>>,
    /// Every value handed out by [`Secrets::get`], shared by all clones, so
    /// redaction covers secrets first used after startup (env-only ones a
    /// connector or MCP server reads later).
    seen: Arc<Mutex<BTreeSet<String>>>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("file_keys", &self.file_names())
            .finish()
    }
}

impl Secrets {
    /// Load `secrets.toml` from `path`. A missing file is not an error; it
    /// is created by the first [`Secrets::set`].
    pub fn load(path: &Path) -> Result<Self, SecretError> {
        let file = match read_checked(path)? {
            Some(text) => parse(&text, path)?,
            None => BTreeMap::new(),
        };
        Ok(Self {
            path: Some(path.to_path_buf()),
            file: Arc::new(RwLock::new(file)),
            seen: Arc::default(),
        })
    }

    /// Re-read `secrets.toml`, for every clone.
    pub fn reload(&self) -> Result<(), SecretError> {
        let path = self.path.as_deref().ok_or(SecretError::NoFile)?;
        let file = match read_checked(path)? {
            Some(text) => parse(&text, path)?,
            None => BTreeMap::new(),
        };
        self.replace(file);
        Ok(())
    }

    /// Store `name = value` in `secrets.toml` (created with mode 0600 if
    /// missing), keeping the file's other entries and comments.
    pub fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        check_name(name)?;
        if value.trim().is_empty() {
            return Err(SecretError::EmptyValue(name.to_owned()));
        }
        // Keep redacting a value this one replaces.
        if let Some(old) = self.file_value(name)
            && let Ok(mut seen) = self.seen.lock()
        {
            seen.insert(old);
        }
        self.edit(|doc| {
            doc.insert(name, toml_edit::value(value));
            true
        })
        .map(|_| ())
    }

    /// Remove `name` from `secrets.toml`. Returns whether it was there.
    pub fn remove(&self, name: &str) -> Result<bool, SecretError> {
        check_name(name)?;
        if let Some(old) = self.file_value(name)
            && let Ok(mut seen) = self.seen.lock()
        {
            seen.insert(old);
        }
        self.edit(|doc| doc.remove(name).is_some())
    }

    /// Apply `change` to the file as it is on disk now, write it back if
    /// `change` returns true, and refresh every clone from the result.
    fn edit(
        &self,
        change: impl FnOnce(&mut toml_edit::DocumentMut) -> bool,
    ) -> Result<bool, SecretError> {
        let path = self.path.as_deref().ok_or(SecretError::NoFile)?;
        let text = read_checked(path)?.unwrap_or_default();
        // Validates the existing file; its entries must all be strings.
        parse(&text, path)?;
        let mut doc: toml_edit::DocumentMut =
            text.parse()
                .map_err(|e: toml_edit::TomlError| SecretError::Parse {
                    path: path.to_path_buf(),
                    message: e.message().to_owned(),
                })?;
        let changed = change(&mut doc);
        let text = doc.to_string();
        if changed {
            write_private(path, &text).map_err(|source| SecretError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        }
        self.replace(parse(&text, path)?);
        Ok(changed)
    }

    fn replace(&self, file: BTreeMap<String, String>) {
        match self.file.write() {
            Ok(mut g) => *g = file,
            Err(poisoned) => *poisoned.into_inner() = file,
        }
    }

    fn file_value(&self, name: &str) -> Option<String> {
        self.file.read().ok()?.get(name).cloned()
    }

    /// Names stored in `secrets.toml`, sorted.
    pub fn file_names(&self) -> Vec<String> {
        self.file
            .read()
            .map(|f| f.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Every secret in `secrets.toml` or named in `uses` (`(secret, user)`
    /// pairs), sorted by name. Never includes values.
    pub fn statuses(&self, uses: &[(String, String)]) -> Vec<SecretStatus> {
        fn status<'a>(
            all: &'a mut BTreeMap<String, SecretStatus>,
            name: &str,
        ) -> &'a mut SecretStatus {
            all.entry(name.to_owned()).or_insert_with(|| SecretStatus {
                name: name.to_owned(),
                in_file: false,
                in_env: std::env::var(name).is_ok_and(|v| !v.is_empty()),
                used_by: Vec::new(),
            })
        }
        let mut all = BTreeMap::new();
        for name in self.file_names() {
            status(&mut all, &name).in_file = true;
        }
        for (name, user) in uses {
            let s = status(&mut all, name);
            if !s.used_by.contains(user) {
                s.used_by.push(user.clone());
            }
        }
        all.into_values().collect()
    }
}

impl Secrets {
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
        let mut all: BTreeSet<String> = self.file_values().into_iter().collect();
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
        self.file
            .read()
            .map(|f| f.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn source(&self, name: &str) -> Option<SecretSource> {
        self.lookup(name).map(|(_, s)| s)
    }

    fn lookup(&self, name: &str) -> Option<(String, SecretSource)> {
        if let Some(v) = std::env::var(name).ok().filter(|v| !v.is_empty()) {
            return Some((v, SecretSource::Env));
        }
        self.file_value(name).map(|v| (v, SecretSource::File))
    }
}

/// Secret names double as environment variable names, which take precedence.
pub fn check_name(name: &str) -> Result<(), SecretError> {
    let mut chars = name.chars();
    let ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 128;
    if ok {
        Ok(())
    } else {
        Err(SecretError::InvalidName(name.to_owned()))
    }
}

/// Read `path` after checking its permissions; `None` if it doesn't exist.
fn read_checked(path: &Path) -> Result<Option<String>, SecretError> {
    let io_err = |source| SecretError::Io {
        path: path.to_path_buf(),
        source,
    };
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(e)),
    };
    check_permissions(path, &meta)?;
    std::fs::read_to_string(path).map(Some).map_err(io_err)
}

fn parse(text: &str, path: &Path) -> Result<BTreeMap<String, String>, SecretError> {
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
    Ok(file)
}

/// Replace `path` with `text` atomically. The file is created with mode
/// 0600, so the value is never readable by others, even briefly.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("toml.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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

    #[test]
    fn set_and_remove_edit_the_file_and_every_clone() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "# my keys\nKEEP = \"keep-me\"\nOLD_KEY = \"old-value\"\n",
            0o600,
        );
        let s = Secrets::load(&p).unwrap();
        let clone = s.clone();
        s.set("TERN_TEST_NEW", "new-value").unwrap();
        s.set("OLD_KEY", "replaced").unwrap();
        assert_eq!(clone.get("TERN_TEST_NEW").as_deref(), Some("new-value"));
        assert_eq!(clone.get("OLD_KEY").as_deref(), Some("replaced"));
        assert_eq!(
            clone.redact("old-value"),
            "[REDACTED]",
            "replaced values stay redacted"
        );

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# my keys"), "{text}");
        assert!(text.contains("KEEP = \"keep-me\""), "{text}");
        assert!(Secrets::load(&p).is_ok(), "still 0600");

        assert!(s.remove("TERN_TEST_NEW").unwrap());
        assert!(!s.remove("TERN_TEST_NEW").unwrap());
        assert_eq!(clone.get("TERN_TEST_NEW"), None);
        assert_eq!(clone.file_names(), ["KEEP", "OLD_KEY"]);
    }

    #[test]
    fn set_creates_a_private_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("secrets.toml");
        let s = Secrets::load(&p).unwrap();
        s.set("TERN_TEST_FRESH", "a \"quoted\" value").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let again = Secrets::load(&p).unwrap();
        assert_eq!(
            again.get("TERN_TEST_FRESH").as_deref(),
            Some("a \"quoted\" value")
        );
    }

    #[test]
    fn set_rejects_bad_names_and_empty_values() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Secrets::load(&tmp.path().join("secrets.toml")).unwrap();
        for bad in ["", "1ABC", "A-B", "A B", "A.B"] {
            assert!(
                matches!(s.set(bad, "v"), Err(SecretError::InvalidName(_))),
                "{bad}"
            );
        }
        assert!(matches!(
            s.set("OK_NAME", "  "),
            Err(SecretError::EmptyValue(_))
        ));
        assert!(!tmp.path().join("secrets.toml").exists());
    }

    #[test]
    fn statuses_merge_file_entries_and_uses() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "TERN_TEST_IN_FILE = \"x\"\n", 0o600);
        let s = Secrets::load(&p).unwrap();
        let uses = [
            ("TERN_TEST_MISSING".to_owned(), "provider a".to_owned()),
            ("TERN_TEST_IN_FILE".to_owned(), "mcp b".to_owned()),
            ("TERN_TEST_IN_FILE".to_owned(), "mcp b".to_owned()),
        ];
        let st = s.statuses(&uses);
        assert_eq!(st.len(), 2);
        assert_eq!(st[0].name, "TERN_TEST_IN_FILE");
        assert!(st[0].in_file && st[0].is_set());
        assert_eq!(st[0].used_by, ["mcp b"]);
        assert_eq!(st[1].name, "TERN_TEST_MISSING");
        assert!(!st[1].is_set());
    }
}
