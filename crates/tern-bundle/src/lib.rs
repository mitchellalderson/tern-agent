//! Bundles (tooling spec §6.5): connectors, skills and routine templates
//! shared as a directory or `.tar.zst` with a `bundle.toml` that lists the
//! SHA-256 of every file, signed with ed25519 in minisign's format, pinned
//! by hash, and installed only after the user has seen everything it can
//! do. There is no registry: a bundle is a URL, a file or a directory.
//!
//! - [`source`] reads and checks a bundle (hash, files, signature);
//! - [`plan`] turns one into what the user approves, and diffs versions;
//! - [`install`] installs, updates and removes them, keeping a lockfile;
//! - [`export`] packages the user's own connectors, skills and routines;
//! - [`openclaw`] converts an OpenClaw workspace into a bundle;
//! - [`state`] is the lockfile and the trust store; [`installed`] is the
//!   read-only view for other parts of tern (`tern security audit`).

pub mod install;
pub mod lookalike;
pub mod manifest;
pub mod plan;
pub mod sign;
pub mod source;
pub mod state;

use std::path::PathBuf;

use tern_core::TernHome;

pub use install::{InstallOptions, Installer, Outcome, Prepared};
pub use manifest::{Manifest, RoutineTemplate};
pub use plan::Plan;
pub use sign::{PublicKey, SecretKey};
pub use source::{Bundle, Fetcher, Origin, Raw};
pub use state::{InstalledBundle, Lock, TrustStore};

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("{0}")]
    Invalid(String),
    #[error("the signature is invalid: {0}")]
    BadSignature(String),
    #[error(
        "the bundle's hash is {got}, not the pinned {expected}: it isn't the bundle you expected"
    )]
    HashMismatch { expected: String, got: String },
    #[error("{0} doesn't match its hash in bundle.toml: it changed after the bundle was made")]
    FileChanged(String),
    #[error("fetching the bundle: {0}")]
    Fetch(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Refused(String),
}

/// Where bundle state lives: `~/.tern/bundles/`.
#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn new(home: &TernHome) -> Self {
        Self {
            root: home.root().join("bundles"),
        }
    }
    /// What is installed, from where, pinned to which hash and key.
    pub fn lock(&self) -> PathBuf {
        self.root.join("bundles.lock")
    }
    /// Keys whose bundles install without the "unknown key" warning.
    pub fn trusted(&self) -> PathBuf {
        self.root.join("trusted.toml")
    }
    /// The user's own signing key (`tern bundle keygen`).
    pub fn signing_key(&self) -> PathBuf {
        self.root.join("signing.key")
    }
    pub fn signing_pub(&self) -> PathBuf {
        self.root.join("signing.pub")
    }
    /// A copy of each installed bundle, so an update can be compared with
    /// what was approved and `remove` knows what it installed.
    pub fn copy(&self, name: &str) -> PathBuf {
        self.root.join("installed").join(name)
    }
}

/// Every installed bundle and how it is signed, for `tern security audit`
/// and anything else that only needs to look (T6b). Reads the lockfile;
/// never fails (a missing or unreadable one is no bundles).
pub fn installed(home: &TernHome) -> Vec<InstalledBundle> {
    let paths = Paths::new(home);
    let trust = TrustStore::load(&paths.trusted()).unwrap_or_default();
    Lock::load(&paths.lock())
        .map(|l| l.bundles)
        .unwrap_or_default()
        .into_iter()
        .map(|mut b| {
            b.key_trusted = b.key.as_deref().is_some_and(|k| trust.contains(k));
            b
        })
        .collect()
}

/// Installed bundles with no signature: what `tern security audit` lists.
pub fn unsigned(home: &TernHome) -> Vec<InstalledBundle> {
    installed(home)
        .into_iter()
        .filter(|b| b.key.is_none())
        .collect()
}
