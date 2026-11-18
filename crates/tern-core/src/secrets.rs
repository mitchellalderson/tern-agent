//! Secret resolution: environment variables first, then `secrets.toml`.
//!
//! Secret values never appear in `Debug` output. `secrets.toml` must not be
//! readable by group or others; loading refuses looser permissions.
//!
//! Values are encrypted at rest ([`vault`]): the file holds `enc:v1:…`
//! strings bound to their names. A plaintext value, such as one pasted into
//! the file by hand, still works and is encrypted on the next load.
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
pub mod vault;

use vault::{KeySource, Vault};

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
    #[error(
        "can't decrypt {0}: the key that sealed it is missing or the value was moved; set it again"
    )]
    Decrypt(String),
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
    /// The key that seals the file's values, opened on first use.
    vault: Arc<LazyVault>,
}

/// A [`Vault`] opened on first use, so reading a file with no encrypted
/// values (or a missing one) never creates a key.
struct LazyVault {
    path: PathBuf,
    source: KeySource,
    cell: Mutex<Option<Vault>>,
}

impl LazyVault {
    fn get(&self) -> Result<Vault, SecretError> {
        let mut cell = self.cell.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(v) = cell.as_ref() {
            return Ok(v.clone());
        }
        let v = Vault::open(&self.path, self.source)?;
        *cell = Some(v.clone());
        Ok(v)
    }
}

impl Default for LazyVault {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            source: KeySource::File,
            cell: Mutex::new(None),
        }
    }
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
    ///
    /// The key comes from [`vault::set_default_key_source`] (the file by
    /// default). Plaintext values are encrypted in place.
    pub fn load(path: &Path) -> Result<Self, SecretError> {
        Self::load_with(path, vault::default_key_source())
    }

    /// [`Secrets::load`] with an explicit key source.
    pub fn load_with(path: &Path, source: KeySource) -> Result<Self, SecretError> {
        let s = Self {
            path: Some(path.to_path_buf()),
            file: Arc::default(),
            seen: Arc::default(),
            vault: Arc::new(LazyVault {
                path: path.to_path_buf(),
                source,
                cell: Mutex::new(None),
            }),
        };
        s.reload()?;
        Ok(s)
    }

    /// Re-read `secrets.toml`, for every clone. Plaintext values found are
    /// encrypted in place.
    pub fn reload(&self) -> Result<(), SecretError> {
        let path = self.path.as_deref().ok_or(SecretError::NoFile)?;
        let (file, plain) = match read_checked(path)? {
            Some(text) => self.parse(&text, path)?,
            None => (BTreeMap::new(), false),
        };
        self.replace(file);
        if plain && let Err(e) = self.edit(|_| false) {
            tracing::warn!("couldn't encrypt {}: {e}", path.display());
        }
        Ok(())
    }

    /// Store `name = value` in `secrets.toml` (created with mode 0600 if
    /// missing), keeping the file's other entries and comments. `name` is a
    /// plain name or a connection's ([`check_scoped`]).
    pub fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.set_many(&[(name, value)])
    }

    /// Store several values in one write, so a token set (access, refresh,
    /// expiry) is never half-saved.
    pub fn set_many(&self, pairs: &[(&str, &str)]) -> Result<(), SecretError> {
        self.update(pairs, &[])
    }

    /// Store `pairs` and remove the connection-scoped entries `remove` in
    /// one write, so a new grant replaces the whole token set at once (a
    /// stale refresh token never survives next to a new access token).
    /// Removals aren't name-checked beyond being scoped, so names an older
    /// layout wrote can be cleaned up.
    pub fn update(&self, pairs: &[(&str, &str)], remove: &[&str]) -> Result<(), SecretError> {
        for (name, value) in pairs {
            check_any(name)?;
            if value.trim().is_empty() {
                return Err(SecretError::EmptyValue((*name).to_owned()));
            }
            // Keep redacting a value this one replaces.
            self.remember_old(name);
        }
        for name in remove {
            if !is_scoped(name) {
                return Err(SecretError::InvalidName((*name).to_owned()));
            }
            self.remember_old(name);
        }
        self.edit(|doc| {
            for name in remove {
                doc.remove(name);
            }
            for (name, value) in pairs {
                doc.insert(name, toml_edit::value(*value));
            }
            true
        })
        .map(|_| ())
    }

    fn remember_old(&self, name: &str) {
        if let Some(old) = self.file_value(name)
            && let Ok(mut seen) = self.seen.lock()
        {
            seen.insert(old);
        }
    }

    /// Remove `name` from `secrets.toml`. Returns whether it was there.
    pub fn remove(&self, name: &str) -> Result<bool, SecretError> {
        check_any(name)?;
        self.remember_old(name);
        self.edit(|doc| doc.remove(name).is_some())
    }

    /// Remove every connection-scoped entry starting with `prefix` (for
    /// example `conn/connector:gmail/work/`). Returns how many there were.
    pub fn remove_prefix(&self, prefix: &str) -> Result<usize, SecretError> {
        if !prefix.starts_with(SCOPED_PREFIX) || !prefix.ends_with('/') {
            return Err(SecretError::InvalidName(prefix.to_owned()));
        }
        let names: Vec<String> = self
            .file_names()
            .into_iter()
            .filter(|n| n.starts_with(prefix))
            .collect();
        if names.is_empty() {
            return Ok(0);
        }
        for n in &names {
            self.remember_old(n);
        }
        self.edit(|doc| {
            for n in &names {
                doc.remove(n);
            }
            true
        })?;
        Ok(names.len())
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
        self.parse(&text, path)?;
        let mut doc: toml_edit::DocumentMut =
            text.parse()
                .map_err(|e: toml_edit::TomlError| SecretError::Parse {
                    path: path.to_path_buf(),
                    message: e.message().to_owned(),
                })?;
        let mut changed = change(&mut doc);
        changed |= self.seal_plaintext(&mut doc)?;
        let text = doc.to_string();
        if changed {
            write_private(path, &text).map_err(|source| SecretError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        }
        self.replace(self.parse(&text, path)?.0);
        Ok(changed)
    }

    /// Encrypt every plaintext string in `doc`. Whether any was.
    fn seal_plaintext(&self, doc: &mut toml_edit::DocumentMut) -> Result<bool, SecretError> {
        let mut vault = None;
        let mut any = false;
        for (name, item) in doc.iter_mut() {
            let Some(text) = item.as_str() else { continue };
            if vault::is_sealed(text) {
                continue;
            }
            if vault.is_none() {
                vault = Some(self.vault.get()?);
            }
            if let Some(v) = &vault {
                let sealed = v.seal(name.get(), text)?;
                *item = toml_edit::value(sealed);
                any = true;
            }
        }
        Ok(any)
    }

    /// The file's entries decrypted, and whether any was plaintext. An entry
    /// that can't be decrypted (lost key, value moved to another name) is
    /// skipped with a warning, so it reads as unset and gets asked for again.
    fn parse(
        &self,
        text: &str,
        path: &Path,
    ) -> Result<(BTreeMap<String, String>, bool), SecretError> {
        let raw = parse(text, path)?;
        let mut plain = false;
        let mut out = BTreeMap::new();
        let mut vault = None;
        for (name, stored) in raw {
            if !vault::is_sealed(&stored) {
                plain = true;
                out.insert(name, stored);
                continue;
            }
            if vault.is_none() {
                vault = Some(self.vault.get()?);
            }
            let Some(v) = &vault else { continue };
            match v.open_value(&name, &stored) {
                Ok(value) => {
                    out.insert(name, value);
                }
                Err(e) => tracing::warn!("{e}"),
            }
        }
        Ok((out, plain))
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
        // A connection's credentials are shown with the connection.
        for name in self.file_names().into_iter().filter(|n| !is_scoped(n)) {
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
        // A connection's credentials live only in the file: an environment
        // variable can't have that name, and mustn't stand in for a token.
        if is_scoped(name) {
            return self.file_value(name).map(|v| (v, SecretSource::File));
        }
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

/// Connection-scoped names start with this (tooling spec §6.8.1).
pub const SCOPED_PREFIX: &str = "conn/";

/// Whether `name` belongs to a connection (`conn/<kind>:<target>/<slug>/…`).
pub fn is_scoped(name: &str) -> bool {
    name.starts_with(SCOPED_PREFIX)
}

/// A connection's credential: `conn/<kind>:<target>/<slug>/<NAME>` for a
/// key the target names, or `conn/<kind>:<target>/<slug>/oauth/<field>`
/// for its OAuth tokens. The kind (`connector` or `mcp`) keeps a connector
/// and an MCP server of the same name apart. Targets are their names
/// (`[a-z0-9_.-]`, ≤ 64), slugs `[a-z0-9_-]` (≤ 16).
pub fn check_scoped(name: &str) -> Result<(), SecretError> {
    let bad = || SecretError::InvalidName(name.to_owned());
    let rest = name.strip_prefix(SCOPED_PREFIX).ok_or_else(bad)?;
    let mut parts = rest.splitn(3, '/');
    let (Some(target), Some(slug), Some(tail)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(bad());
    };
    let target = match target.split_once(':') {
        Some(("connector" | "mcp", t)) => t,
        _ => return Err(bad()),
    };
    let ok_target = !target.is_empty()
        && target.len() <= 64
        && target
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'));
    if !ok_target || !valid_slug(slug) {
        return Err(bad());
    }
    match tail.strip_prefix("oauth/") {
        Some(field)
            if !field.is_empty() && field.chars().all(|c| c.is_ascii_lowercase() || c == '_') =>
        {
            Ok(())
        }
        Some(_) => Err(bad()),
        None => check_name(tail).map_err(|_| bad()),
    }
}

/// A connection slug: `[a-z0-9_-]{1,16}`.
pub fn valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

fn check_any(name: &str) -> Result<(), SecretError> {
    if is_scoped(name) {
        check_scoped(name)
    } else {
        check_name(name)
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

/// Secrets at least this long (in characters) also have their fragments
/// redacted: shorter ones would make a 4-character tail match ordinary text.
pub const FRAGMENT_MIN_LEN: usize = 20;
/// Masked echoes (`ck_**3pNe`) are caught from this length: they need a mask
/// run right before the tail, so they rarely match anything else.
const MASKED_MIN_LEN: usize = 12;
const FRAGMENT_HEAD: usize = 8;
const FRAGMENT_TAIL: usize = 4;

/// Replace each of `values` (longest first) in `text`, and the fragments a
/// service may echo back instead of the whole value: a masked form such as
/// `ck_**3pNe` or `sk-…3pNe`, and a long secret's first 8 and last 4
/// characters on their own.
pub fn redact_with(values: &[String], text: &str) -> String {
    let mut out = text.to_owned();
    for v in values {
        if out.contains(v.as_str()) {
            out = out.replace(v.as_str(), REDACTED);
        }
    }
    for v in values {
        let chars: Vec<char> = v.chars().collect();
        if chars.len() >= MASKED_MIN_LEN {
            let head: String = chars[..FRAGMENT_HEAD.min(chars.len())].iter().collect();
            let tail: String = chars[chars.len() - FRAGMENT_TAIL..].iter().collect();
            out = redact_masked(&out, &head, &tail);
        }
    }
    for v in values {
        let chars: Vec<char> = v.chars().collect();
        if chars.len() >= FRAGMENT_MIN_LEN {
            let head: String = chars[..FRAGMENT_HEAD].iter().collect();
            let tail: String = chars[chars.len() - FRAGMENT_TAIL..].iter().collect();
            for frag in [head, tail] {
                if out.contains(frag.as_str()) {
                    out = out.replace(frag.as_str(), REDACTED);
                }
            }
        }
    }
    out
}

fn is_mask_char(c: char) -> bool {
    matches!(c, '*' | '•' | '●' | '∗' | '…' | '·' | '.')
}

/// Replace `[prefix of head][mask run]tail` with `[REDACTED]`, where the mask
/// run is one or more mask characters directly before `tail`.
fn redact_masked(text: &str, head: &str, tail: &str) -> String {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (at, _) in text.match_indices(tail) {
        let mut start = at;
        for (i, c) in text[..at].char_indices().rev() {
            if !is_mask_char(c) {
                break;
            }
            start = i;
        }
        if start == at {
            continue;
        }
        // Take the leading characters of the secret that precede the mask.
        let before = &text[..start];
        let head_chars: Vec<(usize, char)> = head.char_indices().collect();
        for k in (1..=head_chars.len()).rev() {
            let end = head_chars.get(k).map_or(head.len(), |(i, _)| *i);
            let prefix = &head[..end];
            if before.ends_with(prefix) {
                start -= prefix.len();
                break;
            }
        }
        if spans.last().is_none_or(|(_, e)| start >= *e) {
            spans.push((start, at + tail.len()));
        }
    }
    if spans.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (s, e) in spans {
        out.push_str(&text[pos..s]);
        out.push_str(REDACTED);
        pos = e;
    }
    out.push_str(&text[pos..]);
    out
}

#[cfg(unix)]
pub(crate) fn check_permissions(path: &Path, meta: &std::fs::Metadata) -> Result<(), SecretError> {
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
pub(crate) fn check_permissions(
    _path: &Path,
    _meta: &std::fs::Metadata,
) -> Result<(), SecretError> {
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
        assert!(text.contains("KEEP = \"enc:v1:"), "{text}");
        assert!(!text.contains("keep-me"), "{text}");
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
    fn connection_scoped_names_live_only_in_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "PLAIN = \"plain-value\"\n", 0o600);
        let s = Secrets::load(&p).unwrap();
        s.set_many(&[
            (
                "conn/connector:gmail/work/oauth/access_token",
                "ya29.access-work",
            ),
            (
                "conn/connector:gmail/work/oauth/refresh_token",
                "1//refresh-work",
            ),
            (
                "conn/connector:gmail/home/oauth/access_token",
                "ya29.access-home",
            ),
            (
                "conn/connector:github/personal/GITHUB_TOKEN",
                "ghp_personal",
            ),
        ])
        .unwrap();
        assert_eq!(
            s.get("conn/connector:gmail/work/oauth/access_token")
                .as_deref(),
            Some("ya29.access-work")
        );
        // Old binaries read the file too: every key is still a string.
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("\"conn/connector:gmail/work/oauth/access_token\" = "),
            "{text}"
        );
        assert!(text.contains("PLAIN = \"enc:v1:"), "{text}");
        assert!(
            !text.contains("plain-value") && !text.contains("refresh-work"),
            "{text}"
        );
        // Values are redacted like any other secret.
        assert_eq!(s.redact("t=1//refresh-work"), "t=[REDACTED]");
        // The Secrets view shows plain names only.
        let names: Vec<String> = s.statuses(&[]).into_iter().map(|st| st.name).collect();
        assert_eq!(names, ["PLAIN"]);
        assert_eq!(s.remove_prefix("conn/connector:gmail/work/").unwrap(), 2);
        assert_eq!(s.get("conn/connector:gmail/work/oauth/refresh_token"), None);
        assert!(
            s.get("conn/connector:gmail/home/oauth/access_token")
                .is_some()
        );
        assert_eq!(
            s.redact("1//refresh-work"),
            "[REDACTED]",
            "removed values stay redacted"
        );
        for bad in [
            "conn/connector:gmail/work",
            "conn/connector:Gmail/work/X",
            "conn/connector:gmail/a-very-long-slug-name/X",
            "conn/connector:gmail/work/oauth/",
            "conn/connector:gmail/work/bad-name",
            "conn/connector:/work/X",
            // No kind, or an unknown one.
            "conn/gmail/work/X",
            "conn/other:gmail/work/X",
        ] {
            assert!(s.set(bad, "v").is_err(), "{bad}");
        }
        assert!(s.remove_prefix("conn/").is_ok());
        assert!(s.remove_prefix("PLAIN").is_err());
    }

    const KEY: &str = "ck_live_abcdef0123456789xyz3pNe";

    fn one(v: &str) -> Vec<String> {
        vec![v.to_owned()]
    }

    #[test]
    fn masked_echoes_are_redacted() {
        let v = one(KEY);
        for echoed in [
            "key ck_**3pNe rejected",
            "key ck_live_****3pNe rejected",
            "key sk-…3pNe rejected",
            "key ****3pNe rejected",
            "key ck_•••••3pNe rejected",
            "key ck_...3pNe rejected",
        ] {
            let out = redact_with(&v, echoed);
            assert!(!out.contains("3pNe"), "{echoed} -> {out}");
            assert!(out.contains(REDACTED), "{out}");
            assert!(out.ends_with(" rejected"), "{out}");
        }
        // The prefix is taken with the mask, not left behind.
        assert_eq!(
            redact_with(&v, "key ck_**3pNe rejected"),
            "key [REDACTED] rejected"
        );
    }

    #[test]
    fn first_eight_and_last_four_are_redacted_alone() {
        let v = one(KEY);
        assert_eq!(redact_with(&v, "starts ck_live ok"), "starts ck_live ok");
        let out = redact_with(&v, "it begins ck_live_ and ends in 3pNe");
        assert_eq!(out, "it begins [REDACTED] and ends in [REDACTED]");
        assert_eq!(redact_with(&v, "ends in 3pNe"), "ends in [REDACTED]");
    }

    #[test]
    fn short_secrets_keep_fragments_and_plain_text_alone() {
        // Under 20 characters, a 4-character tail would hit ordinary text.
        let short = one("secretvalue1234");
        assert_eq!(
            redact_with(&short, "see 1234 and secretva"),
            "see 1234 and secretva"
        );
        // Masked echoes still count from 12 characters.
        assert_eq!(redact_with(&short, "x ***1234 y"), "x [REDACTED] y");
        // A mask run not followed by the tail is left alone.
        assert_eq!(redact_with(&one(KEY), "a ***** b 3pNx"), "a ***** b 3pNx");
        // And a tail with no mask run in front isn't treated as masked.
        let tiny = one("abcd1234");
        assert_eq!(redact_with(&tiny, "x ***1234"), "x ***1234");
    }

    #[test]
    fn secrets_redact_covers_fragments_too() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), &format!("API = \"{KEY}\"\n"), 0o600);
        let s = Secrets::load(&p).unwrap();
        assert_eq!(s.redact("ends in ck_**3pNe."), "ends in [REDACTED].");
    }

    #[test]
    fn values_are_encrypted_at_rest_and_plaintext_is_migrated() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "# keep\nOLD = \"legacy-plaintext-value\"\n\"conn/connector:x/a/TOKEN\" = \"scoped-plain\"\n",
            0o600,
        );
        let s = Secrets::load(&p).unwrap();
        assert_eq!(s.get("OLD").as_deref(), Some("legacy-plaintext-value"));
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("legacy-plaintext-value"), "{text}");
        assert!(!text.contains("scoped-plain"), "{text}");
        assert!(
            text.contains("# keep") && text.contains("enc:v1:"),
            "{text}"
        );
        // New values are sealed too, and a fresh load reads them all.
        s.set("NEWER", "brand-new-secret").unwrap();
        assert!(
            !std::fs::read_to_string(&p)
                .unwrap()
                .contains("brand-new-secret")
        );
        let again = Secrets::load(&p).unwrap();
        assert_eq!(again.get("NEWER").as_deref(), Some("brand-new-secret"));
        assert_eq!(
            again.get("conn/connector:x/a/TOKEN").as_deref(),
            Some("scoped-plain")
        );
        // A value pasted into the file by hand is picked up and sealed.
        let mut text = std::fs::read_to_string(&p).unwrap();
        text.push_str("PASTED = \"pasted-by-hand\"\n");
        std::fs::write(&p, text).unwrap();
        again.reload().unwrap();
        assert_eq!(again.get("PASTED").as_deref(), Some("pasted-by-hand"));
        assert!(
            !std::fs::read_to_string(&p)
                .unwrap()
                .contains("pasted-by-hand")
        );
        // Names only in the status view, never values.
        assert!(!format!("{again:?}").contains("brand-new"));
    }

    #[test]
    fn a_value_moved_to_another_name_or_a_lost_key_reads_as_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("secrets.toml");
        let s = Secrets::load(&p).unwrap();
        s.set("A_KEY", "value-a-123").unwrap();
        s.set("B_KEY", "value-b-456").unwrap();
        // Swap the ciphertexts between names.
        let doc: toml::Table = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let (a, b) = (
            doc["A_KEY"].as_str().unwrap(),
            doc["B_KEY"].as_str().unwrap(),
        );
        std::fs::write(&p, format!("A_KEY = \"{b}\"\nB_KEY = \"{a}\"\n")).unwrap();
        let swapped = Secrets::load(&p).unwrap();
        assert_eq!((swapped.get("A_KEY"), swapped.get("B_KEY")), (None, None));
        // A lost key: the values can't be opened, and setting again works.
        std::fs::write(&p, format!("A_KEY = \"{a}\"\n")).unwrap();
        std::fs::remove_file(tmp.path().join("secrets.key")).unwrap();
        let lost = Secrets::load(&p).unwrap();
        assert_eq!(lost.get("A_KEY"), None);
        lost.set("A_KEY", "fresh-value-1").unwrap();
        assert_eq!(
            Secrets::load(&p).unwrap().get("A_KEY").as_deref(),
            Some("fresh-value-1")
        );
    }

    #[test]
    fn reading_a_missing_or_empty_file_creates_no_key() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("secrets.toml");
        Secrets::load(&p).unwrap();
        assert!(!tmp.path().join("secrets.key").exists());
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
