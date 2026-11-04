//! M5 end-to-end: the daemon, Telegram gateway (against a fake Bot API),
//! scheduler, crash recovery, and the Unix-socket API.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The binary under test: `TERN_TEST_BIN` (e.g. a `--profile dist` build,
/// whose settings can differ from test builds) or this crate's build.
fn tern_bin() -> std::path::PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

use serde_json::{Value, json};

const CHAT: i64 = 4242;
const USER: i64 = 4242;

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct FakeTelegram {
    _proc: Killed,
    url: String,
    http: reqwest::Client,
}

impl FakeTelegram {
    fn start() -> Self {
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_telegram.py");
        let mut child = Command::new("python3")
            .arg(script)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port: u16 = line.trim().parse().unwrap();
        Self {
            _proc: Killed(child),
            url: format!("http://127.0.0.1:{port}"),
            http: reqwest::Client::new(),
        }
    }

    async fn user_says(&self, text: &str) {
        self.http
            .post(format!("{}/control/message", self.url))
            .json(&json!({ "chat_id": CHAT, "user_id": USER, "text": text }))
            .send()
            .await
            .unwrap();
    }

    async fn tap(&self, message_id: i64, data: &str) {
        self.http
            .post(format!("{}/control/callback", self.url))
            .json(&json!({ "chat_id": CHAT, "user_id": USER, "message_id": message_id, "data": data }))
            .send()
            .await
            .unwrap();
    }

    async fn messages(&self) -> Vec<Value> {
        self.http
            .get(format!("{}/control/messages", self.url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// Wait for a bot message whose text contains `needle`.
    async fn wait_for(&self, needle: &str, secs: u64) -> Value {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(m) = self
                .messages()
                .await
                .into_iter()
                .find(|m| m["text"].as_str().unwrap_or("").contains(needle))
            {
                return m;
            }
            assert!(
                Instant::now() < end,
                "no bot message containing {needle:?}; got {:#?}",
                self.messages().await
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    tern_home: PathBuf,
    script: PathBuf,
}

impl Env {
    fn new(telegram: Option<&FakeTelegram>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(&tern_home).unwrap();
        if let Some(t) = telegram {
            std::fs::write(
                tern_home.join("config.toml"),
                format!(
                    // No quiet hours: the default window would hold the
                    // scheduled delivery when the tests run at night.
                    "[gateway.telegram]\nenabled = true\nbot_token_secret = \"TELEGRAM_BOT_TOKEN\"\napi_url = \"{}\"\n\n[schedules]\nquiet_hours = \"none\"\n",
                    t.url
                ),
            )
            .unwrap();
        }
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
            .env("TELEGRAM_BOT_TOKEN", "TEST:TOKEN")
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

    async fn pair(&self, tg: &FakeTelegram) {
        let out = self.cmd(&["gateway", "pair"]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let code = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        assert_eq!(code.len(), 8);
        tg.user_says(&code).await;
        tg.wait_for("Paired.", 10).await;
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.tern_home.join("tern.db")).unwrap()
    }
}

fn api(socket: &Path) -> reqwest::Client {
    reqwest::Client::builder()
        .unix_socket(socket)
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn telegram_pairing_streaming_and_button_approvals() {
    let tg = FakeTelegram::start();
    let env = Env::new(Some(&tg));
    env.script(json!({ "turns": [
        { "text": "Checking.", "tool_calls": [{ "name": "shell", "input": { "command": "echo approved-by-button" } }] },
        { "text": "All done: the command printed approved-by-button." }
    ]}));
    let _daemon = env.daemon();

    tg.user_says("hello?").await;
    tg.wait_for("This bot is private", 10).await;

    env.pair(&tg).await;
    tg.user_says("run the command").await;
    let prompt = tg
        .wait_for("Allow destructive · shell echo approved-by-button?", 10)
        .await;
    let buttons = prompt["buttons"].as_array().unwrap();
    let labels: Vec<&str> = buttons
        .iter()
        .map(|b| b["text"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Allow once", "Always (echo *)", "Deny"]);
    let once = buttons[0]["callback_data"].as_str().unwrap();
    tg.tap(prompt["message_id"].as_i64().unwrap(), once).await;

    let done = tg
        .wait_for("All done: the command printed approved-by-button.", 15)
        .await;
    assert!(
        !done["text"].as_str().unwrap().contains('▌'),
        "final text replaces the streaming view"
    );
    let approval = tg.wait_for("→ Allowed", 5).await;
    assert!(
        approval["buttons"].as_array().unwrap().is_empty(),
        "buttons removed after answering"
    );

    tg.user_says("/status").await;
    tg.wait_for("Idle. Model: mock:mock.", 10).await;
}

/// Spec §16 M5: "Every weekday at 8am, summarize my unread GitHub
/// notifications" creates a schedule that runs and delivers to Telegram.
#[tokio::test(flavor = "multi_thread")]
async fn a_schedule_created_from_telegram_runs_and_delivers_there() {
    let tg = FakeTelegram::start();
    let env = Env::new(Some(&tg));
    env.script(json!({ "conversations": [
        { "matches": "Every weekday at 8am", "turns": [
            { "tool_calls": [{ "name": "schedule_create", "input": {
                "cron": "0 8 * * 1-5", "prompt": "Summarize my unread GitHub notifications",
                "initiative": "interrupt" } }] },
            { "text": "Scheduled for weekdays at 8:00." }
        ]},
        { "matches": "Summarize my unread GitHub notifications", "turns": [
            { "text": "You have 3 unread GitHub notifications: 2 review requests and 1 mention." }
        ]}
    ]}));
    let _daemon = env.daemon();
    env.pair(&tg).await;

    tg.user_says("Every weekday at 8am, summarize my unread GitHub notifications")
        .await;
    tg.wait_for("Scheduled for weekdays at 8:00.", 15).await;

    let (id, next, deliver): (String, i64, String) = env
        .db()
        .query_row(
            "SELECT id, next_run_at, deliver_to FROM schedules",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(deliver, format!("telegram:{CHAT}"));
    let next_local = chrono::DateTime::from_timestamp_millis(next)
        .unwrap()
        .with_timezone(&chrono::Local);
    assert_eq!(next_local.format("%H:%M").to_string(), "08:00");
    assert!(!matches!(
        chrono::Datelike::weekday(&next_local),
        chrono::Weekday::Sat | chrono::Weekday::Sun
    ));

    // Fast-forward: make it due now.
    env.db()
        .execute(
            "UPDATE schedules SET next_run_at = ?1 WHERE id = ?2",
            (tern_store::now_ms() - 1000, &id),
        )
        .unwrap();
    tg.wait_for("You have 3 unread GitHub notifications", 15)
        .await;

    let (last, next_after): (Option<i64>, i64) = env
        .db()
        .query_row("SELECT last_run_at, next_run_at FROM schedules", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert!(last.is_some());
    assert!(
        next_after > tern_store::now_ms(),
        "advanced to the next weekday, no backfill"
    );
    let cron_runs: i64 = env
        .db()
        .query_row(
            "SELECT count(*) FROM runs WHERE trigger = 'cron' AND status = 'completed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cron_runs, 1);
}

/// Spec §16 M5: killing the daemon mid-run and restarting resumes the run
/// and delivers the result.
#[tokio::test(flavor = "multi_thread")]
async fn killed_daemon_resumes_the_run_and_delivers_after_restart() {
    let tg = FakeTelegram::start();
    let env = Env::new(Some(&tg));
    env.script(json!({ "turns": [
        { "tool_calls": [{ "name": "write_file", "input": { "path": "step1.txt", "content": "done" } }] },
        { "text": "Slow result ready.", "delay_ms": 2500 }
    ]}));
    let daemon = env.daemon();
    env.pair(&tg).await;
    tg.user_says("do the slow task").await;

    // Wait until the tool step is done and the slow model call is in flight.
    let marker = env.home.join("tern-workspace/step1.txt");
    let end = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(Instant::now() < end, "tool never ran");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(daemon); // SIGKILL
    let status: String = env
        .db()
        .query_row("SELECT status FROM runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        status, "running",
        "a killed daemon leaves the run to resume"
    );
    assert!(!tg.messages().await.iter().any(|m| {
        m["text"]
            .as_str()
            .unwrap_or("")
            .contains("Slow result ready.")
    }));

    let _daemon = env.daemon();
    tg.wait_for("Slow result ready.", 20).await;
    let (status, tools): (String, i64) = env
        .db()
        .query_row(
            "SELECT r.status, (SELECT count(*) FROM steps s WHERE s.run_id = r.id AND s.kind = 'tool') FROM runs r",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((status.as_str(), tools), ("completed", 1));
    // The restart didn't re-handle the chat message: one run, one reply.
    let runs: i64 = env
        .db()
        .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 1);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let replies = tg
        .messages()
        .await
        .into_iter()
        .filter(|m| {
            m["text"]
                .as_str()
                .unwrap_or("")
                .contains("Slow result ready.")
        })
        .count();
    assert_eq!(replies, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn long_data_directories_still_get_a_working_socket() {
    let env = Env::new(None);
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
    let env = Env::new(None);
    env.script(json!({ "turns": [
        { "tool_calls": [{ "name": "shell", "input": { "command": "echo via-api" } }] },
        { "text": "API run finished." }
    ]}));
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
    let approval_id = loop {
        let chunk = tokio::time::timeout(Duration::from_secs(10), events.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        if let Some(line) = buf.lines().find(|l| l.contains("approval_requested")) {
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

/// docs/secret-requests-spec.md §5.4: a run in a Telegram chat that asks
/// for a secret gets a pointer to the app there, never a field; the app
/// answers, and the pointer is updated.
#[tokio::test(flavor = "multi_thread")]
async fn telegram_secret_requests_point_to_the_app() {
    let tg = FakeTelegram::start();
    let env = Env::new(Some(&tg));
    env.script(json!({ "turns": [
        { "tool_calls": [{ "name": "secret_request", "input": {
            "names": ["WEATHER_API_KEY"], "reason": "The weather connector needs it." } }] },
        { "text": "Thanks, the key is saved." }
    ]}));
    let _daemon = env.daemon();
    env.pair(&tg).await;
    tg.user_says("set up weather").await;

    let notice = tg.wait_for("tern needs WEATHER_API_KEY", 10).await;
    let text = notice["text"].as_str().unwrap();
    assert!(
        text.contains("The agent says: \"The weather connector needs it.\""),
        "{text}"
    );
    assert!(text.contains("not here"), "{text}");
    assert!(notice["buttons"].as_array().unwrap().is_empty());

    // The app answers through the API.
    let http = api(&env.socket());
    let pending: Value = http
        .get("http://tern/v1/secret-requests")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = pending[0]["id"].as_str().unwrap();
    let ok = http
        .post(format!("http://tern/v1/secret-requests/{id}"))
        .json(&json!({ "values": { "WEATHER_API_KEY": "wk-from-the-app-123" } }))
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success());

    tg.wait_for("✓ WEATHER_API_KEY saved", 10).await;
    tg.wait_for("Thanks, the key is saved.", 10).await;
    assert!(
        !tg.messages()
            .await
            .iter()
            .any(|m| m["text"].as_str().unwrap_or("").contains("wk-from-the-app")),
        "the value never reaches the chat"
    );
}
