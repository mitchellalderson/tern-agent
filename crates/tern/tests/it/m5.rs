//! M5 end-to-end: the daemon, scheduler, crash recovery, and the
//! Unix-socket API.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The binary under test: `TERN_TEST_BIN` (e.g. a `--profile dist` build,
/// whose settings can differ from test builds) or this crate's build.
fn tern_bin() -> std::path::PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

use serde_json::{Value, json};

use crate::subagents_env;

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
        // Outside the temp dirs, so the shell's sandbox check can verify.
        let tmp = subagents_env::outside_temp();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(&tern_home).unwrap();
        Self {
            script: tmp.path().join("script.json"),
            home,
            tern_home,
            _tmp: tmp,
        }
    }

    fn script(&self, script: Value) {
        std::fs::write(&self.script, script.to_string()).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(tern_bin());
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env("TERN_PROVIDER", format!("mock:{}", self.script.display()))
            .env("TERN_SCHEDULER_TICK_MS", "200")
            .stdin(Stdio::null());
        c
    }

    fn socket(&self) -> PathBuf {
        self.tern_home.join("tern.sock")
    }

    fn daemon(&self) -> Killed {
        let _ = std::fs::remove_file(self.socket());
        let child = self
            .cmd(&["daemon"])
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

#[tokio::test(flavor = "multi_thread")]
async fn long_data_directories_still_get_a_working_socket() {
    let env = Env::new();
    env.script(json!({ "turns": [] }));
    let deep = env
        .home
        .join("a".repeat(60))
        .join("b".repeat(60))
        .join(".tern");
    std::fs::create_dir_all(&deep).unwrap();
    assert!(deep.join("tern.sock").as_os_str().len() > 104);
    let mut cmd = env.cmd(&["daemon"]);
    cmd.env("TERN_HOME", &deep)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let daemon = Killed(cmd.spawn().unwrap());
    let link = deep.join("tern.sock");
    let end = Instant::now() + Duration::from_secs(10);
    while link.symlink_metadata().is_err() {
        assert!(
            Instant::now() < end,
            "daemon did not create its socket link"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let target = std::fs::read_link(&link).unwrap();
    assert!(target.as_os_str().len() <= 100, "{}", target.display());
    let health: Value = api(&target)
        .get("http://tern/v1/health")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(health["pid"].as_u64().is_some());
    drop(daemon);
}

#[tokio::test(flavor = "multi_thread")]
async fn socket_api_runs_streams_and_approves() {
    let env = Env::new();
    env.script(json!({ "turns": [
        { "tool_calls": [{ "name": "shell", "input": { "command": "echo via-api" } }] },
        { "text": "API run finished." }
    ]}));
    // A new install is autonomous, which runs a sandboxed `echo` without
    // asking. This test answers an approval, so it pins the supervised level.
    std::fs::write(
        env.tern_home.join("config.toml"),
        "autonomy = \"supervised\"\n",
    )
    .unwrap();
    let _daemon = env.daemon();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(env.socket())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    // A second daemon on the same home refuses to start.
    let second = env.cmd(&["daemon"]).output().unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"));

    let http = api(&env.socket());
    let health: Value = http
        .get("http://tern/v1/health")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));

    // Subscribe to events, then start a run.
    let mut events = http.get("http://tern/v1/events").send().await.unwrap();
    let started: Value = http
        .post("http://tern/v1/runs")
        .json(&json!({ "text": "run it over the api" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let run_id = started["run_id"].as_str().unwrap().to_owned();

    // The shell call needs approval: answer it from the event stream.
    let mut buf = String::new();
    // Keep-alives arrive every 15s, so the per-chunk timeout alone never
    // ends a stream that has no approval in it: give the whole wait a limit.
    let deadline = Instant::now() + Duration::from_secs(180);
    let approval_id = loop {
        assert!(
            Instant::now() < deadline,
            "no approval_requested within 180s; so far: {buf:?}"
        );
        // Slower than the 15s keep-alive, so a loaded runner can't beat it.
        let chunk = tokio::time::timeout(Duration::from_secs(60), events.chunk())
            .await
            .unwrap_or_else(|_| panic!("no event within 60s; so far: {buf:?}"))
            .unwrap()
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        if let Some(line) = buf
            .lines()
            .find(|l| l.contains("approval_requested") && l.ends_with('}'))
        {
            let ev: Value = serde_json::from_str(line.trim_start_matches("data:").trim()).unwrap();
            assert_eq!(ev["approval_requested"]["summary"], "shell echo via-api");
            break ev["approval_requested"]["id"].as_str().unwrap().to_owned();
        }
    };
    // A client attaching now can find the run and the waiting approval.
    let pending: Value = http
        .get("http://tern/v1/approvals")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending[0]["id"], approval_id.as_str());
    let active: Value = http
        .get("http://tern/v1/runs/active")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(active[0]["run_id"], run_id.as_str());
    let ok = http
        .post(format!("http://tern/v1/approvals/{approval_id}"))
        .json(&json!({ "decision": "once" }))
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success());

    let outcome: Value = http
        .get(format!("http://tern/v1/runs/{run_id}/wait"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(outcome["status"], "completed");
    assert_eq!(outcome["final_text"], "API run finished.");
    let sessions: Value = http
        .get("http://tern/v1/sessions")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sessions[0]["id"], started["session_id"]);
    let detail: Value = http
        .get(format!(
            "http://tern/v1/sessions/{}",
            started["session_id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(detail["messages"].as_array().unwrap().len() >= 4);
}
