//! Connections end to end (tooling spec §6.7–6.8): the agent's sign-in
//! card through the daemon, adding a key-based account over the API, and
//! the key never reaching the journal, the event stream or the logs.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const KEY: &str = "lin-api-9d3e7a51c06b2f48";

const GMAIL: &str = r#"return {
  name = "gmail",
  description = "Mail",
  capabilities = { http = { "gmail.googleapis.com" }, oauth = { google = { scopes = { "https://www.googleapis.com/auth/gmail.readonly" } } } },
  tools = { inbox = { description = "Inbox", effect = "read", run = function(ctx) return ctx.connection.slug end } },
}"#;

const LINEAR: &str = r#"return {
  name = "linear",
  description = "Issues",
  capabilities = { http = { "api.linear.app" }, auth = "LINEAR_KEY" },
  tools = { me = { description = "Me", effect = "read", run = function(ctx) return ctx.connection.slug end } },
}"#;

fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

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
    let end = Instant::now() + Duration::from_secs(60);
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
        let chunk = tokio::time::timeout(Duration::from_secs(60), events.chunk())
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

fn install(env: &Env) {
    let dir = env.tern_home.join("connectors");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("gmail.luau"), GMAIL).unwrap();
    std::fs::write(dir.join("linear.luau"), LINEAR).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agents_sign_in_card_reaches_clients_and_cancelling_skips() {
    let env = Env::new(
        "[auth.clients]\ngoogle = { client_id = \"test-client.apps.googleusercontent.com\" }\n",
        &json!({ "turns": [
            { "tool_calls": [{ "name": "connection_add", "input": { "target": "gmail", "connection": "work" } }] },
            { "text": "Okay." }
        ]}),
    );
    install(&env);
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
            .contains(&json!("connections"))
    );
    let mut events = http.get("http://tern/v1/events").send().await.unwrap();
    let (mut buf, mut rest) = (String::new(), String::new());
    let run_id = start(&http, "connect my work gmail").await;
    let ev = wait_for(&http, &mut events, &mut buf, &mut rest, |e| {
        e.get("auth_prompt").is_some()
    })
    .await;
    let card = &ev["auth_prompt"];
    assert_eq!(card["run_id"], run_id.as_str());
    assert_eq!(card["card"]["kind"], "sign_in");
    assert_eq!(card["card"]["title"], "Sign in to gmail · work");
    let url = card["card"]["url"].as_str().unwrap();
    assert!(
        url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
        "{url}"
    );
    assert!(
        url.contains("code_challenge_method=S256")
            && url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A")
    );
    assert!(url.contains("client_id=test-client.apps.googleusercontent.com"));
    assert_eq!(card["card"]["device_available"], true);
    let id = card["id"].as_str().unwrap().to_owned();
    let call_id = card["call_id"].as_str().unwrap().to_owned();
    // Late clients find it; only answers the card offers are taken.
    let pending: Value = http
        .get("http://tern/v1/auth-prompts")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending[0]["id"], id.as_str());
    let bad = http
        .post(format!("http://tern/v1/auth-prompts/{id}"))
        .json(&json!({ "answer": "oauth" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let ok = http
        .post(format!("http://tern/v1/auth-prompts/{id}"))
        .json(&json!({ "answer": "cancel" }))
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success());
    // The card closes and the tool finishes; the two come from different
    // streams, so in either order.
    let (mut closed, mut done) = (None, None);
    while closed.is_none() || done.is_none() {
        let ev = wait_for(&http, &mut events, &mut buf, &mut rest, |e| {
            e.get("auth_prompt_closed").is_some()
                || (e["type"] == "tool_finished" && e["id"] == call_id.as_str())
        })
        .await;
        if ev.get("auth_prompt_closed").is_some() {
            closed = Some(ev);
        } else {
            done = Some(ev);
        }
    }
    assert_eq!(closed.unwrap()["auth_prompt_closed"], id.as_str());
    let done = done.unwrap();
    assert!(
        done["output"]
            .as_str()
            .unwrap()
            .contains("\"status\":\"skipped\""),
        "{done}"
    );
    assert_eq!(wait_run(&http, &run_id).await["status"], "completed");
    let list: Value = http
        .get("http://tern/v1/connections")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["connections"], json!([]));
    let gmail = list["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "gmail")
        .unwrap();
    assert_eq!(gmail["methods"], json!(["oauth"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn accounts_over_the_api_and_the_key_stays_private() {
    let env = Env::new("", &json!({ "turns": [{ "text": "hi" }] }));
    install(&env);
    let _daemon = env.daemon();
    let http = api(&env.socket());
    let mut events = http.get("http://tern/v1/events").send().await.unwrap();
    let (mut buf, mut rest) = (String::new(), String::new());
    for (slug, value) in [("work", KEY.to_owned()), ("side", format!("{KEY}-side"))] {
        let r = http
            .post("http://tern/v1/connections")
            .json(&json!({ "target": "linear", "connection": slug, "values": { "LINEAR_KEY": value } }))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}", r.text().await.unwrap());
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["status"], "connected");
        assert_eq!(v["connection"]["auth"]["method"], "key");
    }
    wait_for(&http, &mut events, &mut buf, &mut rest, |e| {
        e.get("connections_changed").is_some()
    })
    .await;
    // Unknown names and bad slugs are refused.
    let bad = http
        .post("http://tern/v1/connections")
        .json(&json!({ "target": "linear", "connection": "Bad Slug", "values": { "LINEAR_KEY": "x" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let r = http
        .post("http://tern/v1/connections/connector/linear/side/default")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let list: Value = http
        .get("http://tern/v1/connections")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let conns = list["connections"].as_array().unwrap();
    assert_eq!(conns.len(), 2);
    assert_eq!(conns[0]["slug"], "side");
    assert_eq!(conns[0]["is_default"], true);
    assert_eq!(conns[1]["title"], "linear · work");
    let r = http
        .delete("http://tern/v1/connections/connector/linear/side")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let secrets = std::fs::read_to_string(env.tern_home.join("secrets.toml")).unwrap();
    assert!(
        secrets.contains(&format!(
            "\"conn/connector:linear/work/LINEAR_KEY\" = \"{KEY}\""
        )),
        "{secrets}"
    );
    assert!(
        !secrets.contains("-side"),
        "removed with its account: {secrets}"
    );
    // Nowhere else: not the database, the logs, or the event stream.
    assert!(!buf.contains(KEY));
    let hits = files_containing(&env.tern_home, KEY);
    assert!(hits.is_empty(), "key found in {hits:?}");
}
