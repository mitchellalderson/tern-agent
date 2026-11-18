//! MCP server management over the daemon API (docs/mcp-management-spec.md,
//! M0): import, verify, add, a run using the new tools with no restart,
//! toggles, removal, and the stdio-over-TCP rule. The server is the
//! stdlib-only Python fixture from tern-mcp.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(&tern_home).unwrap();
        std::fs::write(tern_home.join("config.toml"), "# my settings\n").unwrap();
        Self {
            script: tmp.path().join("script.json"),
            home,
            tern_home,
            _tmp: tmp,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tern"));
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env("TERN_PROVIDER", format!("mock:{}", self.script.display()))
            .stdin(Stdio::null());
        c
    }

    fn daemon(&self) -> Killed {
        let child = self
            .cmd(&["daemon"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while !self.tern_home.join("tern.sock").exists() {
            assert!(Instant::now() < end, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Killed(child)
    }

    fn local(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .unix_socket(self.tern_home.join("tern.sock"))
            .build()
            .unwrap()
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.tern_home.join(file)).unwrap_or_default()
    }
}

fn fixture() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tern-mcp/tests/fixtures/mcp_server.py"
    )
    .into()
}

fn fx_config() -> Value {
    json!({
        "transport": "stdio",
        "command": "python3",
        "args": [fixture()],
        "env_secrets": ["FIXTURE_TOKEN"],
        // So the test run needs no approval.
        "effects": { "add": "read" },
    })
}

async fn send(req: reqwest::RequestBuilder) -> (u16, Value) {
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

/// Poll `/v1/mcp/{name}` until `want` holds.
async fn wait_state(http: &reqwest::Client, name: &str, want: impl Fn(&Value) -> bool) -> Value {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, s) = send(http.get(format!("http://tern/v1/mcp/{name}"))).await;
        if want(&s) {
            return s;
        }
        assert!(Instant::now() < end, "{name} never reached the state: {s}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn manage_mcp_servers_without_a_restart() {
    let env = Env::new();
    std::fs::write(
        &env.script,
        json!({ "turns": [
            { "tool_calls": [{ "name": "mcp__fx__add", "input": { "a": 1234, "b": 4321 } }] },
            { "text": "done" },
        ]})
        .to_string(),
    )
    .unwrap();
    let _daemon = env.daemon();
    let http = env.local();
    let mut events = http.get("http://tern/v1/events").send().await.unwrap();

    let (status, list) = send(http.get("http://tern/v1/mcp")).await;
    assert_eq!((status, list), (200, json!([])));

    // Import a Claude Desktop snippet: the token becomes a missing secret.
    let (status, drafts) = send(http.post("http://tern/v1/mcp/import").json(&json!({
        "text": r#"{"mcpServers": {"github": {"command": "github-mcp-server", "args": ["stdio"],
                    "env": {"GITHUB_TOKEN": "<YOUR_TOKEN>", "LOG_LEVEL": "warn"}}}}"#
    })))
    .await;
    assert_eq!(status, 200, "{drafts}");
    assert_eq!(drafts[0]["name"], "github");
    assert_eq!(drafts[0]["config"]["env_secrets"], json!(["GITHUB_TOKEN"]));
    assert_eq!(drafts[0]["config"]["env"], json!({"LOG_LEVEL": "warn"}));
    assert_eq!(drafts[0]["secrets"], json!({"GITHUB_TOKEN": null}));

    // Verify reports a crashing server's stderr, and saves nothing.
    let mut crashing = fx_config();
    crashing["args"] = json!([fixture(), "--fail-start", "Error: bad credentials"]);
    let (_, v) = send(http.post("http://tern/v1/mcp/verify").json(&json!({
        "name": "fx", "config": crashing, "secrets": {"FIXTURE_TOKEN": "tok-123"}
    })))
    .await;
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["error"]["kind"], "exited_early", "{v}");
    assert_eq!(v["error"]["stderr_tail"], json!(["Error: bad credentials"]));
    let (_, v) = send(http.post("http://tern/v1/mcp/verify").json(&json!({
        "name": "fx", "config": fx_config(), "secrets": {"FIXTURE_TOKEN": "tok-123"}
    })))
    .await;
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["verified"]["tools"].as_array().unwrap().len(), 5);
    assert!(!crate::read_secrets(&env.tern_home).contains("tok-123"));
    assert!(!env.read("config.toml").contains("mcp"));

    // Add: the secret goes to secrets.toml, the config to config.toml, and
    // the server starts straight away.
    let (status, s) = send(http.post("http://tern/v1/mcp").json(&json!({
        "name": "fx", "config": fx_config(), "secrets": {"FIXTURE_TOKEN": "tok-123"}
    })))
    .await;
    assert_eq!(status, 201, "{s}");
    let running = wait_state(&http, "fx", |s| s["state"]["kind"] == "running").await;
    assert_eq!(running["state"]["tools"], 5);
    assert_eq!(running["server_info"], "fixture 1.0");
    assert!(crate::read_secrets(&env.tern_home).contains("tok-123"));
    let config = env.read("config.toml");
    assert!(
        config.contains("[mcp.servers.fx]") && config.contains("# my settings"),
        "{config}"
    );
    assert!(!config.contains("tok-123"));
    let (status, _) = send(http.post("http://tern/v1/mcp").json(&json!({
        "name": "fx", "config": fx_config()
    })))
    .await;
    assert_eq!(status, 409);

    // State changes reach /v1/events.
    let end = Instant::now() + Duration::from_secs(60);
    let mut seen = String::new();
    while !(seen.contains("mcp_changed") && seen.contains("\"running\"")) {
        assert!(Instant::now() < end, "no mcp_changed event in {seen}");
        let chunk = tokio::time::timeout(Duration::from_secs(60), events.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }

    // The next run sees the new tools, with no restart.
    let (_, started) = send(
        http.post("http://tern/v1/runs")
            .json(&json!({ "text": "go" })),
    )
    .await;
    let run_id = started["run_id"].as_str().unwrap().to_owned();
    send(http.get(format!("http://tern/v1/runs/{run_id}/wait"))).await;
    let (_, sid) = send(http.get(format!("http://tern/v1/runs/{run_id}/session"))).await;
    let sid = sid["session_id"].as_str().unwrap().to_owned();
    let (_, session) = send(http.get(format!("http://tern/v1/sessions/{sid}"))).await;
    let session = session.to_string();
    // Only the server's answer has the sum.
    assert!(session.contains("5555"), "{session}");
    assert!(!session.contains("unknown tool"), "{session}");

    // Disable hides its tools; enable brings them back.
    let (status, s) = send(
        http.patch("http://tern/v1/mcp/fx")
            .json(&json!({ "enabled": false })),
    )
    .await;
    assert_eq!(
        (status, &s["state"]["kind"]),
        (200, &json!("disabled")),
        "{s}"
    );
    let (_, tools) = send(http.get("http://tern/v1/tools")).await;
    assert!(!tools.to_string().contains("mcp__fx__"));
    send(
        http.patch("http://tern/v1/mcp/fx")
            .json(&json!({ "enabled": true })),
    )
    .await;
    wait_state(&http, "fx", |s| s["state"]["kind"] == "running").await;
    let (status, s) = send(http.post("http://tern/v1/mcp/fx/restart")).await;
    assert_eq!(
        (status, &s["state"]["kind"]),
        (200, &json!("running")),
        "{s}"
    );

    // Remove, with its secret.
    let (status, _) = send(http.delete("http://tern/v1/mcp/fx?remove_secrets=true")).await;
    assert_eq!(status, 200);
    assert!(!crate::read_secrets(&env.tern_home).contains("tok-123"));
    assert!(!env.read("config.toml").contains("mcp.servers.fx"));
    let (status, _) = send(http.get("http://tern/v1/mcp/fx")).await;
    assert_eq!(status, 404);
    let (status, _) = send(http.delete("http://tern/v1/mcp/fx")).await;
    assert_eq!(status, 404);
}

/// Wait for the next approval, as `/v1/approvals` lists it.
async fn next_approval(http: &reqwest::Client) -> Value {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, list) = send(http.get("http://tern/v1/approvals")).await;
        if let Some(a) = list.as_array().and_then(|l| l.first()) {
            return a.clone();
        }
        assert!(Instant::now() < end, "no approval was asked for");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The session's messages as text, for checking tool results.
async fn session_text(http: &reqwest::Client, run_id: &str) -> String {
    send(http.get(format!("http://tern/v1/runs/{run_id}/wait"))).await;
    let (_, sid) = send(http.get(format!("http://tern/v1/runs/{run_id}/session"))).await;
    let sid = sid["session_id"].as_str().unwrap().to_owned();
    let (_, session) = send(http.get(format!("http://tern/v1/sessions/{sid}"))).await;
    session.to_string()
}

/// A run in the test's one session (the mock provider picks its turn from
/// the session's history).
async fn start(http: &reqwest::Client, session: &mut Option<String>) -> String {
    let (_, started) = send(
        http.post("http://tern/v1/runs")
            .json(&json!({ "text": "go", "session_id": session })),
    )
    .await;
    *session = started["session_id"].as_str().map(str::to_owned);
    started["run_id"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_adds_servers_with_approval_on_the_host() {
    let env = Env::new();
    let mut add = fx_config();
    add["name"] = json!("fx");
    std::fs::write(
        &env.script,
        json!({ "turns": [
            // 1: the secret isn't set yet, so nothing is saved.
            { "tool_calls": [{ "name": "mcp_add", "input": add }] },
            { "text": "need the token" },
            // 2: the same call once it is.
            { "tool_calls": [{ "name": "mcp_add", "input": add }] },
            { "text": "added" },
            // 3: its tools work in the next run.
            { "tool_calls": [{ "name": "mcp__fx__add", "input": { "a": 1234, "b": 4321 } }] },
            { "text": "done" },
            // 4: remove it, secret too.
            { "tool_calls": [{ "name": "mcp_remove", "input": { "name": "fx", "remove_secrets": true } }] },
            { "text": "removed" },
        ]})
        .to_string(),
    )
    .unwrap();
    let _daemon = env.daemon();
    let http = env.local();
    // Approvals go to listening clients; with none they're denied at once.
    let _events = http.get("http://tern/v1/events").send().await.unwrap();
    let mut session = None;
    // Run 1: approval is asked every time and can't be "always".
    let run = start(&http, &mut session).await;
    let a = next_approval(&http).await;
    assert_eq!(a["tool"], "mcp_add");
    assert_eq!(a["local_only"], true, "{a}");
    assert_eq!(a["pattern"], Value::Null);
    assert_eq!(a["effect"], "destructive");
    let summary = a["summary"].as_str().unwrap();
    assert!(
        summary.contains("outside the sandbox") && summary.contains("python3"),
        "{summary}"
    );
    let id = a["id"].as_str().unwrap();
    send(
        http.post(format!("http://tern/v1/approvals/{id}"))
            .json(&json!({ "decision": "once" })),
    )
    .await;
    let text = session_text(&http, &run).await;
    assert!(
        text.contains("FIXTURE_TOKEN not set") && text.contains("secret_request"),
        "{text}"
    );
    assert!(!env.read("config.toml").contains("mcp.servers.fx"));

    // Run 2: with the secret set (as secret_request would), it's tested and saved.
    send(
        http.put("http://tern/v1/secrets/FIXTURE_TOKEN")
            .json(&json!({ "value": "tok-123" })),
    )
    .await;
    let run = start(&http, &mut session).await;
    let a = next_approval(&http).await;
    send(
        http.post(format!(
            "http://tern/v1/approvals/{}",
            a["id"].as_str().unwrap()
        ))
        .json(&json!({ "decision": "once" })),
    )
    .await;
    let text = session_text(&http, &run).await;
    assert!(text.contains(r#"\"saved\":true"#), "{text}");
    assert!(text.contains("mcp__fx__add"), "{text}");
    wait_state(&http, "fx", |s| s["state"]["kind"] == "running").await;

    // Run 3: the new tools are offered (add is marked read, so no approval).
    let run = start(&http, &mut session).await;
    let text = session_text(&http, &run).await;
    assert!(text.contains("5555"), "{text}");

    // Run 4: removal is asked for too, and takes the secret with it.
    let run = start(&http, &mut session).await;
    let a = next_approval(&http).await;
    assert_eq!(a["tool"], "mcp_remove");
    assert_eq!(a["pattern"], Value::Null);
    send(
        http.post(format!(
            "http://tern/v1/approvals/{}",
            a["id"].as_str().unwrap()
        ))
        .json(&json!({ "decision": "once" })),
    )
    .await;
    session_text(&http, &run).await;
    let (status, _) = send(http.get("http://tern/v1/mcp/fx")).await;
    assert_eq!(status, 404);
    assert!(!crate::read_secrets(&env.tern_home).contains("tok-123"));
}
