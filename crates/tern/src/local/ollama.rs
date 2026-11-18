//! Ollama as a local engine, over its HTTP API (docs/local-models.md).
//!
//! Rules:
//! - Whatever answers on the address is used, started by tern or not.
//!   Probe first, never start a second server.
//! - A server tern starts ([`Ollama::start`]) is tern's to manage: context,
//!   keep-alive and memory settings, loopback only, stopped with the daemon.
//! - A server the user runs is used as is. tern warns about its settings and
//!   never changes them.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_providers::ollama as api;

use super::{ContextSource, Detection, Engine, Error, LocalModel, OLLAMA_URL, Status};

const GIB: u64 = 1 << 30;
const NAME: &str = "Ollama";

/// Everything that depends on this computer, so tests can replace it.
pub struct Config {
    /// The server's address. Always loopback.
    pub url: String,
    /// Where the app may be (`Ollama.app`), then where the command may be.
    pub app_dirs: Vec<PathBuf>,
    pub bin_dirs: Vec<PathBuf>,
    pub models_dir: PathBuf,
    pub ram_bytes: Option<u64>,
    pub disk_free: fn(&Path) -> Option<u64>,
    pub spawner: Arc<dyn Spawner>,
    /// How long to wait for a server tern started to answer.
    pub ready_within: Duration,
}

impl Config {
    pub fn system() -> Self {
        let home = tern_core::home::user_home().unwrap_or_else(|| PathBuf::from("."));
        let from_env = |k: &str| -> Option<Vec<PathBuf>> {
            std::env::var_os(k).map(|v| std::env::split_paths(&v).collect())
        };
        let mut path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default();
        path_dirs.extend(["/opt/homebrew/bin".into(), "/usr/local/bin".into()]);
        Self {
            url: std::env::var("TERN_LOCAL_URL").unwrap_or_else(|_| OLLAMA_URL.into()),
            app_dirs: from_env("TERN_LOCAL_APPS")
                .unwrap_or_else(|| vec!["/Applications".into(), home.join("Applications")]),
            bin_dirs: from_env("TERN_LOCAL_APPS").map_or(path_dirs, |_| Vec::new()),
            models_dir: std::env::var_os("OLLAMA_MODELS")
                .map_or_else(|| home.join(".ollama/models"), PathBuf::from),
            ram_bytes: tern_core::limits::total_memory_bytes(),
            disk_free,
            spawner: Arc::new(RealSpawner),
            ready_within: Duration::from_secs(30),
        }
    }
}

/// Free bytes on the volume holding `path` (or its nearest parent that exists).
fn disk_free(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let mut p = path;
        while !p.exists() {
            p = p.parent()?;
        }
        let s = nix::sys::statvfs::statvfs(p).ok()?;
        Some(u64::from(s.blocks_available()).saturating_mul(s.fragment_size()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// A server process tern started.
#[async_trait]
pub trait Child: Send + Sync {
    /// Stop it and wait.
    async fn kill(&mut self);
    /// Still running?
    fn alive(&mut self) -> bool;
}

/// Starts `ollama serve`. Tests use a fake: they never run the real one.
pub trait Spawner: Send + Sync {
    fn spawn(&self, bin: &Path, env: &[(String, String)]) -> std::io::Result<Box<dyn Child>>;
}

struct RealSpawner;

struct RealChild(tokio::process::Child);

#[async_trait]
impl Child for RealChild {
    async fn kill(&mut self) {
        let _ = self.0.kill().await;
    }
    fn alive(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Spawner for RealSpawner {
    fn spawn(&self, bin: &Path, env: &[(String, String)]) -> std::io::Result<Box<dyn Child>> {
        let child = tokio::process::Command::new(bin)
            .arg("serve")
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // The daemon going away takes the server with it.
            .kill_on_drop(true)
            .spawn()?;
        Ok(Box::new(RealChild(child)))
    }
}

/// The context tern starts a server with: as much as the memory allows
/// (agents need 64k, Ollama advises).
pub fn managed_context(ram: Option<u64>) -> u64 {
    match ram {
        Some(r) if r < 10 * GIB => 16_384,
        Some(r) if r < 16 * GIB => 32_768,
        _ => 65_536,
    }
}

/// The environment for a server tern starts: loopback only, a long context
/// and keep-alive, and on a small Mac a quantized cache to save memory.
pub fn managed_env(url: &str, ram: Option<u64>) -> Vec<(String, String)> {
    let port = url
        .rsplit_once(':')
        .and_then(|(_, p)| p.trim_end_matches('/').parse::<u16>().ok())
        .unwrap_or(11434);
    let mut env = vec![
        ("OLLAMA_HOST".into(), format!("127.0.0.1:{port}")),
        (
            "OLLAMA_CONTEXT_LENGTH".into(),
            managed_context(ram).to_string(),
        ),
        ("OLLAMA_KEEP_ALIVE".into(), api::KEEP_ALIVE.into()),
    ];
    if ram.is_some_and(|r| r < 32 * GIB) {
        env.push(("OLLAMA_FLASH_ATTENTION".into(), "1".into()));
        env.push(("OLLAMA_KV_CACHE_TYPE".into(), "q8_0".into()));
    }
    env
}

pub struct Ollama {
    cfg: Config,
    quick: reqwest::Client,
    child: tokio::sync::Mutex<Option<Box<dyn Child>>>,
    /// The context a server tern started was given.
    managed_ctx: Mutex<Option<u64>>,
}

impl Ollama {
    pub fn new(cfg: Config) -> Self {
        let quick = tern_core::net::http_client()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            quick,
            cfg,
            child: tokio::sync::Mutex::new(None),
            managed_ctx: Mutex::new(None),
        }
    }

    fn app_bundles(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.cfg.app_dirs.iter().map(|d| d.join("Ollama.app"))
    }

    /// The command to run `serve` with: the app's own, then `ollama` on the
    /// usual paths.
    fn binary(&self) -> Option<PathBuf> {
        self.app_bundles()
            .map(|a| a.join("Contents/Resources/ollama"))
            .chain(self.cfg.bin_dirs.iter().map(|d| d.join("ollama")))
            .find(|p| p.is_file())
    }

    fn installed(&self) -> bool {
        self.app_bundles().any(|a| a.is_dir()) || self.binary().is_some()
    }

    async fn json(&self, path: &str) -> Option<Value> {
        let r = self
            .quick
            .get(format!("{}{path}", self.cfg.url))
            .send()
            .await
            .ok()?;
        r.status().is_success().then_some(())?;
        r.json().await.ok()
    }

    fn managed_value(&self) -> Option<u64> {
        *self.managed_ctx.lock().unwrap_or_else(|p| p.into_inner())
    }

    async fn is_managed(&self) -> bool {
        match self.child.lock().await.as_mut() {
            Some(c) => c.alive(),
            None => false,
        }
    }
}

#[async_trait]
impl Engine for Ollama {
    fn id(&self) -> &'static str {
        "ollama"
    }

    async fn detect(&self) -> Detection {
        let version = self
            .json("/api/version")
            .await
            .and_then(|v| v["version"].as_str().map(str::to_owned));
        let too_old = version
            .as_deref()
            .and_then(api::parse_version)
            .is_some_and(api::version_too_old);
        Detection {
            // A running server is an install, wherever it lives.
            installed: version.is_some() || self.installed(),
            running: version.is_some(),
            version,
            too_old,
        }
    }

    async fn status(&self) -> Status {
        let detection = self.detect().await;
        let managed = detection.running && self.is_managed().await;
        let mut warnings = Vec::new();
        if let Some(v) = detection.version.as_deref().and_then(api::parse_version)
            && api::version_too_old(v)
        {
            warnings.push(api::too_old_warning(v));
        }
        let loaded = if detection.running {
            self.json("/api/ps")
                .await
                .map(|v| api::parse_ps(&v))
                .and_then(|m| m.values().copied().min())
        } else {
            None
        };
        let (context, context_source) = match (loaded, self.managed_value().filter(|_| managed)) {
            (Some(c), _) => (Some(c), Some(ContextSource::Loaded)),
            (None, Some(c)) => (Some(c), Some(ContextSource::Managed)),
            (None, None) => (
                self.cfg.ram_bytes.map(api::default_context),
                self.cfg.ram_bytes.map(|_| ContextSource::Estimate),
            ),
        };
        if detection.running
            && let Some(c) = context.filter(|c| *c < api::MIN_CONTEXT)
        {
            warnings.push(if managed {
                format!(
                    "This Mac's memory leaves {c} tokens of context for local models. Long tasks may lose their early instructions."
                )
            } else {
                api::small_context_warning(c)
            });
        }
        Status {
            engine: "",
            detection,
            managed,
            url: self.cfg.url.clone(),
            models_dir: Some(self.cfg.models_dir.display().to_string()),
            disk_free: (self.cfg.disk_free)(&self.cfg.models_dir),
            ram_bytes: self.cfg.ram_bytes,
            context,
            context_source,
            warnings,
            install_page: super::INSTALL_PAGE,
            // Filled in by `Local`.
            install: super::install::Plan {
                source: String::new(),
                destination: String::new(),
                summary: String::new(),
                available: false,
            },
            installing: super::install::State::Idle,
        }
    }

    async fn list(&self) -> Result<Vec<LocalModel>, Error> {
        let tags = self
            .json("/api/tags")
            .await
            .ok_or_else(|| Error::Unavailable("Ollama isn't running".into()))?;
        let ps = self
            .json("/api/ps")
            .await
            .map(|v| api::parse_ps(&v))
            .unwrap_or_default();
        let models = tags["models"].as_array().cloned().unwrap_or_default();
        let shows = futures::future::join_all(models.iter().map(|m| {
            let name = m["name"].as_str().unwrap_or_default().to_owned();
            let client = self.quick.clone();
            let url = format!("{}/api/show", self.cfg.url);
            async move {
                let r = client
                    .post(url)
                    .json(&json!({ "model": name }))
                    .send()
                    .await
                    .ok()?;
                r.status().is_success().then_some(())?;
                r.json::<Value>().await.ok()
            }
        }))
        .await;
        Ok(models
            .iter()
            .zip(shows)
            .map(|(m, show)| {
                let shown = show.as_ref().map(api::parse_show).unwrap_or_default();
                let name = m["name"].as_str().unwrap_or_default().to_owned();
                let d = &m["details"];
                let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
                LocalModel {
                    loaded: ps.keys().any(|p| {
                        p.strip_suffix(":latest").unwrap_or(p)
                            == name.strip_suffix(":latest").unwrap_or(&name)
                    }),
                    name,
                    size: m["size"].as_u64().unwrap_or(0),
                    digest: m["digest"].as_str().unwrap_or_default().to_owned(),
                    family: text(&d["family"]),
                    parameter_size: text(&d["parameter_size"]),
                    quantization: text(&d["quantization_level"]),
                    tools: shown.tools,
                    thinking: shown.thinking,
                    max_context: shown.max_context,
                }
            })
            .collect())
    }

    async fn start(&self) -> Result<(), Error> {
        // Always reuse what answers.
        if self.detect().await.running {
            return Ok(());
        }
        let bin = self.binary().ok_or(Error::NotInstalled(NAME))?;
        let ctx = managed_context(self.cfg.ram_bytes);
        let env = managed_env(&self.cfg.url, self.cfg.ram_bytes);
        let mut guard = self.child.lock().await;
        if let Some(mut old) = guard.take() {
            old.kill().await;
        }
        let child = self
            .cfg
            .spawner
            .spawn(&bin, &env)
            .map_err(|e| Error::Failed(format!("couldn't start Ollama: {e}")))?;
        *guard = Some(child);
        let deadline = tokio::time::Instant::now() + self.cfg.ready_within;
        loop {
            if self.detect().await.running {
                *self.managed_ctx.lock().unwrap_or_else(|p| p.into_inner()) = Some(ctx);
                return Ok(());
            }
            let dead = guard.as_mut().is_some_and(|c| !c.alive());
            if dead || tokio::time::Instant::now() >= deadline {
                if let Some(mut c) = guard.take() {
                    c.kill().await;
                }
                return Err(Error::Failed(
                    "Ollama didn't start. Another program may be using its port (11434).".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn stop(&self) -> Result<(), Error> {
        let mut guard = self.child.lock().await;
        match guard.take() {
            Some(mut c) => {
                c.kill().await;
                *self.managed_ctx.lock().unwrap_or_else(|p| p.into_inner()) = None;
                Ok(())
            }
            None => Err(Error::Unavailable(
                "tern didn't start this Ollama, so it won't stop it. Quit it from the menu bar."
                    .into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    type Spawned = Arc<Mutex<Vec<(PathBuf, Vec<(String, String)>)>>>;

    #[derive(Default)]
    struct FakeSpawner {
        spawned: Spawned,
        killed: Arc<AtomicUsize>,
        /// Mounts the server's version endpoint when "started".
        on_spawn: Mutex<Option<Box<dyn Fn() + Send>>>,
    }

    struct FakeChild(Arc<AtomicUsize>);

    #[async_trait]
    impl Child for FakeChild {
        async fn kill(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn alive(&mut self) -> bool {
            true
        }
    }

    impl Spawner for FakeSpawner {
        fn spawn(&self, bin: &Path, env: &[(String, String)]) -> std::io::Result<Box<dyn Child>> {
            self.spawned
                .lock()
                .unwrap()
                .push((bin.to_owned(), env.to_vec()));
            if let Some(f) = self.on_spawn.lock().unwrap().as_ref() {
                f();
            }
            Ok(Box::new(FakeChild(self.killed.clone())))
        }
    }

    fn cfg(url: &str, dir: &Path, spawner: Arc<dyn Spawner>) -> Config {
        Config {
            url: url.into(),
            app_dirs: vec![dir.join("Applications")],
            bin_dirs: vec![dir.join("bin")],
            models_dir: dir.join("models"),
            ram_bytes: Some(16 * GIB),
            disk_free: |_| Some(50 * GIB),
            spawner,
            ready_within: Duration::from_millis(600),
        }
    }

    async fn version(server: &MockServer, v: &str) {
        Mock::given(method("GET"))
            .and(path("/api/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": v })))
            .mount(server)
            .await;
    }

    /// A dead address: nothing listens there.
    const DEAD: &str = "http://127.0.0.1:1";

    fn fake_app(dir: &Path) {
        let bin = dir.join("Applications/Ollama.app/Contents/Resources");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("ollama"), "#!/bin/sh\n").unwrap();
    }

    #[test]
    fn managed_settings_fit_the_memory_and_stay_on_loopback() {
        assert_eq!(managed_context(Some(8 * GIB)), 16_384);
        assert_eq!(managed_context(Some(12 * GIB)), 32_768);
        assert_eq!(managed_context(Some(16 * GIB)), 65_536);
        assert_eq!(managed_context(None), 65_536);
        let env = managed_env("http://127.0.0.1:11434", Some(16 * GIB));
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("OLLAMA_HOST"), Some("127.0.0.1:11434"));
        assert_eq!(get("OLLAMA_CONTEXT_LENGTH"), Some("65536"));
        assert_eq!(get("OLLAMA_KEEP_ALIVE"), Some("30m"));
        assert_eq!(get("OLLAMA_KV_CACHE_TYPE"), Some("q8_0"));
        let big = managed_env("http://127.0.0.1:11434", Some(64 * GIB));
        assert!(big.iter().all(|(k, _)| k != "OLLAMA_KV_CACHE_TYPE"));
    }

    #[tokio::test]
    async fn detects_nothing_an_app_and_a_running_server() {
        let tmp = tempfile::tempdir().unwrap();
        let e = Ollama::new(cfg(DEAD, tmp.path(), Arc::new(FakeSpawner::default())));
        assert_eq!(e.detect().await, Detection::default());
        fake_app(tmp.path());
        let d = e.detect().await;
        assert!(d.installed && !d.running);

        let server = MockServer::start().await;
        version(&server, "0.9.0").await;
        let e = Ollama::new(cfg(
            &server.uri(),
            tmp.path(),
            Arc::new(FakeSpawner::default()),
        ));
        let d = e.detect().await;
        assert!(d.running && d.installed && !d.too_old);
        assert_eq!(d.version.as_deref(), Some("0.9.0"));
    }

    #[tokio::test]
    async fn status_warns_about_an_old_server_and_a_small_context() {
        let tmp = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        version(&server, "0.5.0").await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "models": [{ "name": "m", "context_length": 4096 }] })),
            )
            .mount(&server)
            .await;
        let e = Ollama::new(cfg(
            &server.uri(),
            tmp.path(),
            Arc::new(FakeSpawner::default()),
        ));
        let s = e.status().await;
        assert!(s.detection.too_old && !s.managed);
        assert_eq!(s.context, Some(4_096));
        assert_eq!(s.context_source, Some(ContextSource::Loaded));
        assert_eq!(s.disk_free, Some(50 * GIB));
        assert!(s.warnings.iter().any(|w| w.contains("too old")));
        assert!(s.warnings.iter().any(|w| w.contains("4,096")));
    }

    #[tokio::test]
    async fn start_reuses_a_running_server_and_never_spawns() {
        let tmp = tempfile::tempdir().unwrap();
        fake_app(tmp.path());
        let server = MockServer::start().await;
        version(&server, "0.9.0").await;
        let sp = Arc::new(FakeSpawner::default());
        let spawned = sp.spawned.clone();
        let e = Ollama::new(cfg(&server.uri(), tmp.path(), sp));
        e.start().await.unwrap();
        assert!(spawned.lock().unwrap().is_empty());
        // Not ours: stop refuses.
        assert!(matches!(e.stop().await, Err(Error::Unavailable(_))));
        assert!(!e.status().await.managed);
    }

    #[tokio::test]
    async fn start_without_ollama_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let e = Ollama::new(cfg(DEAD, tmp.path(), Arc::new(FakeSpawner::default())));
        assert!(matches!(e.start().await, Err(Error::NotInstalled(_))));
    }

    /// Answers `/api/version` on `port`, from the moment it is called: a
    /// stand-in for the server a "started" Ollama would be.
    fn serve_version(port: u16) {
        use std::io::{Read, Write};
        std::thread::spawn(move || {
            let Ok(l) = std::net::TcpListener::bind(("127.0.0.1", port)) else {
                return;
            };
            for conn in l.incoming().flatten() {
                let mut conn = conn;
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                let body = r#"{"version":"0.9.0"}"#;
                let _ = write!(
                    conn,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
    }

    #[tokio::test]
    async fn start_runs_the_app_binary_with_managed_settings_then_stop_kills_it() {
        let tmp = tempfile::tempdir().unwrap();
        fake_app(tmp.path());
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let sp = Arc::new(FakeSpawner::default());
        let (spawned, killed) = (sp.spawned.clone(), sp.killed.clone());
        *sp.on_spawn.lock().unwrap() = Some(Box::new(move || serve_version(port)));
        let e = Ollama::new(cfg(&format!("http://127.0.0.1:{port}"), tmp.path(), sp));
        e.start().await.unwrap();
        {
            let calls = spawned.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert!(calls[0].0.ends_with("Ollama.app/Contents/Resources/ollama"));
            let env = &calls[0].1;
            assert!(env.contains(&("OLLAMA_HOST".into(), format!("127.0.0.1:{port}"))));
            assert!(env.contains(&("OLLAMA_CONTEXT_LENGTH".into(), "65536".into())));
        }
        let s = e.status().await;
        assert!(s.managed);
        assert_eq!(s.context, Some(65_536));
        assert_eq!(s.context_source, Some(ContextSource::Managed));
        // Asking again reuses it.
        e.start().await.unwrap();
        assert_eq!(spawned.lock().unwrap().len(), 1);
        e.stop().await.unwrap();
        assert_eq!(killed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn start_spawns_when_nothing_answers_and_gives_up_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        fake_app(tmp.path());
        let sp = Arc::new(FakeSpawner::default());
        let (spawned, killed) = (sp.spawned.clone(), sp.killed.clone());
        let e = Ollama::new(cfg(DEAD, tmp.path(), sp));
        // Nothing ever answers: tern spawns once, waits, kills it, and says why.
        let err = e.start().await.unwrap_err();
        assert!(err.to_string().contains("didn't start"), "{err}");
        let calls = spawned.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].0.ends_with("Ollama.app/Contents/Resources/ollama"));
        assert!(
            calls[0]
                .1
                .iter()
                .any(|(k, v)| k == "OLLAMA_HOST" && v == "127.0.0.1:1")
        );
        assert_eq!(killed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn lists_models_with_tools_and_loaded_state() {
        let tmp = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "models": [
                { "name": "qwen3:8b", "size": 5_000_000_000u64, "digest": "abc",
                  "details": { "family": "qwen3", "parameter_size": "8B", "quantization_level": "Q4_K_M" } },
                { "name": "llama2:latest", "size": 3_800_000_000u64, "digest": "def", "details": {} },
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "models": [{ "name": "qwen3:8b", "context_length": 65536 }] }),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/show"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "capabilities": ["completion", "tools"],
                "model_info": { "qwen3.context_length": 40960 },
            })))
            .mount(&server)
            .await;
        let e = Ollama::new(cfg(
            &server.uri(),
            tmp.path(),
            Arc::new(FakeSpawner::default()),
        ));
        let m = e.list().await.unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "qwen3:8b");
        assert_eq!(m[0].parameter_size.as_deref(), Some("8B"));
        assert!(m[0].loaded && !m[1].loaded);
        assert_eq!(m[0].tools, Some(true));
        assert_eq!(m[0].max_context, Some(40_960));
    }
}
