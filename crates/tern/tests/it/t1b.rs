//! T1b end-to-end (docs/personal-agent-tooling-spec.md §3, §4.1, §12.2):
//! routines with watch triggers (cursors, dedup, taint, no model call when
//! nothing happened), gates, and initiative levels with digests, against
//! the daemon, a fake Telegram, and the mock provider.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]
#![cfg(unix)]

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
    fn new(tg: Option<&FakeTelegram>, extra_config: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(tern_home.join("logs")).unwrap();
        let env = Self {
            script: tmp.path().join("script.json"),
            home,
            tern_home,
            telegram_url: tg.map(|t| t.url.clone()).unwrap_or_default(),
            _tmp: tmp,
        };
        env.config(extra_config);
        env
    }

    fn config(&self, extra: &str) {
        let telegram = if self.telegram_url.is_empty() {
            String::new()
        } else {
            format!(
                "[gateway.telegram]\nenabled = true\nbot_token_secret = \"TELEGRAM_BOT_TOKEN\"\napi_url = \"{}\"\n\n",
                self.telegram_url
            )
        };
        std::fs::write(
            self.tern_home.join("config.toml"),
            format!("{telegram}{extra}"),
        )
        .unwrap();
    }

    /// Install an active connector.
    fn connector(&self, name: &str, source: &str) {
        let dir = self.tern_home.join("connectors");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.luau")), source).unwrap();
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

/// Let a few scheduler ticks pass.
async fn ticks() {
    tokio::time::sleep(Duration::from_millis(1200)).await;
}

/// A watch that never finds anything.
const QUIET: &str = r#"return { name = "quiet", description = "nothing happens",
  tools = { ping = { description = "ping", effect = "read", run = function() return "pong" end } },
  watches = { nothing = { description = "never anything", default_every = "1m",
    fn = function(ctx, cursor) return { events = {}, cursor = cursor } end } } }"#;

/// A watch that finds one more event each poll: e1, then e1 e2, then e1 e2
/// e3… Only the cursor says how far it got.
const COUNTER: &str = r#"return { name = "counter", description = "counts",
  tools = { ping = { description = "ping", effect = "read", run = function() return "pong" end } },
  watches = { items = { description = "one more item each time", default_every = "1m",
    fn = function(ctx, cursor)
      local n = (cursor and cursor.n or 0) + 1
      local events = {}
      for i = 1, n do table.insert(events, { id = "e" .. i, title = "item " .. i }) end
      return { events = events, cursor = { n = n } }
    end } } }"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_watch_with_no_events_makes_no_model_calls() {
    let env = Env::new(None, "");
    // Any model call would fail the run it belongs to.
    env.script(
        json!({ "turns": [{ "text": "should never be asked" }], "fail_models": { "mock": 500 } }),
    );
    env.connector("quiet", QUIET);
    let _daemon = env.daemon();
    let s = env
        .create(json!({ "watch": "quiet.nothing", "prompt": "Tell me what happened" }))
        .await;
    assert_eq!(
        (s["trigger"].as_str(), s["every_secs"].as_i64()),
        (Some("watch"), Some(60))
    );
    let id = s["id"].as_str().unwrap();
    // The first poll is due at once.
    let end = Instant::now() + Duration::from_secs(15);
    while env.schedule(id).await["last_check"].is_null() {
        assert!(Instant::now() < end, "never polled");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for _ in 0..3 {
        env.due(id);
        ticks().await;
    }
    let s = env.schedule(id).await;
    assert_eq!(s["last_check"], "no new events");
    assert_eq!(env.query("SELECT count(*) FROM runs"), 0);
    assert_eq!(env.query("SELECT count(*) FROM steps"), 0);
    assert_eq!(env.query("SELECT count(*) FROM routine_events"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_events_start_tainted_runs_once_each_across_restarts() {
    let env = Env::new(None, "");
    env.script(json!({ "turns": [{ "text": "Noted the new item." }] }));
    env.connector("counter", COUNTER);
    let daemon = env.daemon();
    let bad = env
        .api()
        .post("http://tern/v1/schedules")
        .json(&json!({ "watch": "counter.nope", "prompt": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let err = bad.text().await.unwrap();
    assert!(
        err.contains("has no watch") && err.contains("(it has items)"),
        "{err}"
    );
    let s = env
        .create(json!({ "watch": "counter.items", "prompt": "Summarize the new item" }))
        .await;
    let id = s["id"].as_str().unwrap().to_owned();
    let events = |state: &str| {
        format!(
            "SELECT count(*) FROM routine_events WHERE routine_id = '{id}' AND state = '{state}'"
        )
    };
    // First poll: a baseline. e1 is recorded and doesn't run.
    env.wait_count(&events("baseline"), 1, 15).await;
    ticks().await;
    assert_eq!(env.query("SELECT count(*) FROM runs"), 0);
    assert!(
        env.schedule(&id).await["last_check"]
            .as_str()
            .unwrap()
            .starts_with("first poll: 1 existing")
    );

    // Second poll: e1 again (deduplicated) and e2, which runs.
    env.due(&id);
    env.wait_count(&events("started"), 1, 15).await;
    let runs = "SELECT count(*) FROM runs WHERE trigger = 'cron' AND status = 'completed'";
    env.wait_count(runs, 1, 15).await;
    let (taint, input): (String, String) = env
        .db()
        .query_row(
            "SELECT r.input_taint, m.text FROM runs r JOIN messages m ON m.id = r.input_message_id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(taint, "started by watch counter.items");
    assert!(
        input.starts_with("Summarize the new item")
            && input.contains("<untrusted source=\"watch:counter.items\">")
            && input.contains("\"id\":\"e2\"")
            && !input.contains("\"e1\""),
        "{input}"
    );

    // The cursor survives a restart: the next poll finds only e3.
    drop(daemon);
    let _daemon = env.daemon();
    env.due(&id);
    env.wait_count(&events("started"), 2, 15).await;
    env.wait_count(runs, 2, 15).await;
    ticks().await;
    assert_eq!(env.query(&events("baseline")), 1);
    assert_eq!(
        env.query("SELECT count(*) FROM runs"),
        2,
        "each event ran once"
    );
    assert_eq!(
        env.query("SELECT count(*) FROM runs WHERE input_taint IS NOT NULL"),
        2
    );
    let cursor: String = env
        .db()
        .query_row("SELECT watch_cursor FROM schedules", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cursor, r#"{"n":3}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gate_skips_runs_without_a_model_call() {
    let env = Env::new(None, "");
    env.script(json!({ "turns": [{ "text": "Brief: all quiet." }] }));
    env.connector("quiet", QUIET);
    let _daemon = env.daemon();
    let bad = env
        .api()
        .post("http://tern/v1/schedules")
        .json(&json!({ "cron": "0 3 * * *", "prompt": "x", "gate": "true and" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400, "a gate that doesn't compile is refused");
    let s = env
        .create(json!({ "cron": "0 3 * * *", "prompt": "Morning brief",
            "gate": "#connector.tools.ping.run(ctx, {}) == 99", "gate_connector": "quiet" }))
        .await;
    assert_eq!(s["gate_connector"], "quiet");
    let id = s["id"].as_str().unwrap();
    env.due(id);
    let end = Instant::now() + Duration::from_secs(15);
    while env.schedule(id).await["last_check"] != "gate: skipped" {
        assert!(Instant::now() < end, "gate never ran");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ticks().await;
    assert_eq!(env.query("SELECT count(*) FROM runs"), 0);
    let s = env.schedule(id).await;
    assert!(
        s["next_run_at"].as_i64().unwrap() > tern_store::now_ms(),
        "moved on"
    );

    // The same routine with a gate that says yes runs.
    let res = env
        .api()
        .patch(format!("http://tern/v1/schedules/{id}"))
        .json(&json!({ "gate": "connector.tools.ping.run(ctx, {}) == \"pong\"" }))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "{}", res.text().await.unwrap());
    env.due(id);
    env.wait_count(
        "SELECT count(*) FROM runs WHERE status = 'completed'",
        1,
        15,
    )
    .await;
    assert_eq!(env.schedule(id).await["last_check"], "gate: passed");
}

#[tokio::test(flavor = "multi_thread")]
async fn initiative_levels_decide_the_push_and_the_dial_quiets_them() {
    let tg = FakeTelegram::start();
    let env = Env::new(Some(&tg), "[schedules]\nquiet_hours = \"none\"\n");
    env.script(json!({ "conversations": [
        // The gate model's question names the routine's prompt.
        { "matches": "The task:\nCheck the build", "turns": [{ "text": "{\"interrupt\": true, \"headline\": \"Build is red\"}" }] },
        { "matches": "The task:\nCheck the news", "turns": [{ "text": "{\"interrupt\": false, \"headline\": \"Nothing big\"}" }] },
        { "matches": "Silent check", "turns": [{ "text": "Silent result." }] },
        { "matches": "Digest one", "turns": [{ "text": "Digest result one." }] },
        { "matches": "Check the build", "turns": [{ "text": "The build failed on main." }] },
        { "matches": "Check the news", "turns": [{ "text": "Some news." }] },
        { "matches": "Ping me", "turns": [{ "text": "Ping result." }] }
    ]}));
    let daemon = env.daemon();
    let mk = |prompt: &str, initiative: Option<&str>| {
        let mut b = json!({ "cron": "0 3 * * *", "prompt": prompt, "deliver_to": target() });
        if let Some(i) = initiative {
            b["initiative"] = i.into();
        }
        b
    };
    let bad = env
        .api()
        .post("http://tern/v1/schedules")
        .json(&mk("x", Some("loud")))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let silent = env.create(mk("Silent check", Some("silent"))).await;
    let digest = env.create(mk("Digest one", None)).await;
    assert!(digest["initiative"].is_null(), "the default: digest");
    let build = env.create(mk("Check the build", Some("notify"))).await;
    let news = env.create(mk("Check the news", Some("notify"))).await;
    let ping = env.create(mk("Ping me", Some("interrupt"))).await;
    let done = "SELECT count(*) FROM runs WHERE trigger = 'cron' AND status = 'completed'";
    for (n, s) in [&silent, &digest, &build, &news, &ping]
        .into_iter()
        .enumerate()
    {
        env.due(s["id"].as_str().unwrap());
        env.wait_count(done, n as i64 + 1, 15).await;
    }
    // interrupt: at once. notify + yes: at once, with the headline.
    tg.wait_for("Ping result.", 15).await;
    let red = tg.wait_for("The build failed on main.", 15).await;
    assert!(red.starts_with("Build is red\n\n"), "{red}");
    // digest, and notify + no: held for the next digest time.
    env.wait_count(
        "SELECT count(*) FROM held_deliveries WHERE kind = 'digest'",
        2,
        15,
    )
    .await;
    ticks().await;
    assert_eq!(tg.texts().await.len(), 2, "{:?}", tg.texts().await);
    // silent: posted to its channel, never pushed.
    assert_eq!(
        env.query("SELECT count(*) FROM posts WHERE text LIKE '%Silent result.%'"),
        1
    );
    let release = env.query("SELECT min(release_at) FROM held_deliveries");
    assert!(release > tern_store::now_ms(), "held until a digest time");
    // Digest time: one message with both.
    env.db()
        .execute(
            "UPDATE held_deliveries SET release_at = ?1",
            [tern_store::now_ms() - 1000],
        )
        .unwrap();
    let digest_msg = tg.wait_for("Digest · 2 updates", 15).await;
    assert!(
        digest_msg.contains("▸ Digest one\nDigest result one.")
            && digest_msg.contains("▸ Check the news\nSome news."),
        "{digest_msg}"
    );
    assert_eq!(tg.count("Silent result.").await, 0);

    // Vacation: even an interrupt routine only posts.
    let res = env
        .api()
        .put("http://tern/v1/initiative")
        .json(&json!({ "dial": "vacation" }))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "{}", res.text().await.unwrap());
    let status: Value = env
        .api()
        .get("http://tern/v1/status")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["initiative"], "vacation");
    let config = std::fs::read_to_string(env.tern_home.join("config.toml")).unwrap();
    assert!(
        config.contains("initiative_dial = \"vacation\""),
        "{config}"
    );
    env.due(ping["id"].as_str().unwrap());
    env.wait_count(done, 6, 15).await;
    ticks().await;
    assert_eq!(tg.count("Ping result.").await, 1, "not pushed on vacation");
    drop(daemon);
}
