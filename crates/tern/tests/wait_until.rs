//! `wait_until` end to end (docs/wait-until-design.md): an agent waits on a
//! check, the daemon polls it with no model call, and when the condition
//! matches (or the wait times out) one run continues the same session.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

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
}

impl Env {
    fn new(script: &Value, config: &str, ci: Option<&str>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(tern_home.join("connectors")).unwrap();
        std::fs::write(tern_home.join("config.toml"), config).unwrap();
        std::fs::write(tmp.path().join("script.json"), script.to_string()).unwrap();
        if let Some(url) = ci {
            // A read tool that answers whatever the CI server says.
            std::fs::write(
                tern_home.join("connectors/ci.luau"),
                format!(
                    r#"return {{ name = "ci", description = "a CI server",
  capabilities = {{ http = {{ "127.0.0.1" }} }},
  tools = {{ run = {{ description = "status of a run", effect = "read",
    params = {{ type = "object", properties = {{ id = {{ type = "string" }} }} }},
    run = function(ctx, args) return ctx.http.get("{url}/runs/" .. (args.id or "1")).body end }},
    deploy = {{ description = "starts a deploy", effect = "write", run = function() return "no" end }} }} }}"#
                ),
            )
            .unwrap();
        }
        Self {
            home,
            tern_home,
            _tmp: tmp,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(tern_bin());
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env(
                "TERN_PROVIDER",
                format!("mock:{}", self._tmp.path().join("script.json").display()),
            )
            .env("TERN_SEED_AGENTS", "0")
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

    async fn get(&self, path: &str) -> Value {
        let r = self
            .api()
            .get(format!("http://tern{path}"))
            .send()
            .await
            .unwrap();
        r.json().await.unwrap_or(Value::Null)
    }

    /// Start a run and wait for it to finish: its `{run_id, session_id}`.
    async fn say(&self, text: &str) -> Value {
        let r = self
            .api()
            .post("http://tern/v1/runs")
            .json(&json!({ "text": text }))
            .send()
            .await
            .unwrap();
        let started: Value = r.json().await.unwrap();
        self.get(&format!(
            "/v1/runs/{}/wait",
            started["run_id"].as_str().unwrap()
        ))
        .await;
        started
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.tern_home.join("tern.db")).unwrap()
    }

    fn query<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Make every wait due now.
    fn due(&self) {
        self.db()
            .execute(
                "UPDATE waits SET next_check_at = ?1",
                [tern_store::now_ms() - 1000],
            )
            .unwrap();
    }

    /// Make every wait past its timeout.
    fn expire(&self) {
        self.db()
            .execute(
                "UPDATE waits SET expires_at = ?1",
                [tern_store::now_ms() - 1000],
            )
            .unwrap();
    }

    async fn wait_for(&self, what: &str, secs: u64, mut ok: impl FnMut(&Self) -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        while !ok(self) {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

async fn ci_server(body: Value) -> MockServer {
    let server = MockServer::start().await;
    serve(&server, body).await;
    server
}

async fn serve(server: &MockServer, body: Value) {
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

fn watch_ci(every: &str) -> Value {
    json!({ "conversations": [{ "matches": "Watch the CI run", "turns": [
        { "text": "", "tool_calls": [{ "name": "wait_until", "input": {
            "note": "tell Mitch whether CI passed",
            "check": { "connector": "ci", "tool": "run", "args": { "id": "7" } },
            "until": { "path": "status", "in": ["completed", "failed"] },
            "every": every, "timeout": "2h" } }] },
        { "text": "Waiting for CI." },
        { "text": "CI passed." }
    ]}]})
}

#[tokio::test(flavor = "multi_thread")]
async fn polls_without_the_model_then_continues_the_same_session_across_a_restart() {
    let server = ci_server(json!({ "status": "queued" })).await;
    let env = Env::new(&watch_ci("30s"), "", Some(&server.uri()));
    let daemon = env.daemon();
    let first = env.say("Watch the CI run").await;
    let session = first["session_id"].as_str().unwrap().to_owned();
    assert_eq!(env.query::<i64>("SELECT count(*) FROM waits"), 1);
    // Visible: Tasks says what it waits for.
    let tasks = env.get("/v1/tasks").await;
    let w = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["kind"] == "wait")
        .unwrap_or_else(|| panic!("{tasks}"));
    assert_eq!(w["title"], "Waiting: tell Mitch whether CI passed");
    assert_eq!(w["section"], "scheduled");
    assert_eq!(w["session_id"], session.as_str());

    // Several polls while it's queued: the CI server is asked each time,
    // the model never is.
    let steps: i64 = env.query("SELECT count(*) FROM steps");
    for n in 1..=3 {
        env.due();
        env.wait_for("a poll", 15, |e| {
            e.query::<i64>("SELECT checks FROM waits") >= n
        })
        .await;
    }
    assert!(server.received_requests().await.unwrap().len() >= 3);
    assert_eq!(env.query::<i64>("SELECT count(*) FROM steps"), steps);
    assert_eq!(env.query::<i64>("SELECT count(*) FROM runs"), 1);
    let last: String = env.query("SELECT last_check FROM waits");
    assert_eq!(last, "status = \"queued\"");

    // Restart: the wait is still there. Then CI finishes.
    drop(daemon);
    let _daemon = env.daemon();
    assert_eq!(env.query::<i64>("SELECT count(*) FROM waits"), 1);
    serve(
        &server,
        json!({ "status": "completed", "conclusion": "success" }),
    )
    .await;
    env.due();
    env.wait_for("the continuation", 20, |e| {
        e.query::<i64>("SELECT count(*) FROM runs WHERE trigger = 'wait' AND status = 'completed'")
            == 1
    })
    .await;

    // One continuation, in the same session, with the note and the result
    // as untrusted data; the run is tainted for the same reason.
    assert_eq!(env.query::<i64>("SELECT count(*) FROM runs"), 2);
    let (run_session, taint, input): (String, String, String) = env
        .db()
        .query_row(
            "SELECT r.session_id, r.input_taint, m.text FROM runs r
             JOIN messages m ON m.id = r.input_message_id WHERE r.trigger = 'wait'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(run_session, session);
    assert_eq!(taint, "started by a wait on ci.run");
    assert!(
        input.contains("tell Mitch whether CI passed")
            && input.contains("<untrusted source=\"wait:ci.run\">")
            && input.contains("\"conclusion\":\"success\""),
        "{input}"
    );
    // It deleted itself, and the ledger has both ends.
    assert_eq!(env.query::<i64>("SELECT count(*) FROM waits"), 0);
    for kind in ["wait.create", "wait.fire"] {
        assert_eq!(
            env.query::<i64>(&format!(
                "SELECT count(*) FROM ledger WHERE kind = '{kind}'"
            )),
            1,
            "{kind}"
        );
    }
    // And it never fires twice.
    env.due();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(env.query::<i64>("SELECT count(*) FROM runs"), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_times_out_and_says_so() {
    let server = ci_server(json!({ "status": "queued" })).await;
    let env = Env::new(&watch_ci("30s"), "", Some(&server.uri()));
    let _daemon = env.daemon();
    let first = env.say("Watch the CI run").await;
    env.expire();
    env.wait_for("the continuation", 20, |e| {
        e.query::<i64>("SELECT count(*) FROM runs WHERE trigger = 'wait' AND status = 'completed'")
            == 1
    })
    .await;
    let (session, input): (String, String) = env
        .db()
        .query_row(
            "SELECT r.session_id, m.text FROM runs r
             JOIN messages m ON m.id = r.input_message_id WHERE r.trigger = 'wait'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(session, first["session_id"].as_str().unwrap());
    assert!(
        input.contains("timed out") && input.contains("queued"),
        "{input}"
    );
    assert_eq!(env.query::<i64>("SELECT count(*) FROM waits"), 0);
    let why: String =
        env.query("SELECT json_extract(detail, '$.why') FROM ledger WHERE kind = 'wait.fire'");
    assert_eq!(why, "timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cap_the_user_cancelling_and_time_waits() {
    let script = json!({ "conversations": [
        { "matches": "Make two waits", "turns": [
            { "text": "", "tool_calls": [
                { "name": "wait_until", "input": { "note": "first", "check": { "in": "10m" } } },
                { "name": "wait_until", "input": { "note": "second", "check": { "in": "10m" } } },
                { "name": "wait_until", "input": { "note": "bad", "check": { "connector": "ci", "tool": "deploy" }, "until": { "equals": 1 } } }
            ] },
            { "text": "Done." }
        ]},
        { "matches": "Check back", "turns": [
            { "text": "", "tool_calls": [
                { "name": "wait_until", "input": { "note": "look at the build", "check": { "in": "30s" } } }
            ] },
            { "text": "Back soon." },
            { "text": "Looked." }
        ]}
    ]});
    let env = Env::new(
        &script,
        "[schedules]\nmax_waits = 1\n",
        Some("http://127.0.0.1:9"),
    );
    let _daemon = env.daemon();
    let run = env.say("Make two waits").await;
    let outputs: Vec<String> = {
        let db = env.db();
        let mut stmt = db
            .prepare(
                "SELECT output_json FROM steps WHERE run_id = ?1 AND kind = 'tool' ORDER BY idx",
            )
            .unwrap();
        stmt.query_map([run["run_id"].as_str().unwrap()], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert!(outputs[0].contains("waiting"), "{outputs:?}");
    assert!(outputs[1].contains("already have 1"), "{outputs:?}");
    assert!(outputs[2].contains("isn't a read tool"), "{outputs:?}");
    assert_eq!(env.query::<i64>("SELECT count(*) FROM waits"), 1);

    // The user cancels it from the API: nothing continues.
    let list = env.get("/v1/waits").await;
    let id = list[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(list[0]["note"], "first");
    let del = env
        .api()
        .delete(format!("http://tern/v1/waits/{id}"))
        .send()
        .await
        .unwrap();
    assert!(del.status().is_success());
    assert_eq!(env.query::<i64>("SELECT count(*) FROM waits"), 0);
    let actor: String = env.query("SELECT actor FROM ledger WHERE kind = 'wait.cancel'");
    assert_eq!(actor, "owner");
    let gone = env
        .api()
        .delete(format!("http://tern/v1/waits/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), 404);

    // "Check back in 30s": a time wait fires at its time, untainted.
    let first = env.say("Check back on the build").await;
    assert_eq!(env.query::<i64>("SELECT count(*) FROM waits"), 1);
    env.expire();
    env.wait_for("the continuation", 20, |e| {
        e.query::<i64>("SELECT count(*) FROM runs WHERE trigger = 'wait' AND status = 'completed'")
            == 1
    })
    .await;
    let (session, taint, input): (String, Option<String>, String) = env
        .db()
        .query_row(
            "SELECT r.session_id, r.input_taint, m.text FROM runs r
             JOIN messages m ON m.id = r.input_message_id WHERE r.trigger = 'wait'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(session, first["session_id"].as_str().unwrap());
    assert_eq!(taint, None);
    assert!(input.contains("look at the build"), "{input}");
}
