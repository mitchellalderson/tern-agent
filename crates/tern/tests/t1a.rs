//! T1a end-to-end (docs/personal-agent-tooling-spec.md §4.1, §8.3, §12.1):
//! `NO_REPLY`, quiet hours (across a restart), `urgent`, and per-schedule
//! caps, against the daemon, a fake Telegram, and the mock provider.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use chrono::Timelike;
use serde_json::{Value, json};

const CHAT: i64 = 5151;

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

    async fn texts(&self) -> Vec<String> {
        let all: Vec<Value> = self
            .http
            .get(format!("{}/control/messages", self.url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        all.iter()
            .map(|m| m["text"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    async fn count(&self, needle: &str) -> usize {
        self.texts()
            .await
            .iter()
            .filter(|t| t.contains(needle))
            .count()
    }

    /// Wait for a bot message containing `needle`; returns its text.
    async fn wait_for(&self, needle: &str, secs: u64) -> String {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(t) = self.texts().await.into_iter().find(|t| t.contains(needle)) {
                return t;
            }
            assert!(
                Instant::now() < end,
                "no bot message containing {needle:?}; got {:#?}",
                self.texts().await
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
    telegram_url: String,
}

impl Env {
    fn new(tg: &FakeTelegram, extra_config: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(tern_home.join("logs")).unwrap();
        let env = Self {
            script: tmp.path().join("script.json"),
            home,
            tern_home,
            telegram_url: tg.url.clone(),
            _tmp: tmp,
        };
        env.config(extra_config);
        env
    }

    fn config(&self, extra: &str) {
        std::fs::write(
            self.tern_home.join("config.toml"),
            format!(
                "[gateway.telegram]\nenabled = true\nbot_token_secret = \"TELEGRAM_BOT_TOKEN\"\napi_url = \"{}\"\n\n{extra}",
                self.telegram_url
            ),
        )
        .unwrap();
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

    fn api(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .unix_socket(self.socket())
            .build()
            .unwrap()
    }

    async fn create(&self, body: Value) -> Value {
        let res = self
            .api()
            .post("http://tern/v1/schedules")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(res.status().is_success(), "{}", res.text().await.unwrap());
        res.json().await.unwrap()
    }

    async fn schedule(&self, id: &str) -> Value {
        let all: Vec<Value> = self
            .api()
            .get("http://tern/v1/schedules")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        all.into_iter().find(|s| s["id"] == id).unwrap()
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.tern_home.join("tern.db")).unwrap()
    }

    /// Make a schedule due now.
    fn due(&self, id: &str) {
        self.db()
            .execute(
                "UPDATE schedules SET next_run_at = ?1 WHERE id = ?2",
                (tern_store::now_ms() - 1000, id),
            )
            .unwrap();
    }

    fn query(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Wait until `sql` (a count) reaches `n`.
    async fn wait_count(&self, sql: &str, n: i64, secs: u64) {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            let got = self.query(sql);
            if got == n {
                return;
            }
            assert!(Instant::now() < end, "{sql}: {got}, wanted {n}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn logs(&self) -> String {
        let mut out = String::new();
        for e in std::fs::read_dir(self.tern_home.join("logs")).unwrap() {
            out.push_str(&std::fs::read_to_string(e.unwrap().path()).unwrap_or_default());
        }
        out
    }
}

fn target() -> String {
    format!("telegram:{CHAT}")
}

/// Quiet hours from an hour ago to an hour from now, local.
fn quiet_now() -> String {
    let now = chrono::Local::now();
    let t = now.hour() * 60 + now.minute();
    let fmt = |m: u32| format!("{:02}:{:02}", m / 60 % 24, m % 60);
    format!("{}-{}", fmt(t + 23 * 60), fmt(t + 60))
}

/// Let a few scheduler ticks pass.
async fn ticks() {
    tokio::time::sleep(Duration::from_millis(1200)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_no_reply_run_delivers_nothing_and_is_recorded_quiet() {
    let tg = FakeTelegram::start();
    let env = Env::new(&tg, "");
    env.script(json!({ "conversations": [
        { "matches": "Watch the inbox", "turns": [{ "text": "  NO_REPLY\n" }] },
        { "matches": "Watch the build", "turns": [{ "text": "The build is green." }] }
    ]}));
    let _daemon = env.daemon();
    let common = json!({ "cron": "0 3 * * *", "deliver_to": target(), "initiative": "interrupt", "quiet_hours": "none" });
    let mut quiet = common.clone();
    quiet["prompt"] = "Watch the inbox".into();
    let quiet = env.create(quiet).await;
    let mut loud = common;
    loud["prompt"] = "Watch the build".into();
    let loud = env.create(loud).await;
    assert_eq!(quiet["quiet_hours"], "none");

    env.due(quiet["id"].as_str().unwrap());
    env.wait_count(
        "SELECT count(*) FROM runs WHERE status = 'completed' AND quiet = 1",
        1,
        15,
    )
    .await;
    // A delivery that does happen shows the pipe is working.
    env.due(loud["id"].as_str().unwrap());
    tg.wait_for("The build is green.", 15).await;
    ticks().await;
    let texts = tg.texts().await;
    assert_eq!(texts, ["The build is green."], "NO_REPLY sent nothing");

    let q = env.schedule(quiet["id"].as_str().unwrap()).await;
    assert_eq!(
        (q["last_status"].as_str(), q["last_quiet"].as_bool()),
        (Some("completed"), Some(true))
    );
    let l = env.schedule(loud["id"].as_str().unwrap()).await;
    assert_eq!(l["last_quiet"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn quiet_hours_hold_deliveries_across_a_restart_and_urgent_bypasses_them() {
    let tg = FakeTelegram::start();
    let env = Env::new(
        &tg,
        &format!("[schedules]\nquiet_hours = \"{}\"\n", quiet_now()),
    );
    env.script(json!({ "conversations": [
        { "matches": "Morning brief", "turns": [{ "text": "Brief: two meetings." }] },
        { "matches": "Follow-ups", "turns": [{ "text": "Follow-ups: reply to Sam." }] },
        { "matches": "Server down?", "turns": [{ "text": "ALERT: the server is down." }] }
    ]}));
    let daemon = env.daemon();
    let mut ids = Vec::new();
    for (prompt, urgent) in [
        ("Morning brief", false),
        ("Follow-ups", false),
        ("Server down?", true),
    ] {
        let s = env
            .create(json!({ "cron": "0 3 * * *", "prompt": prompt, "deliver_to": target(), "initiative": "interrupt", "urgent": urgent }))
            .await;
        assert_eq!(s["urgent"], urgent);
        ids.push(s["id"].as_str().unwrap().to_owned());
    }
    for id in &ids {
        env.due(id);
    }
    // The urgent one goes out at once; the others are held, not delayed.
    tg.wait_for("ALERT: the server is down.", 15).await;
    env.wait_count("SELECT count(*) FROM held_deliveries", 2, 15)
        .await;
    assert_eq!(
        env.query("SELECT count(*) FROM runs WHERE status = 'completed'"),
        3
    );
    let release = env.query("SELECT min(release_at) FROM held_deliveries");
    assert!(
        release > tern_store::now_ms() + 50 * 60_000,
        "held until the window ends"
    );
    ticks().await;
    assert_eq!(tg.texts().await.len(), 1, "{:?}", tg.texts().await);

    // Held deliveries are in the database: a restart keeps them.
    drop(daemon);
    let _daemon = env.daemon();
    ticks().await;
    assert_eq!(env.query("SELECT count(*) FROM held_deliveries"), 2);
    // Fast-forward to the end of the window: one combined message.
    env.db()
        .execute(
            "UPDATE held_deliveries SET release_at = ?1",
            [tern_store::now_ms() - 1000],
        )
        .unwrap();
    let combined = tg.wait_for("2 updates from quiet hours", 15).await;
    assert!(
        combined.contains("▸ Morning brief\nBrief: two meetings.")
            && combined.contains("▸ Follow-ups\nFollow-ups: reply to Sam."),
        "{combined}"
    );
    env.wait_count("SELECT count(*) FROM held_deliveries", 0, 5)
        .await;
    ticks().await;
    assert_eq!(tg.texts().await.len(), 2, "sent once");
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_per_day_pauses_the_schedule_with_exactly_one_notice() {
    let tg = FakeTelegram::start();
    let env = Env::new(&tg, "");
    env.script(json!({ "turns": [{ "text": "Checked the feed." }] }));
    let _daemon = env.daemon();
    let s = env
        .create(
            json!({ "cron": "* * * * *", "prompt": "Check the feed", "deliver_to": target(), "initiative": "interrupt",
            "quiet_hours": "none", "runs_per_day": 2 }),
        )
        .await;
    let id = s["id"].as_str().unwrap();
    let runs = "SELECT count(*) FROM runs WHERE trigger = 'cron' AND status = 'completed'";
    for n in 1..=2 {
        env.due(id);
        env.wait_count(runs, n, 15).await;
    }
    env.due(id);
    let notice = tg.wait_for("⏸ Paused", 15).await;
    assert!(
        notice.contains("“Check the feed” until tomorrow") && notice.contains("runs_per_day = 2"),
        "{notice}"
    );
    let paused = env.schedule(id).await;
    assert_eq!(paused["paused"], "budget");
    assert_eq!(paused["paused_reason"], "runs_per_day");
    assert_eq!(paused["enabled"], true);

    // Later slots today stay quiet: no run, no second notice.
    for _ in 0..2 {
        env.due(id);
        ticks().await;
    }
    assert_eq!(env.query(runs), 2);
    assert_eq!(tg.count("⏸ Paused").await, 1);

    // Resuming counts from now, so it runs again.
    let res = env
        .api()
        .patch(format!("http://tern/v1/schedules/{id}"))
        .json(&json!({ "resume": true }))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success());
    let resumed: Value = res.json().await.unwrap();
    assert_eq!(resumed["paused"], Value::Null);
    env.due(id);
    env.wait_count(runs, 3, 15).await;

    // Doctor reports a schedule paused by its budget.
    env.db()
        .execute(
            "UPDATE schedules SET paused_until = ?1, paused_reason = 'runs_per_day'",
            [tern_store::now_ms() + 3_600_000],
        )
        .unwrap();
    let out = env.cmd(&["doctor", "--offline"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("“Check the feed” is paused by its budget (runs_per_day)"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tokens_per_run_stops_a_run_as_failed() {
    let tg = FakeTelegram::start();
    let env = Env::new(&tg, "[schedules]\nquiet_hours = \"none\"\n");
    env.script(json!({ "turns": [
        { "text": "Looking.", "tool_calls": [{ "name": "schedule_list", "input": {} }] },
        { "text": "A long answer that should never be produced." }
    ]}));
    let _daemon = env.daemon();
    let s = env
        .create(
            json!({ "cron": "0 3 * * *", "prompt": "Dig through everything", "deliver_to": target(), "initiative": "interrupt",
            "tokens_per_run": 100 }),
        )
        .await;
    assert_eq!(s["tokens_per_run"], 100);
    env.due(s["id"].as_str().unwrap());
    let failed = tg.wait_for("Failed: stopped: this run used", 15).await;
    assert!(
        failed.contains("over its cap of 100 (tokens_per_run)"),
        "{failed}"
    );
    assert_eq!(
        env.query("SELECT count(*) FROM runs WHERE status = 'failed' AND budget_tokens = 100"),
        1
    );
    assert_eq!(tg.count("never be produced").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn usd_per_day_is_ignored_without_a_price_and_applies_with_one() {
    let tg = FakeTelegram::start();
    let env = Env::new(
        &tg,
        "[schedules]\nquiet_hours = \"none\"\nusd_per_day = 0.0001\n",
    );
    env.script(json!({ "turns": [{ "text": "Spent a little." }] }));
    let daemon = env.daemon();
    let s = env
        .create(json!({ "cron": "* * * * *", "prompt": "Spend", "deliver_to": target(), "initiative": "interrupt" }))
        .await;
    let id = s["id"].as_str().unwrap().to_owned();
    let runs = "SELECT count(*) FROM runs WHERE trigger = 'cron' AND status = 'completed'";
    for n in 1..=3 {
        env.due(&id);
        env.wait_count(runs, n, 15).await;
    }
    assert_eq!(
        tg.count("⏸ Paused").await,
        0,
        "no price: the cap is ignored"
    );
    let logs = env.logs();
    assert_eq!(
        logs.matches("usd_per_day is ignored").count(),
        1,
        "noted once:\n{logs}"
    );
    let out = env.cmd(&["doctor", "--offline"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("“Spend”: usd_per_day is ignored, no price is known for mock:mock"),
        "{text}"
    );

    // With a price, the same spend pauses it.
    drop(daemon);
    env.config(
        "[schedules]\nquiet_hours = \"none\"\nusd_per_day = 0.0001\n\n[providers.mock]\nkind = \"openai_compat\"\nbase_url = \"http://127.0.0.1:9\"\n\n[providers.mock.prices]\nmock = { input = 3.0, output = 15.0 }\n",
    );
    let _daemon = env.daemon();
    env.due(&id);
    let notice = tg.wait_for("⏸ Paused", 15).await;
    assert!(notice.contains("usd_per_day = $0.0001"), "{notice}");
    assert_eq!(env.query(runs), 3);
}
