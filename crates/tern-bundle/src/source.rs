//! Reading a bundle from a directory, a `.tar.zst`, or a URL, and checking
//! it: the pinned hash first, then every file against the manifest, then
//! the signature (tooling spec §6.5).

use std::collections::BTreeMap;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::Url;

use crate::BundleError;
use crate::manifest::{MANIFEST, MAX_FILE_BYTES, MAX_FILES, MAX_TOTAL_BYTES, Manifest, SIGNATURE};
use crate::sign::{PublicKey, Signature, sha256_hex};

/// Largest archive tern downloads.
pub const MAX_ARCHIVE_BYTES: usize = 8 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: usize = 5;

/// Where a bundle came from, as the lockfile records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Url(String),
    Path(PathBuf),
    /// Made by tern itself (an import); nothing to fetch again.
    Generated(String),
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::Url(u) => f.write_str(u),
            Origin::Path(p) => write!(f, "{}", p.display()),
            Origin::Generated(s) => f.write_str(s),
        }
    }
}

/// A bundle's bytes, read but not yet checked.
#[derive(Debug, Clone)]
pub struct Raw {
    pub origin: Origin,
    /// The hash that pins it: SHA-256 of the archive as downloaded or read,
    /// or of `bundle.toml` for a directory (which lists every file's hash).
    pub sha256: String,
    /// Every file, by its path in the bundle, including the manifest and
    /// signature.
    pub files: BTreeMap<String, Vec<u8>>,
}

/// How a bundle is signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signed {
    /// No signature: installs with a warning.
    No,
    /// A valid signature by `key`.
    By(PublicKey),
}

/// A checked bundle: the files match the manifest and the signature, if
/// any, is valid. Whether its key is trusted is decided by the caller.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub origin: Origin,
    pub sha256: String,
    pub manifest: Manifest,
    pub manifest_text: String,
    pub signature: Option<String>,
    pub signed: Signed,
    /// The files the manifest lists (not the manifest or signature).
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Bundle {
    pub fn file_text(&self, path: &str) -> Option<String> {
        self.files
            .get(path)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }
}

/// `https://…/x.tar.zst#sha256=<hex>`: the URL and the pin in it.
pub fn split_pin(source: &str) -> (String, Option<String>) {
    match source.rsplit_once("#sha256=") {
        Some((url, pin)) => (url.to_owned(), Some(pin.trim().to_ascii_lowercase())),
        None => (source.to_owned(), None),
    }
}

pub fn is_url(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("http://")
}

/// Read a local directory or archive.
pub fn read_path(path: &Path) -> Result<Raw, BundleError> {
    let origin = Origin::Path(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| BundleError::Io(format!("{}: {e}", path.display())))?;
    if meta.is_dir() {
        let files = read_dir(path)?;
        let sha256 = files
            .get(MANIFEST)
            .map(|m| sha256_hex(m))
            .ok_or_else(|| BundleError::Invalid(format!("{} has no {MANIFEST}", path.display())))?;
        return Ok(Raw {
            origin,
            sha256,
            files,
        });
    }
    if meta.len() as usize > MAX_ARCHIVE_BYTES {
        return Err(BundleError::Invalid(format!(
            "{} is larger than {MAX_ARCHIVE_BYTES} bytes",
            path.display()
        )));
    }
    let bytes =
        std::fs::read(path).map_err(|e| BundleError::Io(format!("{}: {e}", path.display())))?;
    unpack(origin, &bytes)
}

fn read_dir(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, BundleError> {
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = root.join(&rel);
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| BundleError::Io(format!("{}: {e}", dir.display())))?
            .flatten()
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            // Version control and editor droppings aren't part of a bundle.
            if rel.as_os_str().is_empty() && (name == ".git" || name == ".DS_Store") {
                continue;
            }
            let rel_path = rel.join(&name);
            let key = rel_path.to_string_lossy().replace('\\', "/");
            let ft = e.file_type().map_err(|e| BundleError::Io(e.to_string()))?;
            if ft.is_symlink() {
                return Err(BundleError::Invalid(format!(
                    "{key} is a symbolic link; bundles hold plain files only"
                )));
            }
            if ft.is_dir() {
                stack.push(rel_path);
                continue;
            }
            let bytes =
                std::fs::read(e.path()).map_err(|err| BundleError::Io(format!("{key}: {err}")))?;
            add(&mut files, &mut total, key, bytes)?;
        }
    }
    Ok(files)
}

fn add(
    files: &mut BTreeMap<String, Vec<u8>>,
    total: &mut usize,
    key: String,
    bytes: Vec<u8>,
) -> Result<(), BundleError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(BundleError::Invalid(format!(
            "{key} is larger than {MAX_FILE_BYTES} bytes"
        )));
    }
    *total += bytes.len();
    if *total > MAX_TOTAL_BYTES || files.len() >= MAX_FILES + 2 {
        return Err(BundleError::Invalid(format!(
            "the bundle is larger than {MAX_TOTAL_BYTES} bytes or {MAX_FILES} files"
        )));
    }
    files.insert(key, bytes);
    Ok(())
}

/// Unpack a `.tar.zst` in memory. Only regular files and directories are
/// accepted; links, devices and paths leaving the bundle are refused.
pub fn unpack(origin: Origin, bytes: &[u8]) -> Result<Raw, BundleError> {
    let sha256 = sha256_hex(bytes);
    let decoder = ruzstd::decoding::StreamingDecoder::new(bytes)
        .map_err(|e| BundleError::Invalid(format!("not a zstd archive: {e}")))?;
    // Bound decompression as well as the files in it.
    let limited = decoder.take((MAX_TOTAL_BYTES + MAX_FILES * 1024 + 64 * 1024) as u64);
    let mut archive = tar::Archive::new(limited);
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    let entries = archive
        .entries()
        .map_err(|e| BundleError::Invalid(format!("reading the archive: {e}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|e| BundleError::Invalid(format!("reading the archive: {e}")))?;
        let kind = entry.header().entry_type();
        let path = entry
            .path()
            .map_err(|e| BundleError::Invalid(format!("an archive path: {e}")))?
            .to_string_lossy()
            .replace('\\', "/");
        let path = path.trim_start_matches("./").to_owned();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(BundleError::Invalid(format!(
                "{path} isn't a plain file (links and devices are refused)"
            )));
        }
        if path.split('/').any(|p| p == ".." || p.is_empty()) || path.starts_with('/') {
            return Err(BundleError::Invalid(format!("{path} leaves the bundle")));
        }
        if entry.size() as usize > MAX_FILE_BYTES {
            return Err(BundleError::Invalid(format!(
                "{path} is larger than {MAX_FILE_BYTES} bytes"
            )));
        }
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| BundleError::Invalid(format!("{path}: {e}")))?;
        if files.contains_key(&path) {
            return Err(BundleError::Invalid(format!(
                "{path} is in the archive twice"
            )));
        }
        add(&mut files, &mut total, path, buf)?;
    }
    Ok(Raw {
        origin,
        sha256,
        files,
    })
}

/// Pack files (manifest and signature included) as a `.tar.zst`, the same
/// bytes every time for the same files.
pub fn pack(files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, BundleError> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, bytes) in files {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, path, bytes.as_slice())
            .map_err(|e| BundleError::Io(format!("packing {path}: {e}")))?;
    }
    let tar = builder
        .into_inner()
        .map_err(|e| BundleError::Io(format!("packing: {e}")))?;
    Ok(ruzstd::encoding::compress_to_vec(
        tar.as_slice(),
        ruzstd::encoding::CompressionLevel::Fastest,
    ))
}

/// Check `raw` against an expected pin, then the manifest, then the
/// signature. A bad signature, a changed file or a wrong pin is an error,
/// never a warning; only a missing signature is allowed.
pub fn verify(raw: Raw, pin: Option<&str>) -> Result<Bundle, BundleError> {
    if let Some(pin) = pin {
        let pin = pin
            .trim()
            .trim_start_matches("sha256:")
            .to_ascii_lowercase();
        if pin != raw.sha256 {
            return Err(BundleError::HashMismatch {
                expected: pin,
                got: raw.sha256,
            });
        }
    }
    let Raw {
        origin,
        sha256,
        mut files,
    } = raw;
    let manifest_bytes = files
        .remove(MANIFEST)
        .ok_or_else(|| BundleError::Invalid(format!("no {MANIFEST} in the bundle")))?;
    let manifest_text = String::from_utf8(manifest_bytes.clone())
        .map_err(|_| BundleError::Invalid(format!("{MANIFEST} isn't UTF-8")))?;
    let manifest = Manifest::parse(&manifest_text)?;
    let signature = files
        .remove(SIGNATURE)
        .map(|b| String::from_utf8_lossy(&b).into_owned());

    for f in &manifest.files {
        let bytes = files.get(&f.path).ok_or_else(|| {
            BundleError::Invalid(format!(
                "{MANIFEST} lists {}, which isn't in the bundle",
                f.path
            ))
        })?;
        let got = sha256_hex(bytes);
        if !got.eq_ignore_ascii_case(&f.sha256) {
            return Err(BundleError::FileChanged(f.path.clone()));
        }
    }
    if let Some(extra) = files
        .keys()
        .find(|p| !manifest.files.iter().any(|f| &f.path == *p))
    {
        return Err(BundleError::Invalid(format!(
            "{extra} is in the bundle but not in {MANIFEST}, so neither its hash nor the signature covers it"
        )));
    }

    let signed = match (&manifest.bundle.key, &signature) {
        (_, None) => Signed::No,
        (None, Some(_)) => {
            return Err(BundleError::BadSignature(format!(
                "there is a {SIGNATURE} but {MANIFEST} names no key to check it with"
            )));
        }
        (Some(key), Some(sig)) => {
            let key = PublicKey::parse(key)?;
            Signature::parse(sig)?.verify(&manifest_bytes, &key)?;
            Signed::By(key)
        }
    };
    Ok(Bundle {
        origin,
        sha256,
        manifest,
        manifest_text,
        signature,
        signed,
        files,
    })
}

/// How URLs are fetched: https only, public addresses only (the client is
/// pinned to the checked addresses), redirects followed by hand with each
/// hop checked again, and a size cap. `allow_private` is for tests and
/// for `[bundles] allow_private_hosts`.
#[derive(Debug, Clone, Default)]
pub struct Fetcher {
    pub allow_private: bool,
}

impl Fetcher {
    pub async fn fetch(&self, url: &str) -> Result<Raw, BundleError> {
        let mut url = Url::parse(url).map_err(|e| BundleError::Fetch(format!("{url}: {e}")))?;
        let original = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            let client = self.client(&url).await?;
            let resp = client
                .get(url.clone())
                .send()
                .await
                .map_err(|e| BundleError::Fetch(format!("{}: {e}", origin_of(&url))))?;
            let status = resp.status();
            if status.is_redirection() {
                let next = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|l| l.to_str().ok())
                    .and_then(|l| url.join(l).ok())
                    .ok_or_else(|| {
                        BundleError::Fetch(format!(
                            "{}: a redirect with no location",
                            origin_of(&url)
                        ))
                    })?;
                url = next;
                continue;
            }
            if !status.is_success() {
                return Err(BundleError::Fetch(format!(
                    "{}: HTTP {status}",
                    origin_of(&url)
                )));
            }
            let bytes = read_capped(resp).await?;
            return unpack(Origin::Url(original), &bytes);
        }
        Err(BundleError::Fetch(format!(
            "{original}: more than {MAX_REDIRECTS} redirects"
        )))
    }

    async fn client(&self, url: &Url) -> Result<reqwest::Client, BundleError> {
        let local_http = self.allow_private && url.scheme() == "http";
        if url.scheme() != "https" && !local_http {
            return Err(BundleError::Fetch(format!(
                "{} isn't https; bundles are only fetched over https",
                origin_of(url)
            )));
        }
        let host = url
            .host_str()
            .ok_or_else(|| BundleError::Fetch(format!("{url} has no host")))?;
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let builder = tern_core::net::http_client().timeout(FETCH_TIMEOUT);
        if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
            if !self.allow_private && !tern_core::net::is_public(ip) {
                return Err(private(host));
            }
            return builder
                .build()
                .map_err(|e| BundleError::Fetch(e.to_string()));
        }
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((bare, port))
            .await
            .map_err(|e| BundleError::Fetch(format!("resolving {host}: {e}")))?
            .collect();
        if addrs.is_empty() {
            return Err(BundleError::Fetch(format!("{host} did not resolve")));
        }
        if !self.allow_private && addrs.iter().any(|a| !tern_core::net::is_public(a.ip())) {
            return Err(private(host));
        }
        builder
            .resolve_to_addrs(bare, &addrs)
            .build()
            .map_err(|e| BundleError::Fetch(e.to_string()))
    }
}

fn private(host: &str) -> BundleError {
    BundleError::Fetch(format!(
        "{host} is a private, loopback or link-local address; bundles are fetched from the public internet (download it and install the file instead)"
    ))
}

fn origin_of(u: &Url) -> String {
    tern_auth::http::origin(u)
}

async fn read_capped(mut resp: reqwest::Response) -> Result<Vec<u8>, BundleError> {
    if resp
        .content_length()
        .is_some_and(|n| n as usize > MAX_ARCHIVE_BYTES)
    {
        return Err(BundleError::Fetch(format!(
            "the bundle is larger than {MAX_ARCHIVE_BYTES} bytes"
        )));
    }
    let mut raw = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| BundleError::Fetch(e.to_string()))?
    {
        if raw.len() + chunk.len() > MAX_ARCHIVE_BYTES {
            return Err(BundleError::Fetch(format!(
                "the bundle is larger than {MAX_ARCHIVE_BYTES} bytes"
            )));
        }
        raw.extend_from_slice(&chunk);
    }
    Ok(raw)
}

/// Make a bundle's files from its header and contents: `bundle.toml` with
/// every file's hash and, with `key`, `bundle.toml.minisig`.
pub fn assemble(
    mut header: crate::manifest::Header,
    files: BTreeMap<String, Vec<u8>>,
    key: Option<&crate::sign::SecretKey>,
) -> Result<BTreeMap<String, Vec<u8>>, BundleError> {
    header.key = key.map(|k| k.public.to_base64());
    let manifest = Manifest {
        files: files
            .iter()
            .map(|(path, bytes)| crate::manifest::FileEntry {
                path: path.clone(),
                sha256: sha256_hex(bytes),
            })
            .collect(),
        bundle: header,
    };
    // Checked as an installer would check it.
    let text = manifest.to_toml()?;
    Manifest::parse(&text)?;
    let mut out = files;
    if let Some(k) = key {
        let comment = format!(
            "bundle {} {} signed {}",
            manifest.bundle.name,
            manifest.bundle.version,
            chrono::Utc::now().format("%Y-%m-%d")
        );
        out.insert(
            SIGNATURE.into(),
            k.sign(text.as_bytes(), &comment).into_bytes(),
        );
    }
    out.insert(MANIFEST.into(), text.into_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Header;
    use crate::sign::SecretKey;

    fn header() -> Header {
        Header {
            name: "demo".into(),
            version: "1.0.0".into(),
            description: "a demo".into(),
            author: "test".into(),
            key: None,
        }
    }

    fn files() -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            (
                "skills/hello/SKILL.md".to_owned(),
                b"---\nname: hello\ndescription: says hello\n---\nSay hello.\n".to_vec(),
            ),
            ("README.md".to_owned(), b"# demo\n".to_vec()),
        ])
    }

    fn raw(files: BTreeMap<String, Vec<u8>>) -> Raw {
        let packed = pack(&files).unwrap();
        unpack(Origin::Generated("test".into()), &packed).unwrap()
    }

    #[test]
    fn signature_valid() {
        let key = SecretKey::generate().unwrap();
        let b = verify(raw(assemble(header(), files(), Some(&key)).unwrap()), None).unwrap();
        assert_eq!(b.signed, Signed::By(key.public));
        assert_eq!(b.files.len(), 2);
    }

    #[test]
    fn signature_missing_is_unsigned() {
        let b = verify(raw(assemble(header(), files(), None).unwrap()), None).unwrap();
        assert_eq!(b.signed, Signed::No);
        // A key in the manifest without its signature file: still unsigned.
        let key = SecretKey::generate().unwrap();
        let mut f = assemble(header(), files(), Some(&key)).unwrap();
        f.remove(SIGNATURE);
        assert_eq!(verify(raw(f), None).unwrap().signed, Signed::No);
    }

    #[test]
    fn signature_invalid_is_refused() {
        let key = SecretKey::generate().unwrap();
        let other = SecretKey::generate().unwrap();
        // Re-signed by someone else, keeping the original key in the manifest.
        let mut f = assemble(header(), files(), Some(&key)).unwrap();
        let manifest = f[MANIFEST].clone();
        f.insert(SIGNATURE.into(), other.sign(&manifest, "x").into_bytes());
        assert!(matches!(
            verify(raw(f), None),
            Err(BundleError::BadSignature(_))
        ));
        // The manifest edited after signing (its key swapped for another).
        let mut f = assemble(header(), files(), Some(&key)).unwrap();
        let text = String::from_utf8(f[MANIFEST].clone())
            .unwrap()
            .replace("a demo", "a friendly demo");
        f.insert(MANIFEST.into(), text.into_bytes());
        assert!(matches!(
            verify(raw(f), None),
            Err(BundleError::BadSignature(_))
        ));
        // A signature with no key to check it.
        let mut f = assemble(header(), files(), None).unwrap();
        f.insert(
            SIGNATURE.into(),
            key.sign(&f[MANIFEST].clone(), "x").into_bytes(),
        );
        assert!(matches!(
            verify(raw(f), None),
            Err(BundleError::BadSignature(_))
        ));
    }

    #[test]
    fn hash_mismatch_is_refused() {
        let f = assemble(header(), files(), None).unwrap();
        let r = raw(f.clone());
        let sha = r.sha256.clone();
        assert!(verify(r, Some(&sha)).is_ok());
        assert!(verify(raw(f.clone()), Some(&format!("sha256:{sha}"))).is_ok());
        assert!(matches!(
            verify(raw(f.clone()), Some(&"0".repeat(64))),
            Err(BundleError::HashMismatch { .. })
        ));
        // A file changed after the manifest was made.
        let mut f = f;
        f.insert("README.md".into(), b"# changed\n".to_vec());
        assert!(
            matches!(verify(raw(f), None), Err(BundleError::FileChanged(p)) if p == "README.md")
        );
    }

    #[test]
    fn unlisted_files_are_refused() {
        let mut f = assemble(header(), files(), None).unwrap();
        f.insert("skills/hello/extra.md".into(), b"hi".to_vec());
        assert!(verify(raw(f), None).is_err());
    }

    #[test]
    fn packing_is_deterministic_and_directories_read_the_same() {
        let f = assemble(header(), files(), None).unwrap();
        assert_eq!(pack(&f).unwrap(), pack(&f).unwrap());
        let dir = tempfile::tempdir().unwrap();
        for (p, b) in &f {
            let path = dir.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b).unwrap();
        }
        let from_dir = read_path(dir.path()).unwrap();
        assert_eq!(from_dir.files, f);
        assert_eq!(from_dir.sha256, sha256_hex(&f[MANIFEST]));
        let archive = dir.path().join("x.tar.zst");
        std::fs::write(&archive, pack(&f).unwrap()).unwrap();
        assert_eq!(read_path(&archive).unwrap().files, f);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST), "x").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("README.md")).unwrap();
        assert!(read_path(dir.path()).is_err());
    }

    #[test]
    fn archive_links_and_escapes_are_refused() {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        b.append_link(&mut h, "README.md", "/etc/passwd").unwrap();
        let tar = b.into_inner().unwrap();
        let z = ruzstd::encoding::compress_to_vec(
            tar.as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        assert!(unpack(Origin::Generated("t".into()), &z).is_err());
        assert!(unpack(Origin::Generated("t".into()), b"not zstd").is_err());
    }

    #[tokio::test]
    async fn fetch_is_https_and_public_only() {
        let f = Fetcher::default();
        let e = f.fetch("http://example.com/x.tar.zst").await.unwrap_err();
        assert!(e.to_string().contains("https"), "{e}");
        let e = f.fetch("https://127.0.0.1/x.tar.zst").await.unwrap_err();
        assert!(e.to_string().contains("private"), "{e}");
    }

    #[tokio::test]
    async fn fetch_follows_redirects_checking_each() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let bytes = pack(&assemble(header(), files(), None).unwrap()).unwrap();
        Mock::given(path("/r"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/b.tar.zst"))
            .mount(&server)
            .await;
        Mock::given(path("/b.tar.zst"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
            .mount(&server)
            .await;
        let f = Fetcher {
            allow_private: true,
        };
        let raw = f.fetch(&format!("{}/r", server.uri())).await.unwrap();
        assert_eq!(raw.sha256, sha256_hex(&bytes));
        // Without the test escape the same local server is refused.
        assert!(
            Fetcher::default()
                .fetch(&format!("{}/r", server.uri()))
                .await
                .is_err()
        );
    }
}
