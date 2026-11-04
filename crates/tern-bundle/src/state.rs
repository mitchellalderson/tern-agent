//! The lockfile (`bundles/bundles.lock`: what is installed, pinned to
//! source, hash and key) and the trust store (`bundles/trusted.toml`: keys
//! the user trusts). Both are TOML files the user can read and edit.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::BundleError;
use crate::sign::PublicKey;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Lock {
    #[serde(default, rename = "bundle")]
    pub bundles: Vec<InstalledBundle>,
}

/// One installed bundle.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstalledBundle {
    pub name: String,
    pub version: String,
    /// The URL or path it was installed from (`update` fetches it again),
    /// or `openclaw:<dir>` for an import.
    pub source: String,
    /// SHA-256 of the archive (or of `bundle.toml` for a directory).
    pub sha256: String,
    /// The signing key, pinned: an update signed by another key is refused
    /// unless the user accepts the new key. `None` = unsigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The agent its routines belong to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub installed_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connectors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routines: Vec<InstalledRoutine>,
    /// Agents it made (templates become real agents at install).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<InstalledAgent>,
    /// Whether `key` is in the trust store: filled in when read, not stored.
    #[serde(skip)]
    pub key_trusted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledAgent {
    /// Its slug in the bundle (and when it was made).
    pub slug: String,
    /// The agent id (`tern:<slug>`) it became.
    pub id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledRoutine {
    /// Its name in the bundle.
    pub name: String,
    /// The routine (`sch_…`) it became.
    pub id: String,
}

impl InstalledBundle {
    pub fn signed(&self) -> bool {
        self.key.is_some()
    }

    /// `signed by D4CA68C37F8EBF48 (trusted)`, `unsigned`.
    pub fn signature_words(&self) -> String {
        match &self.key {
            None => "unsigned".into(),
            Some(k) => {
                let id = PublicKey::parse(k).map(|k| k.id_hex()).unwrap_or_default();
                if self.key_trusted {
                    format!("signed by {id} (trusted)")
                } else {
                    format!("signed by {id}")
                }
            }
        }
    }
}

impl Lock {
    pub fn load(path: &Path) -> Result<Self, BundleError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| BundleError::Invalid(format!("{}: {}", path.display(), e.message()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(BundleError::Io(format!("{}: {e}", path.display()))),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), BundleError> {
        let body = toml::to_string(self).map_err(|e| BundleError::Io(e.to_string()))?;
        write_atomic(
            path,
            &format!(
                "# Installed bundles, written by `tern bundle`: source, hash and key are pinned.\n{body}"
            ),
        )
    }

    pub fn get(&self, name: &str) -> Option<&InstalledBundle> {
        self.bundles.iter().find(|b| b.name == name)
    }

    pub fn put(&mut self, b: InstalledBundle) {
        self.bundles.retain(|x| x.name != b.name);
        self.bundles.push(b);
        self.bundles.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn remove(&mut self, name: &str) -> Option<InstalledBundle> {
        let i = self.bundles.iter().position(|b| b.name == name)?;
        Some(self.bundles.remove(i))
    }

    /// The bundle that installed connector `name`, if one did.
    pub fn owner_of_connector(&self, name: &str) -> Option<&InstalledBundle> {
        self.bundles
            .iter()
            .find(|b| b.connectors.iter().any(|c| c == name))
    }

    pub fn owner_of_skill(&self, slug: &str) -> Option<&InstalledBundle> {
        self.bundles
            .iter()
            .find(|b| b.skills.iter().any(|s| s == slug))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default, rename = "key")]
    pub keys: Vec<TrustedKey>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrustedKey {
    /// The minisign public key (`RWQ…`).
    pub key: String,
    /// Whose it is, in the user's words.
    #[serde(default)]
    pub name: String,
    pub added_at: String,
}

impl TrustStore {
    pub fn load(path: &Path) -> Result<Self, BundleError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| BundleError::Invalid(format!("{}: {}", path.display(), e.message()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(BundleError::Io(format!("{}: {e}", path.display()))),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), BundleError> {
        let body = toml::to_string(self).map_err(|e| BundleError::Io(e.to_string()))?;
        write_atomic(
            path,
            &format!(
                "# Bundle signing keys you trust (`tern bundle trust`). Bundles signed by\n# one of these install without the unknown-key warning.\n{body}"
            ),
        )
    }

    /// Whether `key` (any form `PublicKey::parse` reads) is trusted.
    pub fn contains(&self, key: &str) -> bool {
        let Ok(k) = PublicKey::parse(key) else {
            return false;
        };
        self.keys
            .iter()
            .any(|t| PublicKey::parse(&t.key).is_ok_and(|t| t == k))
    }

    pub fn find(&self, key: &PublicKey) -> Option<&TrustedKey> {
        self.keys
            .iter()
            .find(|t| PublicKey::parse(&t.key).is_ok_and(|t| t == *key))
    }

    /// Add `key` under `name`; true if it wasn't there.
    pub fn add(&mut self, key: &PublicKey, name: &str) -> bool {
        if self.find(key).is_some() {
            return false;
        }
        self.keys.push(TrustedKey {
            key: key.to_base64(),
            name: name.to_owned(),
            added_at: now(),
        });
        true
    }

    /// Remove a key by its base64, key id or name; true if one was removed.
    pub fn remove(&mut self, which: &str) -> bool {
        let before = self.keys.len();
        self.keys.retain(|t| {
            let id = PublicKey::parse(&t.key)
                .map(|k| k.id_hex())
                .unwrap_or_default();
            !(t.key == which
                || id.eq_ignore_ascii_case(which)
                || (!t.name.is_empty() && t.name == which))
        });
        self.keys.len() != before
    }
}

pub(crate) fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Write through a temporary file and rename, so a crash never leaves half
/// a lockfile.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<(), BundleError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| BundleError::Io(format!("{}: {e}", dir.display())))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| BundleError::Io(format!("{}: {e}", path.display())))
}
