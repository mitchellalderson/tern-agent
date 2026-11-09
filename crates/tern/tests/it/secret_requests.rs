//! Secret requests end to end (docs/secret-requests-spec.md): the agent
//! asks for a secret, an API client answers, and the value is stored
//! without reaching the journal, the event stream or the logs.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

const VALUE: &str = "wk-live-7f3a9c1e5b2d8046";

const WEATHER: &str = r#"return {
  name = "weather",
  description = "Forecasts",
  capabilities = { http = { "api.weather.example" }, secrets = { "WEATHER_API_KEY" } },
  tools = {
    forecast = {
      description = "Forecast",
      params = { type = "object", properties = {} },
      run = function(ctx, args) return ctx.secret("WEATHER_API_KEY") ~= nil end,
    },
  },
}"#;

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    tern_home: PathBuf,
    script: PathBuf,
}

impl Env {
    fn new(config: &str, script: &Value) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(&tern_home).unwrap();
        std::fs::write(tern_home.join("config.toml"), config).unwrap();
        let script_path = tmp.path().join("script.json");
        std::fs::write(&script_path, script.to_string()).unwrap();
        Self {
            script: script_path,
            home,
            tern_home,
            _tmp: tmp,
        }
    }

    fn socket(&self) -> PathBuf {
        self.tern_home.join("tern.sock")
    }

    fn daemon(&self) -> Killed {
        let child = Command::new(tern_bin())
            .arg("daemon")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env("TERN_PROVIDER", format!("mock:{}", self.script.display()))
            .env("RUST_LOG", "debug")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while !self.socket().exists() {
            assert!(Instant::now() < end, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Killed(child)
    }
}

fn api(socket: &Path) -> reqwest::Client {
    reqwest::Client::builder()
        .unix_socket(socket)
        .build()
        .unwrap()
}

/// Read events until one matches `want`, answering approvals on the way.
/// `seen` keeps everything read, for the leak check; `rest` holds a line
/// that hasn't fully arrived yet.
async fn wait_for(
    http: &reqwest::Client,
    events: &mut reqwest::Response,
    seen: &mut String,
    rest: &mut String,
    want: impl Fn(&Value) -> bool,
) -> Value {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        while let Some(nl) = rest.find('\n') {
            let line: String = rest.drain(..=nl).collect();
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let ev: Value = serde_json::from_str(data.trim()).unwrap();
            if let Some(id) = ev["approval_requested"]["id"].as_str() {
                http.post(format!("http://tern/v1/approvals/{id}"))
                    .json(&json!({ "decision": "once" }))
                    .send()
                    .await
                    .unwrap();
            }
            if want(&ev) {
                return ev;
            }
        }
        assert!(Instant::now() < end, "no matching event in {seen}");
        let chunk = tokio::time::timeout(Duration::from_secs(15), events.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = String::from_utf8_lossy(&chunk);
        seen.push_str(&text);
        rest.push_str(&text);
    }
}

async fn start(http: &reqwest::Client, text: &str) -> String {
    let started: Value = http
        .post("http://tern/v1/runs")
        .json(&json!({ "text": text }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    started["run_id"].as_str().unwrap().to_owned()
}

async fn wait_run(http: &reqwest::Client, run_id: &str) -> Value {
    http.get(format!("http://tern/v1/runs/{run_id}/wait"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Files under `dir` (other than `secrets.toml`) that contain `needle`.
fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            hits.extend(files_containing(&p, needle));
        } else if p.file_name().is_some_and(|n| n != "secrets.toml")
            && let Ok(bytes) = std::fs::read(&p)
            && bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
        {
            hits.push(p);
        }
    }
    hits
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_asks_api_client_answers_value_stays_private() {
    let env = Env::new(
        "",
        &json!({ "turns": [
            { "tool_calls": [{ "name": "connector_write", "input": { "name": "weather", "source": WEATHER } }] },
            { "tool_calls": [{ "name": "secret_request", "input": {
                "names": ["WEATHER_API_KEY"],
                "reason": "The weather connector sends it to api.weather.example."
            } }] },
            { "text": "Saved, thanks." }
        ]}),
    );
    let _daemon = env.daemon();
    let http = api(&env.socket());
    let health: Value = http
        .get("http://tern/v1/health")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        health["features"]
            .as_array()
            .unwrap()
            .contains(&json!("secret_requests"))
    );

    let mut events = http.get("http://tern/v1/events").send().await.unwrap();
    let (mut buf, mut rest) = (String::new(), String::new());
    let run_id = start(&http, "set up a weather connector").await;
    let ev = wait_for(&http, &mut events, &mut buf, &mut rest, |e| {
        e.get("secret_requested").is_some()
    })
    .await;
    let req = &ev["secret_requested"];
    assert_eq!(req["run_id"], run_id.as_str());
    assert_eq!(
        req["reason"],
        "The weather connector sends it to api.weather.example."
    );
    assert_eq!(req["secrets"][0]["name"], "WEATHER_API_KEY");
    assert_eq!(req["secrets"][0]["in_file"], false);
    assert_eq!(
        req["secrets"][0]["used_by"],
        json!([
            "connector weather (draft) · reaches api.weather.example · reads the value itself (legacy ctx.secret)"
        ])
    );
    let id = req["id"].as_str().unwrap().to_owned();
    let call_id = req["call_id"].as_str().unwrap().to_owned();

    // Late clients find it.
    let pending: Value = http
        .get("http://tern/v1/secret-requests")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending[0]["id"], id.as_str());

    // Only names the request asked for can be written.
    let bad = http
        .post(format!("http://tern/v1/secret-requests/{id}"))
        .json(&json!({ "values": { "ANTHROPIC_API_KEY": "sk-ant-nope" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);

    let ok = http
        .post(format!("http://tern/v1/secret-requests/{id}"))
        .json(&json!({ "values": { "WEATHER_API_KEY": format!("  {VALUE}\n") } }))
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success());
    let again = http
        .post(format!("http://tern/v1/secret-requests/{id}"))
        .json(&json!({ "values": {} }))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 404);

    // The tool call finishes with only the outcome.
    let done = wait_for(&http, &mut events, &mut buf, &mut rest, |e| {
        e["type"] == "tool_finished"
            && e["id"] == call_id.as_str()
            && e["run_id"] == run_id.as_str()
    })
    .await;
    assert_eq!(done["output"], r#"{"WEATHER_API_KEY":"set"}"#);
    let outcome = wait_run(&http, &run_id).await;
    assert_eq!(outcome["status"], "completed", "{outcome}");

    let secrets = std::fs::read_to_string(env.tern_home.join("secrets.toml")).unwrap();
    assert!(
        secrets.contains(&format!("WEATHER_API_KEY = \"{VALUE}\"")),
        "{secrets}"
    );
    let pending: Value = http
        .get("http://tern/v1/secret-requests")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending, json!([]));

    // Nowhere else: not the database, the logs, or the event stream.
    assert!(!buf.contains(VALUE));
    let hits = files_containing(&env.tern_home, VALUE);
    assert!(hits.is_empty(), "value found in {hits:?}");
    // The logs were written (so the check above covered them) and name
    // the secret, not its value.
    let logged = files_containing(
        &env.tern_home.join("logs"),
        "secret set from a secret_request",
    );
    assert!(!logged.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn unanswered_requests_time_out() {
    let env = Env::new(
        "secret_request_timeout_secs = 1\n",
        &json!({ "turns": [
            { "tool_calls": [{ "name": "secret_request", "input": {
                "names": ["SOME_TOKEN"], "reason": "testing"
            } }] },
            { "text": "Gave up." }
        ]}),
    );
    let _daemon = env.daemon();
    let http = api(&env.socket());
    let run_id = start(&http, "go").await;
    let outcome = wait_run(&http, &run_id).await;
    assert_eq!(outcome["status"], "completed", "{outcome}");

    let session: Value = http
        .get(format!("http://tern/v1/runs/{run_id}/session"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sid = session["session_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let detail = http
        .get(format!("http://tern/v1/sessions/{sid}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(detail.contains("timed_out"), "{detail}");
    let pending: Value = http
        .get("http://tern/v1/secret-requests")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending, json!([]), "the expired request is unlisted");
}
