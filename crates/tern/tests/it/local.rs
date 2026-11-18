//! Local models over the daemon API (docs/local-models.md): status, the
//! installed list, start, stop and the install consent. The engine is a
//! fake Ollama on a local port, and the daemon is pointed at an empty
//! folder for apps, so nothing here sees this computer's Ollama, starts a
//! process or downloads anything.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

struct Daemon {
    child: Child,
    _tmp: tempfile::TempDir,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A daemon with extra environment variables (it starts from a clear one).
fn daemon_with(env: &[(&str, &str)]) -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let tern_home = home.join(".tern");
    std::fs::create_dir_all(&tern_home).unwrap();
    let socket = tern_home.join("tern.sock");
    let child = Command::new(tern_bin())
        .arg("daemon")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("TERN_HOME", &tern_home)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < end, "daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon {
        child,
        _tmp: tmp,
        socket,
    }
}

struct Api(reqwest::Client);

impl Api {
    fn new(d: &Daemon) -> Self {
        Self(
            reqwest::Client::builder()
                .unix_socket(d.socket.clone())
                .build()
                .unwrap(),
        )
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (u16, String) {
        let mut req = self.0.request(method, format!("http://tern{path}"));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.text().await.unwrap())
    }

    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (status, text) = self.call(method.clone(), path, body).await;
        assert_eq!(status, 200, "{method} {path}: {text}");
        serde_json::from_str(&text).unwrap()
    }
}

/// Where nothing listens.
const DEAD: &str = "http://127.0.0.1:1";

fn local_daemon(url: &str) -> Daemon {
    let apps = tempfile::tempdir().unwrap();
    let d = daemon_with(&[
        ("TERN_LOCAL_DISK_FREE", "107374182400"),
        ("TERN_LOCAL_URL", url),
        ("TERN_LOCAL_APPS", apps.path().to_str().unwrap()),
    ]);
    // Keep the folder alive as long as the daemon.
    std::mem::forget(apps);
    d
}

async fn fake_ollama() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": "0.9.0" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/ps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "models": [] })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "models": [
            { "name": "qwen3:8b", "size": 5, "digest": "d", "details": { "parameter_size": "8B" } }
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/pull"))
        .respond_with(ResponseTemplate::new(200).set_body_string(concat!(
            "{\"status\":\"pulling manifest\"}\n",
            "{\"status\":\"pulling aaa\",\"digest\":\"aaa\",\"total\":100,\"completed\":100}\n",
            "{\"status\":\"success\"}\n",
        )))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/delete"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "capabilities": ["tools"], "model_info": { "qwen3.context_length": 40960 }
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_ollama_is_reused_as_is() {
    let fake = fake_ollama().await;
    let d = local_daemon(&fake.uri());
    let api = Api::new(&d);
    let s = api.ok(Method::GET, "/v1/local/status", None).await;
    assert_eq!(s["engine"], "ollama");
    assert_eq!(s["running"], true);
    assert_eq!(s["installed"], true);
    assert_eq!(s["managed"], false);
    assert_eq!(s["version"], "0.9.0");
    assert!(
        s["install"]["summary"]
            .as_str()
            .unwrap()
            .contains("checksum")
    );
    assert_eq!(s["installing"]["state"], "idle");
    let m = api.ok(Method::GET, "/v1/local/models", None).await;
    assert_eq!(m[0]["name"], "qwen3:8b");
    assert_eq!(m[0]["tools"], true);
    assert_eq!(m[0]["max_context"], 40960);
    // Start reuses it; stop leaves a server tern didn't start alone.
    api.ok(Method::POST, "/v1/local/start", None).await;
    let (code, text) = api.call(Method::POST, "/v1/local/stop", None).await;
    assert_eq!(code, 409, "{text}");
    assert!(text.contains("didn't start"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_ollama_it_says_so_and_never_downloads_without_consent() {
    let d = local_daemon(DEAD);
    let api = Api::new(&d);
    let s = api.ok(Method::GET, "/v1/local/status", None).await;
    assert_eq!(s["installed"], false);
    assert_eq!(s["running"], false);
    assert_eq!(s["install_page"], "https://ollama.com/download");
    let (code, text) = api.call(Method::POST, "/v1/local/start", None).await;
    assert_eq!(code, 409, "{text}");
    assert!(text.contains("isn't installed"), "{text}");
    let (code, text) = api
        .call(Method::POST, "/v1/local/install", Some(json!({})))
        .await;
    assert_eq!(code, 400, "{text}");
    assert!(text.contains("after you agree"), "{text}");
    let s = api.ok(Method::GET, "/v1/local/status", None).await;
    assert_eq!(s["installing"]["state"], "idle");
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_runs_in_the_daemon_and_delete_removes() {
    let fake = fake_ollama().await;
    let d = local_daemon(&fake.uri());
    let api = Api::new(&d);
    let job = api
        .ok(
            Method::POST,
            "/v1/local/pull",
            Some(json!({ "model": "qwen3:8b" })),
        )
        .await;
    assert_eq!(job["model"], "qwen3:8b");
    let id = job["id"].as_str().unwrap().to_owned();
    let end = Instant::now() + Duration::from_secs(10);
    let done = loop {
        let list = api.ok(Method::GET, "/v1/local/pulls", None).await;
        let p = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id.as_str())
            .unwrap()
            .clone();
        if p["phase"] == "done" || p["phase"] == "failed" {
            break p;
        }
        assert!(Instant::now() < end, "download did not finish: {p}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(done["phase"], "done", "{done}");
    assert_eq!(done["total"], 100);
    // A bad name never reaches Ollama; a model with a slash in it deletes.
    let (code, _) = api
        .call(
            Method::POST,
            "/v1/local/pull",
            Some(json!({ "model": "a b" })),
        )
        .await;
    assert_eq!(code, 400);
    api.ok(Method::DELETE, "/v1/local/models/hf.co/user/model:Q4", None)
        .await;
    let (code, _) = api
        .call(Method::POST, "/v1/local/pulls/nope/cancel", None)
        .await;
    assert_eq!(code, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_without_ollama_running_says_to_start_it() {
    let d = local_daemon(DEAD);
    let api = Api::new(&d);
    let (code, text) = api
        .call(
            Method::POST,
            "/v1/local/pull",
            Some(json!({ "model": "qwen3:8b" })),
        )
        .await;
    assert_eq!(code, 409, "{text}");
    assert!(text.contains("Start it"), "{text}");
}

/// `tern local …` with no daemon: straight to the (fake) Ollama.
fn cli(url: &str, args: &[&str]) -> (bool, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let apps = tmp.path().join("apps");
    std::fs::create_dir_all(&apps).unwrap();
    let out = Command::new(tern_bin())
        .args(["local"])
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("TERN_HOME", home.join(".tern"))
        .env("TERN_LOCAL_URL", url)
        .env("TERN_LOCAL_APPS", &apps)
        .env("TERN_LOCAL_DISK_FREE", "107374182400")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_cli_reads_status_and_lists_without_a_daemon() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let fake = rt.block_on(fake_ollama());
    let (ok, out, err) = cli(&fake.uri(), &["status", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["running"], true);
    assert_eq!(v["managed"], false);
    let (ok, out, err) = cli(&fake.uri(), &["list"]);
    assert!(ok, "{err}");
    assert!(out.contains("qwen3:8b") && out.contains("tools"), "{out}");
    // Start and stop need the daemon.
    let (ok, _, err) = cli(&fake.uri(), &["start"]);
    assert!(!ok && err.contains("tern start"), "{err}");
}

#[test]
fn the_cli_pulls_with_progress_and_says_when_ollama_is_off() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let fake = rt.block_on(fake_ollama());
    let (ok, out, err) = cli(&fake.uri(), &["pull", "qwen3:8b"]);
    assert!(ok, "{err}");
    assert!(out.contains("qwen3:8b is ready"), "{out}");
    let (ok, _, err) = cli(DEAD, &["pull", "qwen3:8b"]);
    assert!(!ok && err.contains("Start it"), "{err}");
    let (ok, out, _) = cli(&fake.uri(), &["rm", "qwen3:8b"]);
    assert!(ok && out.contains("removed"), "{out}");
}
