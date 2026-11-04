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
    port: u16,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(&tern_home).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        std::fs::write(
            tern_home.join("config.toml"),
            format!(
                "# my settings\n[api]\nlisten = [\"unix\", \"tcp://127.0.0.1:{port}\"]\ntls = \"auto\"\n"
            ),
        )
        .unwrap();
        Self {
            script: tmp.path().join("script.json"),
            home,
            tern_home,
            port,
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

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "tern {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn daemon(&self) -> Killed {
        let child = self
            .cmd(&["daemon"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err()
            || !self.tern_home.join("tern.sock").exists()
        {
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
    assert!(!env.read("secrets.toml").contains("tok-123"));
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
    assert!(env.read("secrets.toml").contains("tok-123"));
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
    let end = Instant::now() + Duration::from_secs(10);
    let mut seen = String::new();
    while !(seen.contains("mcp_changed") && seen.contains("\"running\"")) {
        assert!(Instant::now() < end, "no mcp_changed event in {seen}");
        let chunk = tokio::time::timeout(Duration::from_secs(10), events.chunk())
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
    assert!(!env.read("secrets.toml").contains("tok-123"));
    assert!(!env.read("config.toml").contains("mcp.servers.fx"));
    let (status, _) = send(http.get("http://tern/v1/mcp/fx")).await;
    assert_eq!(status, 404);
    let (status, _) = send(http.delete("http://tern/v1/mcp/fx")).await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn paired_devices_cannot_add_stdio_servers() {
    let env = Env::new();
    std::fs::write(&env.script, json!({ "turns": [] }).to_string()).unwrap();
    let _daemon = env.daemon();
    let link = env.ok(&["devices", "pair", "--scope", "owner", "--name", "laptop"]);
    let token = env.ok(&["devices", "join", &link]);
    let http = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let url = |p: &str| format!("https://127.0.0.1:{}{p}", env.port);
    let auth = format!("Bearer {token}");

    let (status, e) = send(
        http.post(url("/v1/mcp"))
            .header("authorization", &auth)
            .json(&json!({ "name": "fx", "config": fx_config() })),
    )
    .await;
    assert_eq!(status, 403, "{e}");
    assert!(e["error"].as_str().unwrap().contains("remote_mcp_stdio"));
    let (status, _) = send(
        http.post(url("/v1/mcp/verify"))
            .header("authorization", &auth)
            .json(&json!({ "name": "fx", "config": fx_config() })),
    )
    .await;
    assert_eq!(status, 403);

    // HTTP servers and reads are fine from a device.
    let (status, s) = send(
        http.post(url("/v1/mcp"))
            .header("authorization", &auth)
            .json(&json!({ "name": "remote", "config": {
                "transport": "http", "url": "https://mcp.example.invalid/mcp"
            }})),
    )
    .await;
    assert_eq!(status, 201, "{s}");
    let (status, list) = send(http.get(url("/v1/mcp")).header("authorization", &auth)).await;
    assert_eq!(status, 200);
    assert_eq!(list[0]["name"], "remote");

    // The local owner can add stdio servers.
    let (status, s) = send(env.local().post("http://tern/v1/mcp").json(&json!({
        "name": "fx", "config": fx_config(), "secrets": {"FIXTURE_TOKEN": "t"}
    })))
    .await;
    assert_eq!(status, 201, "{s}");
}
