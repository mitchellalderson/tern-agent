//! GUI phases G1–G2 against the daemon API: routines (schedules), session
//! runs and journal steps, `session_id` on events and `run_id` on messages.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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

fn daemon(script: &Value) -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let tern_home = home.join(".tern");
    std::fs::create_dir_all(&tern_home).unwrap();
    let script_path = tmp.path().join("script.json");
    std::fs::write(&script_path, script.to_string()).unwrap();
    let socket = tern_home.join("tern.sock");
    let child = Command::new(tern_bin())
        .arg("daemon")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("TERN_HOME", &tern_home)
        .env("TERN_PROVIDER", format!("mock:{}", script_path.display()))
        .env("TERN_SCHEDULER_TICK_MS", "200")
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

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.0.request(method, format!("http://tern{path}"));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        let (status, v) = self.call(reqwest::Method::GET, path, None).await;
        assert_eq!(status, 200, "GET {path}: {v}");
        v
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn routines_crud_preview_and_run_now() {
    let d = daemon(&json!({ "turns": [{ "text": "Morning summary done." }] }));
    let api = Api::new(&d);
    use reqwest::Method;

    let (status, v) = api
        .call(
            Method::POST,
            "/v1/schedules/preview",
            Some(json!({ "cron": "0 8 * * mon-fri" })),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(v["next"].as_array().unwrap().len(), 3);
    let (status, v) = api
        .call(
            Method::POST,
            "/v1/schedules/preview",
            Some(json!({ "cron": "nope" })),
        )
        .await;
    assert_eq!(status, 400);
    assert!(v["error"].as_str().unwrap().contains("invalid cron"));

    let (status, created) = api
        .call(
            Method::POST,
            "/v1/schedules",
            Some(json!({ "cron": "0 8 * * 1-5", "prompt": " summarize my inbox " })),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["prompt"], "summarize my inbox");
    assert!(created["next_run_at"].as_i64().unwrap() > 0);
    assert!(created["deliver_to"].is_null());
    let (status, _) = api
        .call(
            Method::POST,
            "/v1/schedules",
            Some(json!({ "cron": "* * * * *", "prompt": "x", "deliver_to": "slack:1" })),
        )
        .await;
    assert_eq!(status, 400);

    // Disable, then edit the prompt; disabled schedules have no next run.
    let (status, v) = api
        .call(
            Method::PATCH,
            &format!("/v1/schedules/{id}"),
            Some(json!({ "enabled": false, "prompt": "summarize my inbox briefly" })),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["enabled"], false);
    assert!(v["next_run_at"].is_null());

    // Run now: the run lands in the schedule's own session.
    let (status, started) = api
        .call(Method::POST, &format!("/v1/schedules/{id}/run"), None)
        .await;
    assert_eq!(status, 200, "{started}");
    assert_eq!(started["session_id"], created["session_id"]);
    let run = started["run_id"].as_str().unwrap();
    let outcome = api.get(&format!("/v1/runs/{run}/wait")).await;
    assert_eq!(outcome["status"], "completed");

    let list = api.get("/v1/schedules").await;
    assert_eq!(list[0]["last_status"], "completed");
    assert_eq!(list[0]["prompt"], "summarize my inbox briefly");

    let (status, _) = api
        .call(Method::DELETE, &format!("/v1/schedules/{id}"), None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(api.get("/v1/schedules").await, json!([]));
    let (status, _) = api
        .call(Method::DELETE, &format!("/v1/schedules/{id}"), None)
        .await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_steps_and_event_sessions() {
    let d = daemon(&json!({ "turns": [
        { "text": "Writing.", "tool_calls": [{ "name": "write_file", "input": { "path": "a.txt", "content": "hi" } }] },
        { "text": "Wrote it." }
    ]}));
    let api = Api::new(&d);

    let mut events = api.0.get("http://tern/v1/events").send().await.unwrap();
    let started: Value = api
        .0
        .post("http://tern/v1/runs")
        .json(&json!({ "text": "write a file" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let (run, session) = (
        started["run_id"].as_str().unwrap().to_owned(),
        started["session_id"].as_str().unwrap().to_owned(),
    );

    // Every agent event names its session, not only run_started.
    let mut buf = String::new();
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(60), events.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        if buf.contains("\"run_finished\"") {
            break;
        }
    }
    let evs: Vec<Value> = buf
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|l| serde_json::from_str(l.trim()).unwrap())
        .collect();
    assert!(evs.iter().any(|e| e["type"] == "tool_finished"));
    for e in &evs {
        assert_eq!(e["session_id"], session.as_str(), "{e}");
    }

    let runs = api.get(&format!("/v1/sessions/{session}/runs")).await;
    assert_eq!(runs[0]["id"], run.as_str());
    assert_eq!(runs[0]["status"], "completed");
    let steps = api.get(&format!("/v1/runs/{run}/steps")).await;
    let kinds: Vec<&str> = steps
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"llm") && kinds.contains(&"tool"),
        "{kinds:?}"
    );
    assert!(
        steps
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["status"] == "completed")
    );

    let detail = api.get(&format!("/v1/sessions/{session}")).await;
    let assistant = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["run_id"], run.as_str());

    let (status, _) = api
        .call(reqwest::Method::GET, "/v1/runs/nope/steps", None)
        .await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_list_their_effect_and_untrusted_flag() {
    let d = daemon(&json!({ "turns": [] }));
    let api = Api::new(&d);
    let tools = api.get("/v1/tools").await;
    let find = |name: &str| {
        tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no {name} in {tools}"))
    };
    let fetch = find("http_fetch");
    assert_eq!(
        (fetch["effect"].as_str(), fetch["untrusted"].as_bool()),
        (Some("read"), Some(true))
    );
    assert!(fetch["input_schema"].is_object(), "{fetch}");
    assert_eq!(find("write_file")["effect"], "write");
    assert_eq!(find("shell")["effect"], "destructive");
    assert_eq!(find("read_file")["untrusted"], false);
}
