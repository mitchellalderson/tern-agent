//! "Install Ollama for me": download the official app, check it, put it in
//! `~/Applications`. Runs only after the person agreed, says what it does,
//! and installs nothing it couldn't verify:
//!
//! 1. the release comes from Ollama's GitHub (`api.github.com`), over https;
//! 2. the download host must be GitHub's, and its SHA-256 must equal the
//!    `digest` GitHub lists for the file (no digest: no install);
//! 3. the unpacked app must pass `codesign --verify`, be accepted by
//!    Gatekeeper (`spctl`, so signed and notarized), and be signed by a
//!    Developer ID whose name says Ollama.
//!
//! The system's tools are behind [`Tools`], so tests never run them.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};

const RELEASE_API: &str = "https://api.github.com/repos/ollama/ollama/releases/latest";
const ASSET: &str = "Ollama-darwin.zip";
/// Hosts a download may come from: GitHub and its release storage.
const ALLOWED_HOSTS: &[&str] = &["github.com", "githubusercontent.com"];
/// The app is about 230 MB; anything near a gigabyte is not it.
const MAX_BYTES: u64 = 1 << 30;

/// What "Install for me" would do, shown before the person agrees.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Plan {
    /// Where it comes from.
    pub source: String,
    /// Where it goes.
    pub destination: String,
    /// Said in a sentence, to show beside the button.
    pub summary: String,
    /// Possible on this computer (macOS only).
    pub available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Asking GitHub which file.
    Fetching,
    Downloading,
    /// Checksum and signature.
    Verifying,
    Installing,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Idle,
    Working {
        phase: Phase,
        completed: u64,
        total: u64,
    },
    Done {
        /// Who signed the app, as the system reports it.
        signer: String,
    },
    Failed {
        message: String,
    },
}

impl State {
    pub fn busy(&self) -> bool {
        matches!(self, Self::Working { .. })
    }
}

/// The system's own tools: unpacking and checking an app.
#[async_trait]
pub trait Tools: Send + Sync {
    /// Unpack `zip` into `into`; returns the `.app` inside.
    async fn extract(&self, zip: &Path, into: &Path) -> Result<PathBuf, String>;
    /// Check the signature and Gatekeeper's verdict; returns who signed it.
    async fn verify(&self, app: &Path) -> Result<String, String>;
    /// Move the app to `dest`.
    async fn place(&self, app: &Path, dest: &Path) -> Result<(), String>;
}

/// The result of a finished install.
#[derive(Debug)]
pub struct Installed {
    pub app: PathBuf,
    pub signer: String,
}

pub struct Installer {
    api_url: String,
    allowed_hosts: Vec<String>,
    work_dir: PathBuf,
    dest: PathBuf,
    tools: Box<dyn Tools>,
    available: bool,
}

impl Installer {
    pub fn system() -> Self {
        let home = tern_core::home::user_home().unwrap_or_else(|| PathBuf::from("."));
        let apps = std::env::var_os("TERN_LOCAL_APPS")
            .and_then(|v| std::env::split_paths(&v).next())
            .unwrap_or_else(|| home.join("Applications"));
        Self {
            api_url: RELEASE_API.into(),
            allowed_hosts: ALLOWED_HOSTS.iter().map(|h| (*h).to_owned()).collect(),
            work_dir: std::env::temp_dir(),
            dest: apps.join("Ollama.app"),
            tools: Box::new(MacTools),
            available: cfg!(target_os = "macos"),
        }
    }

    #[cfg(test)]
    pub fn new(
        api_url: &str,
        allowed_hosts: &[&str],
        work_dir: PathBuf,
        dest: PathBuf,
        tools: Box<dyn Tools>,
    ) -> Self {
        Self {
            api_url: api_url.into(),
            allowed_hosts: allowed_hosts.iter().map(|h| (*h).to_owned()).collect(),
            work_dir,
            dest,
            tools,
            available: true,
        }
    }

    pub fn plan(&self) -> Plan {
        let dest = self.dest.display().to_string();
        Plan {
            source: "github.com/ollama/ollama (Ollama's official release)".into(),
            summary: format!(
                "Downloads Ollama from its official GitHub release (about 230 MB), checks its checksum and Apple signature, and puts it in {dest}. Nothing else changes."
            ),
            destination: dest,
            available: self.available,
        }
    }

    /// Do it. `progress(phase, done, total)` is called as it goes.
    pub async fn run(
        &self,
        progress: &(dyn Fn(Phase, u64, u64) + Send + Sync),
    ) -> Result<Installed, InstallError> {
        if !self.available {
            return Err(InstallError("Install for me works on macOS only.".into()));
        }
        if self.dest.exists() {
            return Err(InstallError(format!(
                "{} is already there.",
                self.dest.display()
            )));
        }
        let client = tern_core::net::http_client()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30 * 60))
            .build()
            .map_err(|e| InstallError(format!("http client: {e}")))?;
        progress(Phase::Fetching, 0, 0);
        let asset = self.find_asset(&client).await?;
        let dir = self.work_dir.join(format!(
            "tern-ollama-install-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).map_err(|e| InstallError(format!("temp folder: {e}")))?;
        let result = self.install_from(&client, &asset, &dir, progress).await;
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    async fn find_asset(&self, client: &reqwest::Client) -> Result<Asset, InstallError> {
        check_source(&self.api_url, &self.allowed_hosts)?;
        let release: serde_json::Value = client
            .get(&self.api_url)
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| InstallError(format!("couldn't reach GitHub: {e}")))?
            .error_for_status()
            .map_err(|e| InstallError(format!("GitHub said no: {e}")))?
            .json()
            .await
            .map_err(|e| InstallError(format!("unexpected answer from GitHub: {e}")))?;
        let a = release["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|a| a["name"] == ASSET))
            .ok_or_else(|| InstallError(format!("the latest release has no {ASSET}")))?;
        let url = a["browser_download_url"]
            .as_str()
            .ok_or_else(|| InstallError("the release has no download link".into()))?;
        check_source(url, &self.allowed_hosts)?;
        // No checksum, no install: there'd be nothing to check it against.
        let sha256 = a["digest"]
            .as_str()
            .and_then(|d| d.strip_prefix("sha256:"))
            .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| {
                InstallError(
                    "GitHub lists no checksum for this release, so tern won't install it. Download Ollama from ollama.com instead."
                        .into(),
                )
            })?
            .to_ascii_lowercase();
        Ok(Asset {
            url: url.to_owned(),
            size: a["size"].as_u64().unwrap_or(0),
            sha256,
        })
    }

    async fn install_from(
        &self,
        client: &reqwest::Client,
        asset: &Asset,
        dir: &Path,
        progress: &(dyn Fn(Phase, u64, u64) + Send + Sync),
    ) -> Result<Installed, InstallError> {
        let zip = dir.join(ASSET);
        self.download(client, asset, &zip, progress).await?;
        progress(Phase::Verifying, 0, 0);
        let app = self
            .tools
            .extract(&zip, &dir.join("unpacked"))
            .await
            .map_err(|e| InstallError(format!("couldn't unpack the download: {e}")))?;
        let signer = self.tools.verify(&app).await.map_err(|e| {
            InstallError(format!(
                "the download failed its signature check, so it was not installed: {e}"
            ))
        })?;
        progress(Phase::Installing, 0, 0);
        if let Some(parent) = self.dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| InstallError(format!("couldn't create {}: {e}", parent.display())))?;
        }
        self.tools.place(&app, &self.dest).await.map_err(|e| {
            InstallError(format!("couldn't put it in {}: {e}", self.dest.display()))
        })?;
        Ok(Installed {
            app: self.dest.clone(),
            signer,
        })
    }

    async fn download(
        &self,
        client: &reqwest::Client,
        asset: &Asset,
        to: &Path,
        progress: &(dyn Fn(Phase, u64, u64) + Send + Sync),
    ) -> Result<(), InstallError> {
        use tokio::io::AsyncWriteExt;
        // GitHub sends release files to its storage host: follow a few
        // redirects by hand, each one checked against the allowed hosts.
        let mut url = asset.url.clone();
        let mut resp = None;
        for _ in 0..4 {
            let r = client
                .get(&url)
                .send()
                .await
                .map_err(|e| InstallError(format!("download failed: {e}")))?;
            if r.status().is_redirection() {
                let next = r
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| InstallError("bad redirect from GitHub".into()))?;
                url = next.to_owned();
                check_source(&url, &self.allowed_hosts)?;
                continue;
            }
            resp = Some(
                r.error_for_status()
                    .map_err(|e| InstallError(format!("download failed: {e}")))?,
            );
            break;
        }
        let resp = resp.ok_or_else(|| InstallError("too many redirects".into()))?;
        let total = resp.content_length().unwrap_or(asset.size);
        if total > MAX_BYTES {
            return Err(InstallError(
                "the download is far bigger than Ollama".into(),
            ));
        }
        let mut file = tokio::fs::File::create(to)
            .await
            .map_err(|e| InstallError(format!("temp file: {e}")))?;
        let mut hash = Sha256::new();
        let mut done = 0u64;
        let mut body = resp.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| InstallError(format!("download failed: {e}")))?;
            done += chunk.len() as u64;
            if done > MAX_BYTES {
                return Err(InstallError(
                    "the download is far bigger than Ollama".into(),
                ));
            }
            hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| InstallError(format!("writing the download: {e}")))?;
            progress(Phase::Downloading, done, total);
        }
        file.flush()
            .await
            .map_err(|e| InstallError(format!("writing the download: {e}")))?;
        let got = format!("{:x}", hash.finalize());
        if got != asset.sha256 {
            return Err(InstallError(
                "the download doesn't match GitHub's checksum, so it was not installed".into(),
            ));
        }
        Ok(())
    }
}

struct Asset {
    url: String,
    size: u64,
    sha256: String,
}

/// An install that couldn't finish, in words to show.
#[derive(Debug)]
pub struct InstallError(pub String);

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InstallError {}

/// https only (plain http only to this computer, for tests), and a host on
/// the list.
fn check_source(url: &str, hosts: &[String]) -> Result<(), InstallError> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| InstallError(format!("{url:?} isn't a web address")))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or("");
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(h, _)| h)
        .to_ascii_lowercase();
    let loopback = host == "127.0.0.1" || host == "localhost";
    let allowed = hosts
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")));
    let secure = scheme == "https" || (scheme == "http" && loopback);
    if !secure || !allowed {
        return Err(InstallError(format!(
            "won't download from {host}: only Ollama's GitHub release is allowed"
        )));
    }
    Ok(())
}

/// The macOS tools: `ditto`, `codesign`, `spctl`.
struct MacTools;

async fn run(cmd: &str, args: &[&str]) -> Result<(String, String), String> {
    let out = tokio::process::Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("{cmd}: {e}"))?;
    let (o, e) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    if out.status.success() {
        Ok((o, e))
    } else {
        Err(format!("{cmd}: {}", e.trim()))
    }
}

#[async_trait]
impl Tools for MacTools {
    async fn extract(&self, zip: &Path, into: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
        run(
            "/usr/bin/ditto",
            &["-x", "-k", &zip.to_string_lossy(), &into.to_string_lossy()],
        )
        .await?;
        let app = into.join("Ollama.app");
        app.is_dir()
            .then_some(app)
            .ok_or_else(|| "no Ollama.app inside".into())
    }

    async fn verify(&self, app: &Path) -> Result<String, String> {
        let p = app.to_string_lossy();
        run("/usr/bin/codesign", &["--verify", "--deep", "--strict", &p]).await?;
        // Gatekeeper's verdict: signed with a Developer ID and notarized.
        run("/usr/sbin/spctl", &["--assess", "--type", "execute", &p]).await?;
        let (_, info) = run("/usr/bin/codesign", &["-dv", "--verbose=2", &p]).await?;
        let signer = info
            .lines()
            .find_map(|l| l.strip_prefix("Authority=Developer ID Application:"))
            .map(|s| s.trim().to_owned())
            .ok_or("it isn't signed with a Developer ID")?;
        if !signer.to_ascii_lowercase().contains("ollama") {
            return Err(format!("it is signed by {signer}, not by Ollama"));
        }
        Ok(signer)
    }

    async fn place(&self, app: &Path, dest: &Path) -> Result<(), String> {
        if std::fs::rename(app, dest).is_ok() {
            return Ok(());
        }
        // Another volume: copy, keeping the signature's attributes.
        run(
            "/usr/bin/ditto",
            &[&app.to_string_lossy(), &dest.to_string_lossy()],
        )
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Records what it was asked, touches nothing.
    #[derive(Default)]
    struct Fake {
        calls: std::sync::Arc<Mutex<Vec<&'static str>>>,
        reject: bool,
    }

    #[async_trait]
    impl Tools for Fake {
        async fn extract(&self, _zip: &Path, into: &Path) -> Result<PathBuf, String> {
            self.calls.lock().unwrap().push("extract");
            Ok(into.join("Ollama.app"))
        }
        async fn verify(&self, _app: &Path) -> Result<String, String> {
            self.calls.lock().unwrap().push("verify");
            if self.reject {
                Err("not signed".into())
            } else {
                Ok("Ollama Inc. (TEAM)".into())
            }
        }
        async fn place(&self, _app: &Path, dest: &Path) -> Result<(), String> {
            self.calls.lock().unwrap().push("place");
            std::fs::create_dir_all(dest).map_err(|e| e.to_string())
        }
    }

    const BODY: &[u8] = b"pretend this is a zip";

    fn sha(b: &[u8]) -> String {
        format!("{:x}", Sha256::digest(b))
    }

    async fn release(server: &MockServer, digest: Option<String>) {
        let mut asset = json!({
            "name": ASSET,
            "size": BODY.len(),
            "browser_download_url": format!("{}/dl/{ASSET}", server.uri()),
        });
        if let Some(d) = digest {
            asset["digest"] = json!(d);
        }
        Mock::given(method("GET"))
            .and(path("/release"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "assets": [asset] })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/dl/{ASSET}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
            .mount(server)
            .await;
    }

    fn installer(server: &MockServer, tmp: &Path, tools: Fake) -> Installer {
        Installer::new(
            &format!("{}/release", server.uri()),
            &["127.0.0.1"],
            tmp.join("work"),
            tmp.join("Applications/Ollama.app"),
            Box::new(tools),
        )
    }

    #[tokio::test]
    async fn downloads_checks_and_installs() {
        let server = MockServer::start().await;
        release(&server, Some(format!("sha256:{}", sha(BODY)))).await;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("work")).unwrap();
        let fake = Fake::default();
        let calls = fake.calls.clone();
        let phases = Mutex::new(Vec::new());
        let done = installer(&server, tmp.path(), fake)
            .run(&|p, _, _| {
                let mut v = phases.lock().unwrap();
                if v.last() != Some(&p) {
                    v.push(p);
                }
            })
            .await
            .unwrap();
        assert_eq!(done.signer, "Ollama Inc. (TEAM)");
        assert!(tmp.path().join("Applications/Ollama.app").exists());
        assert_eq!(*calls.lock().unwrap(), ["extract", "verify", "place"]);
        assert_eq!(
            *phases.lock().unwrap(),
            [
                Phase::Fetching,
                Phase::Downloading,
                Phase::Verifying,
                Phase::Installing
            ]
        );
        // The temp folder is gone.
        assert_eq!(
            std::fs::read_dir(tmp.path().join("work")).unwrap().count(),
            0
        );
    }

    #[tokio::test]
    async fn a_wrong_checksum_installs_nothing() {
        let server = MockServer::start().await;
        release(&server, Some(format!("sha256:{}", sha(b"something else")))).await;
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let calls = fake.calls.clone();
        let err = installer(&server, tmp.path(), fake)
            .run(&|_, _, _| {})
            .await
            .unwrap_err();
        assert!(err.0.contains("checksum"), "{err}");
        assert!(calls.lock().unwrap().is_empty());
        assert!(!tmp.path().join("Applications").exists());
    }

    #[tokio::test]
    async fn no_published_checksum_means_no_install() {
        let server = MockServer::start().await;
        release(&server, None).await;
        let tmp = tempfile::tempdir().unwrap();
        let err = installer(&server, tmp.path(), Fake::default())
            .run(&|_, _, _| {})
            .await
            .unwrap_err();
        assert!(err.0.contains("no checksum"), "{err}");
    }

    #[tokio::test]
    async fn a_failed_signature_check_installs_nothing() {
        let server = MockServer::start().await;
        release(&server, Some(format!("sha256:{}", sha(BODY)))).await;
        let tmp = tempfile::tempdir().unwrap();
        let fake = Fake {
            reject: true,
            ..Fake::default()
        };
        let calls = fake.calls.clone();
        let err = installer(&server, tmp.path(), fake)
            .run(&|_, _, _| {})
            .await
            .unwrap_err();
        assert!(err.0.contains("signature"), "{err}");
        assert_eq!(*calls.lock().unwrap(), ["extract", "verify"]);
        assert!(!tmp.path().join("Applications").exists());
    }

    #[tokio::test]
    async fn refuses_a_host_that_is_not_github() {
        let server = MockServer::start().await;
        release(&server, Some(format!("sha256:{}", sha(BODY)))).await;
        let tmp = tempfile::tempdir().unwrap();
        // Allowed list excludes this server.
        let i = Installer::new(
            &format!("{}/release", server.uri()),
            &["github.com"],
            tmp.path().join("work"),
            tmp.path().join("Ollama.app"),
            Box::new(Fake::default()),
        );
        let err = i.run(&|_, _, _| {}).await.unwrap_err();
        assert!(err.0.contains("only Ollama's GitHub"), "{err}");
    }

    #[test]
    fn source_rules() {
        let hosts = vec!["github.com".to_owned(), "githubusercontent.com".to_owned()];
        assert!(check_source("https://github.com/ollama/ollama/x.zip", &hosts).is_ok());
        assert!(check_source("https://objects.githubusercontent.com/x", &hosts).is_ok());
        assert!(check_source("http://github.com/x", &hosts).is_err());
        assert!(check_source("https://evilgithub.com/x", &hosts).is_err());
        assert!(check_source("https://github.com.evil.io/x", &hosts).is_err());
        assert!(check_source("https://user@evil.io/github.com", &hosts).is_err());
    }

    #[test]
    fn the_plan_says_what_it_does() {
        let tmp = tempfile::tempdir().unwrap();
        let i = Installer::new(
            "http://127.0.0.1/x",
            &[],
            tmp.path().into(),
            tmp.path().join("Ollama.app"),
            Box::new(Fake::default()),
        );
        let p = i.plan();
        assert!(p.summary.contains("checks its checksum"));
        assert!(p.destination.ends_with("Ollama.app"));
    }
}
