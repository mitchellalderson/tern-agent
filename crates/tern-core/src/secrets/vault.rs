//! Encryption at rest for `secrets.toml`.
//!
//! AES-256-GCM, a fresh random nonce per value, and the secret's name as
//! associated data: a ciphertext copied under another name (a connection's
//! token moved to a different connection) fails to open. Names already carry their scope, e.g.
//! `conn/connector:gmail/work/oauth/access_token`.
//!
//! The key comes from the macOS keychain when the host asks for it
//! ([`set_default_key_source`]), else from `secrets.key` next to
//! `secrets.toml` (mode 0600). The file key keeps a stray copy of
//! `secrets.toml` (a backup, a pasted file, a sync folder) unreadable, but
//! not against something that can read the whole home folder.
//!
//! A stored value is `enc:v1:` plus hex of `nonce || ciphertext || tag`. A
//! value without that prefix is legacy plaintext and is migrated on the next
//! write.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_rs::rand;

use super::SecretError;

pub const PREFIX: &str = "enc:v1:";
const KEY_LEN: usize = 32;
const KEY_FILE: &str = "secrets.key";
const KEYCHAIN_SERVICE: &str = "tern";
const KEYCHAIN_ACCOUNT: &str = "secrets-key";

/// Where the encryption key lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// The macOS keychain, falling back to the key file when it is
    /// unavailable (other platforms, a locked or headless session).
    Keychain,
    /// `secrets.key` next to `secrets.toml`.
    File,
}

static DEFAULT_SOURCE: OnceLock<KeySource> = OnceLock::new();

/// Choose the key source for every [`super::Secrets::load`] in this process.
/// The `tern` binary asks for the keychain; tests and library users keep the
/// file, so they never touch the user's keychain.
pub fn set_default_key_source(source: KeySource) {
    let _ = DEFAULT_SOURCE.set(source);
}

pub(super) fn default_key_source() -> KeySource {
    DEFAULT_SOURCE.get().copied().unwrap_or(KeySource::File)
}

#[derive(Clone)]
pub struct Vault {
    key: std::sync::Arc<LessSafeKey>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Vault(..)")
    }
}

/// Whether `stored` is an encrypted value (not legacy plaintext).
pub fn is_sealed(stored: &str) -> bool {
    stored.starts_with(PREFIX)
}

impl Vault {
    pub fn from_key(bytes: &[u8; KEY_LEN]) -> Result<Self, String> {
        let key = UnboundKey::new(&AES_256_GCM, bytes).map_err(|_| "bad key".to_owned())?;
        Ok(Self {
            key: std::sync::Arc::new(LessSafeKey::new(key)),
        })
    }

    /// The vault for the secrets at `secrets_path`: the key from `source`,
    /// created on first use.
    pub fn open(secrets_path: &Path, source: KeySource) -> Result<Self, SecretError> {
        let key_file = secrets_path.with_file_name(KEY_FILE);
        let bytes = match source {
            KeySource::File => file_key(&key_file)?,
            KeySource::Keychain => keychain_key(&key_file)?,
        };
        Self::from_key(&bytes).map_err(|message| SecretError::Parse {
            path: key_file,
            message,
        })
    }

    /// `plaintext` sealed under `name`.
    pub fn seal(&self, name: &str, plaintext: &str) -> Result<String, SecretError> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::fill(&mut nonce).map_err(|_| crypto_err("random number generator failed"))?;
        let mut buf = plaintext.as_bytes().to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(name.as_bytes()),
                &mut buf,
            )
            .map_err(|_| crypto_err("encryption failed"))?;
        let mut out = String::with_capacity(PREFIX.len() + 2 * (NONCE_LEN + buf.len()));
        out.push_str(PREFIX);
        out.push_str(&hex(&nonce));
        out.push_str(&hex(&buf));
        Ok(out)
    }

    /// The value sealed under `name`. A value without the `enc:v1:` prefix
    /// is legacy plaintext and comes back as is.
    pub fn open_value(&self, name: &str, stored: &str) -> Result<String, SecretError> {
        let Some(rest) = stored.strip_prefix(PREFIX) else {
            return Ok(stored.to_owned());
        };
        let bad = || SecretError::Decrypt(name.to_owned());
        let mut bytes = unhex(rest).ok_or_else(bad)?;
        if bytes.len() < NONCE_LEN {
            return Err(bad());
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&bytes[..NONCE_LEN]);
        let plain = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(name.as_bytes()),
                &mut bytes[NONCE_LEN..],
            )
            .map_err(|_| bad())?;
        String::from_utf8(plain.to_vec()).map_err(|_| bad())
    }
}

fn crypto_err(message: &str) -> SecretError {
    SecretError::Parse {
        path: PathBuf::from("secrets"),
        message: message.to_owned(),
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn new_key() -> Result<[u8; KEY_LEN], SecretError> {
    let mut k = [0u8; KEY_LEN];
    rand::fill(&mut k).map_err(|_| crypto_err("random number generator failed"))?;
    Ok(k)
}

/// The key in `path` (hex, mode 0600), created if missing.
fn file_key(path: &Path) -> Result<[u8; KEY_LEN], SecretError> {
    let io_err = |source| SecretError::Io {
        path: path.to_path_buf(),
        source,
    };
    match std::fs::metadata(path) {
        Ok(meta) => {
            super::check_permissions(path, &meta)?;
            let text = std::fs::read_to_string(path).map_err(io_err)?;
            decode_key(&text).ok_or_else(|| SecretError::Parse {
                path: path.to_path_buf(),
                message: "not a valid key file".to_owned(),
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = new_key()?;
            write_new_private(path, &hex(&key)).map_err(io_err)?;
            Ok(key)
        }
        Err(e) => Err(io_err(e)),
    }
}

fn decode_key(text: &str) -> Option<[u8; KEY_LEN]> {
    let bytes = unhex(text.trim())?;
    bytes.try_into().ok()
}

fn write_new_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

/// The keychain key, looked up once per process (a failed lookup is not
/// retried, so a missing keychain can't raise a prompt twice).
#[cfg(target_os = "macos")]
fn keychain_key(key_file: &Path) -> Result<[u8; KEY_LEN], SecretError> {
    static CACHE: OnceLock<Option<[u8; KEY_LEN]>> = OnceLock::new();
    if let Some(k) = CACHE.get_or_init(|| keychain_lookup().ok()) {
        return Ok(*k);
    }
    file_key(key_file)
}

/// Never raises a prompt: without a login keychain file on disk the
/// Security framework isn't called at all (headless, CI, a fresh user).
#[cfg(target_os = "macos")]
fn keychain_lookup() -> Result<[u8; KEY_LEN], SecretError> {
    let has_keychain = std::env::var_os("HOME").is_some_and(|h| {
        let dir = Path::new(&h).join("Library/Keychains");
        dir.join("login.keychain-db").exists() || dir.join("login.keychain").exists()
    });
    if !has_keychain {
        tracing::info!("no login keychain; keeping the secrets key in {KEY_FILE}");
        return Err(crypto_err("no keychain"));
    }
    use security_framework::passwords::{PasswordOptions, generic_password, set_generic_password};
    /// `errSecItemNotFound`.
    const NOT_FOUND: i32 = -25300;
    match generic_password(PasswordOptions::new_generic_password(
        KEYCHAIN_SERVICE,
        KEYCHAIN_ACCOUNT,
    )) {
        Ok(bytes) => decode_key(&String::from_utf8_lossy(&bytes))
            .ok_or_else(|| crypto_err("unreadable keychain entry")),
        Err(e) if e.code() == NOT_FOUND => {
            let key = new_key()?;
            set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, hex(&key).as_bytes())
                .map_err(|e| crypto_err(&e.to_string()))?;
            Ok(key)
        }
        Err(e) => {
            tracing::info!("keychain unavailable ({e}); keeping the secrets key in {KEY_FILE}");
            Err(crypto_err("keychain unavailable"))
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_key(key_file: &Path) -> Result<[u8; KEY_LEN], SecretError> {
    let _ = (KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);
    file_key(key_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> Vault {
        Vault::from_key(&[7u8; KEY_LEN]).unwrap()
    }

    #[test]
    fn round_trips_and_never_contains_the_plaintext() {
        let v = vault();
        let s = v.seal("API_KEY", "sk-very-secret").unwrap();
        assert!(is_sealed(&s) && !s.contains("secret"), "{s}");
        assert_eq!(v.open_value("API_KEY", &s).unwrap(), "sk-very-secret");
        // A fresh nonce each time.
        assert_ne!(s, v.seal("API_KEY", "sk-very-secret").unwrap());
    }

    #[test]
    fn the_name_is_bound_to_the_ciphertext() {
        let v = vault();
        let s = v.seal("A_KEY", "value").unwrap();
        assert!(matches!(
            v.open_value("B_KEY", &s),
            Err(SecretError::Decrypt(n)) if n == "B_KEY"
        ));
    }

    #[test]
    fn tampering_and_wrong_keys_fail() {
        let v = vault();
        let s = v.seal("N", "value").unwrap();
        let mut bad = s.clone();
        bad.pop();
        bad.push(if s.ends_with('0') { '1' } else { '0' });
        assert!(v.open_value("N", &bad).is_err());
        assert!(v.open_value("N", "enc:v1:zz").is_err());
        assert!(v.open_value("N", "enc:v1:").is_err());
        let other = Vault::from_key(&[8u8; KEY_LEN]).unwrap();
        assert!(other.open_value("N", &s).is_err());
    }

    #[test]
    fn plaintext_passes_through_as_legacy() {
        assert_eq!(vault().open_value("N", "plain").unwrap(), "plain");
    }

    #[test]
    fn the_key_file_is_created_private_and_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("secrets.toml");
        let a = Vault::open(&p, KeySource::File).unwrap();
        let sealed = a.seal("N", "v").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tmp.path().join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let b = Vault::open(&p, KeySource::File).unwrap();
        assert_eq!(b.open_value("N", &sealed).unwrap(), "v");
    }
}
