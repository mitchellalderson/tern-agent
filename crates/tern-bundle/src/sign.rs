//! Ed25519 signatures over a bundle's manifest, in minisign's formats
//! (tooling spec §6.5), so `minisign -Vm bundle.toml -P <key>` checks a
//! bundle tern signed and `minisign -Sm bundle.toml` signs one tern
//! accepts. Public keys are minisign's: base64 of `Ed`, an 8-byte key id
//! and the 32-byte key. Signatures may be prehashed (`ED`, BLAKE2b-512 of
//! the file, minisign's default) or legacy (`Ed`, the file itself); tern
//! writes prehashed ones.
//!
//! tern's own secret key is not minisign's (that one is scrypt-encrypted):
//! it is a PKCS#8 document in a 0600 file, like `secrets.toml`.

use std::path::Path;

use aws_lc_rs::rand::SecureRandom;
use aws_lc_rs::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use blake2::Digest;

use crate::BundleError;

/// A minisign public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey {
    pub id: [u8; 8],
    pub key: [u8; 32],
}

impl PublicKey {
    /// From the base64 line, or a whole `.pub` file (the line after the
    /// `untrusted comment:`).
    pub fn parse(s: &str) -> Result<Self, BundleError> {
        let line = s
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
            .unwrap_or_default();
        let raw = b64_decode(line)
            .filter(|r| r.len() == 42 && &r[..2] == b"Ed")
            .ok_or_else(|| {
                BundleError::Invalid(format!(
                    "{:?} isn't a minisign public key (base64 of 42 bytes starting \"Ed\", like RWQ…)",
                    short(line)
                ))
            })?;
        let mut id = [0u8; 8];
        let mut key = [0u8; 32];
        id.copy_from_slice(&raw[2..10]);
        key.copy_from_slice(&raw[10..]);
        Ok(Self { id, key })
    }

    /// The base64 line (`RWQ…`).
    pub fn to_base64(&self) -> String {
        let mut raw = b"Ed".to_vec();
        raw.extend_from_slice(&self.id);
        raw.extend_from_slice(&self.key);
        b64_encode(&raw)
    }

    /// The key id as minisign prints it: 16 hex digits of the
    /// little-endian id.
    pub fn id_hex(&self) -> String {
        format!("{:016X}", u64::from_le_bytes(self.id))
    }

    /// A `.pub` file minisign reads.
    pub fn to_file(&self) -> String {
        format!(
            "untrusted comment: minisign public key {}\n{}\n",
            self.id_hex(),
            self.to_base64()
        )
    }
}

/// tern's signing key.
pub struct SecretKey {
    pub public: PublicKey,
    pair: Ed25519KeyPair,
    pkcs8: Vec<u8>,
}

impl SecretKey {
    pub fn generate() -> Result<Self, BundleError> {
        let rng = aws_lc_rs::rand::SystemRandom::new();
        let doc = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| BundleError::Invalid("generating a key failed".into()))?;
        let mut id = [0u8; 8];
        rng.fill(&mut id)
            .map_err(|_| BundleError::Invalid("generating a key id failed".into()))?;
        Self::from_parts(id, doc.as_ref())
    }

    fn from_parts(id: [u8; 8], pkcs8: &[u8]) -> Result<Self, BundleError> {
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8)
            .map_err(|_| BundleError::Invalid("the signing key file is damaged".into()))?;
        let mut key = [0u8; 32];
        key.copy_from_slice(pair.public_key().as_ref());
        Ok(Self {
            public: PublicKey { id, key },
            pair,
            pkcs8: pkcs8.to_vec(),
        })
    }

    pub fn to_file(&self) -> String {
        let mut raw = b"Ed".to_vec();
        raw.extend_from_slice(&self.public.id);
        raw.extend_from_slice(&self.pkcs8);
        format!(
            "untrusted comment: tern bundle signing key {} (keep this file private)\n{}\n",
            self.public.id_hex(),
            b64_encode(&raw)
        )
    }

    pub fn parse(s: &str) -> Result<Self, BundleError> {
        let line = s
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
            .unwrap_or_default();
        let raw = b64_decode(line)
            .filter(|r| r.len() > 10 && &r[..2] == b"Ed")
            .ok_or_else(|| BundleError::Invalid("the signing key file is damaged".into()))?;
        let mut id = [0u8; 8];
        id.copy_from_slice(&raw[2..10]);
        Self::from_parts(id, &raw[10..])
    }

    pub fn load(path: &Path) -> Result<Self, BundleError> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            BundleError::Io(format!(
                "reading {}: {e} (make one with `tern bundle keygen`)",
                path.display()
            ))
        })?;
        Self::parse(&text)
    }

    /// Write it readable only by the user; never overwrites.
    pub fn save(&self, path: &Path) -> Result<(), BundleError> {
        if path.exists() {
            return Err(BundleError::Invalid(format!(
                "{} already exists; remove it first to make a new key",
                path.display()
            )));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| BundleError::Io(e.to_string()))?;
        }
        write_private(path, &self.to_file())
            .map_err(|e| BundleError::Io(format!("writing {}: {e}", path.display())))
    }

    /// A minisign signature file for `data` (prehashed), with `comment`
    /// as its trusted comment.
    pub fn sign(&self, data: &[u8], comment: &str) -> String {
        let comment = comment.replace(['\r', '\n'], " ");
        let sig = self.pair.sign(&blake2b(data));
        let mut first = b"ED".to_vec();
        first.extend_from_slice(&self.public.id);
        first.extend_from_slice(sig.as_ref());
        let mut global_msg = sig.as_ref().to_vec();
        global_msg.extend_from_slice(comment.as_bytes());
        let global = self.pair.sign(&global_msg);
        format!(
            "untrusted comment: signature from tern bundle key {}\n{}\ntrusted comment: {comment}\n{}\n",
            self.public.id_hex(),
            b64_encode(&first),
            b64_encode(global.as_ref())
        )
    }
}

/// A signature file, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub key_id: [u8; 8],
    prehashed: bool,
    sig: Vec<u8>,
    pub trusted_comment: String,
    global: Vec<u8>,
}

impl Signature {
    pub fn parse(text: &str) -> Result<Self, BundleError> {
        let bad =
            |why: &str| BundleError::Invalid(format!("the signature file is malformed: {why}"));
        let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
        let first = lines.next().ok_or_else(|| bad("empty"))?;
        if !first.starts_with("untrusted comment:") {
            return Err(bad("no untrusted comment line"));
        }
        let raw = lines
            .next()
            .and_then(|l| b64_decode(l.trim()))
            .filter(|r| r.len() == 74)
            .ok_or_else(|| bad("the signature line isn't 74 bytes of base64"))?;
        let prehashed = match &raw[..2] {
            b"ED" => true,
            b"Ed" => false,
            _ => return Err(bad("unknown signature algorithm")),
        };
        let comment = lines
            .next()
            .and_then(|l| l.strip_prefix("trusted comment: "))
            .ok_or_else(|| bad("no trusted comment line"))?;
        let global = lines
            .next()
            .and_then(|l| b64_decode(l.trim()))
            .filter(|g| g.len() == 64)
            .ok_or_else(|| bad("no global signature"))?;
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&raw[2..10]);
        Ok(Self {
            key_id,
            prehashed,
            sig: raw[10..].to_vec(),
            trusted_comment: comment.to_owned(),
            global,
        })
    }

    /// Check this signature over `data` with `key`: the key id, the
    /// signature itself, and the signature over the trusted comment.
    pub fn verify(&self, data: &[u8], key: &PublicKey) -> Result<(), BundleError> {
        if self.key_id != key.id {
            return Err(BundleError::BadSignature(format!(
                "it was made with key {}, not the bundle's key {}",
                PublicKey {
                    id: self.key_id,
                    key: [0; 32]
                }
                .id_hex(),
                key.id_hex()
            )));
        }
        let pk = UnparsedPublicKey::new(&ED25519, key.key);
        let ok = if self.prehashed {
            pk.verify(&blake2b(data), &self.sig)
        } else {
            pk.verify(data, &self.sig)
        };
        ok.map_err(|_| {
            BundleError::BadSignature(
                "it doesn't match bundle.toml: the manifest changed after signing, or it was signed by someone else".into(),
            )
        })?;
        let mut global_msg = self.sig.clone();
        global_msg.extend_from_slice(self.trusted_comment.as_bytes());
        pk.verify(&global_msg, &self.global).map_err(|_| {
            BundleError::BadSignature("its trusted comment was changed after signing".into())
        })
    }
}

/// Create `path` readable and writable only by the user.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(text.as_bytes())
}

fn blake2b(data: &[u8]) -> Vec<u8> {
    blake2::Blake2b512::digest(data).to_vec()
}

/// Hex SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, data)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn short(s: &str) -> String {
    s.chars().take(60).collect()
}

const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, as minisign writes it.
pub fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = u32::from(ABC.iter().position(|&a| a == c)? as u8);
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for n in 0..10 {
            let v: Vec<u8> = (0..n).map(|i: u8| i.wrapping_mul(37)).collect();
            assert_eq!(b64_decode(&b64_encode(&v)).unwrap(), v);
        }
        assert_eq!(b64_encode(b"Ed"), "RWQ=");
    }

    #[test]
    fn sign_and_verify() {
        let sk = SecretKey::generate().unwrap();
        let again = SecretKey::parse(&sk.to_file()).unwrap();
        assert_eq!(again.public, sk.public);
        let pk = PublicKey::parse(&sk.public.to_file()).unwrap();
        assert_eq!(pk, sk.public);
        assert!(pk.to_base64().starts_with("RW"));

        let sig = Signature::parse(&sk.sign(b"hello", "bundle x 1.0")).unwrap();
        assert_eq!(sig.trusted_comment, "bundle x 1.0");
        sig.verify(b"hello", &pk).unwrap();
        assert!(matches!(
            sig.verify(b"hellp", &pk),
            Err(BundleError::BadSignature(_))
        ));
        let other = SecretKey::generate().unwrap();
        assert!(sig.verify(b"hello", &other.public).is_err());

        // A changed trusted comment fails the global signature.
        let text = sk
            .sign(b"hello", "a")
            .replace("trusted comment: a", "trusted comment: b");
        assert!(
            Signature::parse(&text)
                .unwrap()
                .verify(b"hello", &pk)
                .is_err()
        );
    }

    /// A key and signature made by rsign2 0.6 (a minisign implementation:
    /// `rsign generate -W`, then `rsign sign -t "tern test vector"`) over
    /// "tern\n": tern reads minisign's formats.
    #[test]
    fn verifies_a_minisign_signature() {
        let pk = PublicKey::parse(
            "untrusted comment: minisign public key: D4CA68C37F8EBF48\nRWRIv45/w2jK1LefmtjPXZslWSos934HqJTfC+771rZu9uYW3mo8hdOX\n",
        )
        .unwrap();
        assert_eq!(pk.id_hex(), "D4CA68C37F8EBF48");
        let sig = Signature::parse(
            "untrusted comment: signature from rsign secret key\nRURIv45/w2jK1NTPloRi8yIAi/GZt0tlf4/e5TkGpJ1K2VN9Pf0BIYaf7KMtLI+296O5P0TfA556sWrEPNfH43BxCP2sF3irqg8=\ntrusted comment: tern test vector\nEH1MESzoGA0J4GqQnGOxjL1CKzreAmBPN6Gstj2qq1DlyfARxS35Gg7v5W3bahl/J5Of/FzW8ki8Nrc/ySlEAA==\n",
        )
        .unwrap();
        sig.verify(b"tern\n", &pk).unwrap();
        assert!(sig.verify(b"tern", &pk).is_err());
    }

    /// Legacy (`Ed`, not prehashed) signatures, which `minisign -l` makes.
    #[test]
    fn verifies_legacy_signatures() {
        // Build a legacy (`Ed`) signature by hand and check it verifies.
        let sk = SecretKey::generate().unwrap();
        let sig = sk.pair.sign(b"data");
        let mut first = b"Ed".to_vec();
        first.extend_from_slice(&sk.public.id);
        first.extend_from_slice(sig.as_ref());
        let mut g = sig.as_ref().to_vec();
        g.extend_from_slice(b"c");
        let global = sk.pair.sign(&g);
        let text = format!(
            "untrusted comment: x\n{}\ntrusted comment: c\n{}\n",
            b64_encode(&first),
            b64_encode(global.as_ref())
        );
        Signature::parse(&text)
            .unwrap()
            .verify(b"data", &sk.public)
            .unwrap();
    }
}
