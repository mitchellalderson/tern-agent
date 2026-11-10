//! S1 end-to-end (docs/subagents-design.md §7): sub-agents spawned and
//! awaited by a parent run, with the mock provider driving parent and
//! children from one script, through crashes, resume and cancel.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The binary under test: `TERN_TEST_BIN` (e.g. a `--profile dist` build,
/// whose settings can differ from test builds) or this crate's build.
fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

const FINAL: &str = "All three reported: done.";

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
    fn new(script: &Value) -> Self {
        Self::with_config(script, "[subagents]\nenabled = true\n")
    }

    fn with_config(script: &Value, config: &str) -> Self {
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

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(tern_bin());
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env("TERN_PROVIDER", format!("mock:{}", self.script.display()))
            .stdin(Stdio::null());
        c
    }

    fn socket(&self) -> PathBuf {
        self.tern_home.join("tern.sock")
    }

    fn daemon_with(&self, crash_after: Option<u32>) -> Killed {
        let _ = std::fs::remove_file(self.socket());
        let mut cmd = self.cmd(&["daemon"]);
        if let Some(n) = crash_after {
            cmd.env("TERN_CRASH_AFTER_STEP", n.to_string());
        }
        let child = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        // The socket file appears at bind(), a moment before listen(), so
        // "exists" can still mean "connection refused". Wait for a connect.
        while std::os::unix::net::UnixStream::connect(self.socket()).is_err() {
            assert!(Instant::now() < end, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Killed(child)
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.tern_home.join("tern.db")).unwrap()
    }

    fn query<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// (label, status, error) of every sub-agent, by label.
    fn children(&self) -> Vec<(String, String, Option<String>)> {
        let db = self.db();
        let mut stmt = db
            .prepare(
                "SELECT label, status, error FROM runs WHERE parent_run_id IS NOT NULL ORDER BY label",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn parent_status(&self) -> String {
        self.query("SELECT status FROM runs WHERE parent_run_id IS NULL")
    }

    fn wait_for(&self, what: &str, secs: u64, mut ok: impl FnMut(&Self) -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        while !ok(self) {
            assert!(
                Instant::now() < end,
                "timed out waiting for {what}; children: {:?}",
                self.children()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The parent's final answer and every tool result it saw.
    fn parent_transcript(&self) -> (String, String) {
        let db = self.db();
        let mut stmt = db
            .prepare(
                "SELECT m.role, m.text FROM messages m JOIN runs r ON r.id = m.run_id
                 WHERE r.parent_run_id IS NULL ORDER BY m.id",
            )
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let last = rows
            .iter()
            .rev()
            .find(|(role, _)| role == "assistant")
            .map(|(_, t)| t.clone())
            .unwrap_or_default();
        let all = rows
            .into_iter()
            .map(|(_, t)| t)
            .collect::<Vec<_>>()
            .join("\n");
        (last, all)
    }

    /// Wait for the daemon to abort itself (TERN_CRASH_AFTER_STEP).
    fn wait_abort(&self, d: &mut Killed) {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            if d.0.try_wait().unwrap().is_some() {
                return;
            }
            assert!(Instant::now() < end, "daemon did not abort");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn api(socket: &Path) -> reqwest::Client {
    reqwest::Client::builder()
        .unix_socket(socket)
        .build()
        .unwrap()
}

async fn start(socket: &Path, text: &str) -> Value {
    api(socket)
        .post("http://tern/v1/runs")
        .json(&json!({ "text": text }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// A sub-agent: three directory listings, then its report. Step indexes:
/// 0 llm, 1 tool, 2 llm, 3 tool, 4 llm, 5 tool, 6 llm. The first model
/// call is slow, so the parent's early steps always come first.
fn child_turns(report: &str, first_delay_ms: u64) -> Value {
    let list = json!({ "tool_calls": [{ "name": "list_dir", "input": { "path": "." } }] });
    let mut first = list.clone();
    first["delay_ms"] = first_delay_ms.into();
    json!([first, list, list, { "text": report }])
}

/// The parent spawns a, b and c and waits for all of them in one turn:
/// 0 llm, 1-3 agent_spawn, 4 agent_wait, 5 llm (final answer).
fn fan_out_script(first_delay_ms: u64) -> Value {
    let spawn = |l: &str| {
        json!({ "name": "agent_spawn", "input": {
            "label": l,
            "task": format!("SUBTASK-{l}: list the workspace and report"),
            "tools": "read_only"
        }})
    };
    json!({
        "turns": [
            { "text": "Fanning out.", "tool_calls": [spawn("a"), spawn("b"), spawn("c"),
                { "name": "agent_wait", "input": {} }] },
            { "text": FINAL }
        ],
        "conversations": [
            { "when_task_contains": "SUBTASK-a", "turns": child_turns("report from a", first_delay_ms) },
            { "when_task_contains": "SUBTASK-b", "turns": child_turns("report from b", first_delay_ms) },
            { "when_task_contains": "SUBTASK-c", "turns": child_turns("report from c", first_delay_ms) }
        ]
    })
}

fn assert_fan_out_finished(env: &Env) {
    env.wait_for("the parent to complete", 30, |e| {
        e.parent_status() == "completed"
    });
    let kids = env.children();
    assert_eq!(
        kids.iter()
            .map(|(l, s, _)| (l.as_str(), s.as_str()))
            .collect::<Vec<_>>(),
        [("a", "completed"), ("b", "completed"), ("c", "completed")],
        "exactly one child per spawn call"
    );
    let (last, all) = env.parent_transcript();
    assert_eq!(last, FINAL);
    for r in ["report from a", "report from b", "report from c"] {
        assert!(
            all.contains(r),
            "{r} missing from the parent's context:\n{all}"
        );
    }
    let sessions: i64 = env.query("SELECT count(*) FROM sessions WHERE source = 'subagent'");
    assert_eq!(sessions, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn fan_out_of_three_reports_back_to_the_parent() {
    let env = Env::new(&fan_out_script(0));
    let _daemon = env.daemon_with(None);
    let http = api(&env.socket());
    let mut events = http.get("http://tern/v1/events").send().await.unwrap();
    let started = start(&env.socket(), "research three things").await;
    let parent = started["run_id"].as_str().unwrap().to_owned();

    // Events: the parent announces each child; each child's run_started
    // names its parent.
    let mut buf = String::new();
    let end = Instant::now() + Duration::from_secs(90);
    let evs = loop {
        let chunk = tokio::time::timeout(Duration::from_secs(60), events.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        let evs: Vec<Value> = buf
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|l| serde_json::from_str(l.trim()).ok())
            .collect();
        let finished = evs
            .iter()
            .any(|e| e["type"] == "run_finished" && e["run_id"] == parent.as_str());
        if finished {
            break evs;
        }
        assert!(Instant::now() < end, "parent never finished");
    };
    let spawned: Vec<&Value> = evs
        .iter()
        .filter(|e| e["type"] == "subagent_spawned")
        .collect();
    let child_starts: Vec<&Value> = evs
        .iter()
        .filter(|e| e["type"] == "run_started" && e["run_id"] != parent.as_str())
        .collect();
    assert_eq!(spawned.len(), 3, "{spawned:#?}");
    for (ev, label) in spawned.iter().zip(["a", "b", "c"]) {
        assert_eq!(ev["run_id"], parent.as_str());
        assert_eq!(ev["label"], label);
        assert_eq!(ev["status"], "running");
        assert_eq!(ev["model"], "mock:mock");
        assert!(ev["call_id"].as_str().unwrap().starts_with("mock_"));
        assert!(ev["task"].as_str().unwrap().starts_with("SUBTASK-"));
        assert!(ev["child_session_id"].is_string() && ev["child_run_id"].is_string());
    }
    assert_eq!(child_starts.len(), 3);
    // Each child is announced before its own events start.
    let pos = |f: &dyn Fn(&Value) -> bool| evs.iter().position(f).unwrap();
    for ev in &spawned {
        let child = ev["child_run_id"].clone();
        let announced = pos(&|e| e["type"] == "subagent_spawned" && e["child_run_id"] == child);
        let started = pos(&|e| e["type"] == "run_started" && e["run_id"] == child);
        assert!(announced < started, "{evs:#?}");
    }
    assert!(
        child_starts
            .iter()
            .all(|e| e["parent_run_id"] == parent.as_str())
    );
    // A top-level run_started has no parent_run_id key at all.
    let top = evs
        .iter()
        .find(|e| e["type"] == "run_started" && e["run_id"] == parent.as_str())
        .unwrap();
    assert!(top.get("parent_run_id").is_none(), "{top}");

    assert_fan_out_finished(&env);
    // Children are read-only and can't spawn (max_depth 1), but can ask.
    let scope: String = env.query("SELECT tool_scope FROM runs WHERE label = 'a'");
    let scope: Vec<String> = serde_json::from_str(&scope).unwrap();
    assert!(scope.contains(&"list_dir".to_owned()));
    assert!(scope.contains(&"agent_ask_parent".to_owned()));
    assert!(
        !scope
            .iter()
            .any(|t| (t.starts_with("agent_") && t != "agent_ask_parent") || t == "shell")
    );

    // `runs show` lists the children under their spawn steps.
    let out = env.cmd(&["runs", "show", &parent]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for l in ["└ a ", "└ b ", "└ c "] {
        assert!(text.contains(l), "{text}");
    }
    let list = env.cmd(&["runs", "list"]).output().unwrap();
    let list = String::from_utf8_lossy(&list.stdout);
    assert_eq!(list.lines().count(), 1, "{list}");
    assert!(list.contains("(+3 sub-agents)"), "{list}");
}

/// Crash at step `n`, restart, and check the result is what an uncrashed
/// run gives: one child per spawn call, the same final answer.
async fn crash_then_restart(n: u32, check: impl FnOnce(&Env)) {
    let env = Env::new(&fan_out_script(500));
    let mut daemon = env.daemon_with(Some(n));
    start(&env.socket(), "research three things").await;
    env.wait_abort(&mut daemon);
    drop(daemon);
    check(&env);
    let _daemon = env.daemon_with(None);
    assert_fan_out_finished(&env);
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_during_spawn_then_restart_spawns_no_duplicates() {
    crash_then_restart(2, |env| {
        // The parent died starting its second agent_spawn.
        let (kind, status): (String, String) = env
            .db()
            .query_row(
                "SELECT s.kind, s.status FROM steps s JOIN runs r ON r.id = s.run_id
                 WHERE r.parent_run_id IS NULL AND s.idx = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((kind.as_str(), status.as_str()), ("tool", "started"));
        assert_eq!(env.children().len(), 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_during_wait_then_restart_finishes_the_same() {
    crash_then_restart(4, |env| {
        let input: String = env.query(
            "SELECT s.input_json FROM steps s JOIN runs r ON r.id = s.run_id
             WHERE r.parent_run_id IS NULL AND s.idx = 4 AND s.status = 'started'",
        );
        assert!(input.contains("agent_wait"), "{input}");
        assert_eq!(env.children().len(), 3);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_in_a_child_then_restart_finishes_the_same() {
    crash_then_restart(5, |env| {
        // Only children get to step 5 before the parent's wait returns.
        let in_child: i64 = env.query(
            "SELECT count(*) FROM steps s JOIN runs r ON r.id = s.run_id
             WHERE r.parent_run_id IS NOT NULL AND s.idx = 5 AND s.status = 'started'",
        );
        // (Siblings racing to the same step may both record it before
        // the abort lands.)
        assert!(in_child >= 1);
        let in_parent: i64 = env.query(
            "SELECT count(*) FROM steps s JOIN runs r ON r.id = s.run_id
             WHERE r.parent_run_id IS NULL AND s.idx = 5",
        );
        assert_eq!(in_parent, 0);
        assert_eq!(env.parent_status(), "running");
    })
    .await;
}

/// `tern run` dies while the parent waits; `tern resume` of the parent
/// resumes its children (nobody else drives them in the CLI).
#[test]
fn resuming_a_parent_in_the_cli_resumes_its_children() {
    let env = Env::new(&fan_out_script(500));
    let o = env
        .cmd(&["run", "--yes", "research three things"])
        .env("TERN_CRASH_AFTER_STEP", "4")
        .output()
        .unwrap();
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    let run = stderr
        .lines()
        .find_map(|l| l.strip_prefix("run "))
        .and_then(|l| l.split_whitespace().next())
        .expect("run id on stderr")
        .to_owned();
    assert_eq!(env.children().len(), 3);
    assert!(
        env.children().iter().all(|(_, s, _)| s == "running"),
        "{:?}",
        env.children()
    );

    let o = env.cmd(&["resume", "--yes", &run]).output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        o.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(stdout.trim(), FINAL);
    assert_fan_out_finished(&env);
}

/// Ending a turn while a child runs doesn't finish the run: an inbox step
/// waits for the report and the model gets another turn with it.
#[test]
fn a_parent_cannot_finish_while_its_children_run() {
    let script = json!({
        "turns": [
            { "tool_calls": [{ "name": "agent_spawn", "input": {
                "label": "helper", "task": "SUBTASK-helper: report" } }] },
            { "text": "Started a helper." },
            { "text": "The helper says: it worked." }
        ],
        "conversations": [
            { "when_task_contains": "SUBTASK-helper", "turns": [{ "text": "it worked", "delay_ms": 1000 }] }
        ]
    });
    let env = Env::new(&script);
    let o = env.cmd(&["run", "--yes", "delegate it"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(stdout.trim(), "The helper says: it worked.");
    let kinds: String = env.query(
        "SELECT group_concat(kind, ',') FROM (SELECT s.kind FROM steps s JOIN runs r ON r.id = s.run_id
         WHERE r.parent_run_id IS NULL ORDER BY s.idx)",
    );
    assert_eq!(kinds, "llm,tool,llm,inbox,llm");
    let (_, all) = env.parent_transcript();
    assert!(
        all.contains("[sub-agent helper completed]\nit worked"),
        "{all}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_the_parent_cancels_its_children() {
    let spawn = |l: &str| json!({ "name": "agent_spawn", "input": { "label": l, "task": format!("SUBTASK-{l}") } });
    let slow = json!([{ "text": "never", "delay_ms": 60000 }]);
    let script = json!({
        "turns": [{ "tool_calls": [spawn("x"), spawn("y"), { "name": "agent_wait", "input": {} }] }],
        "conversations": [
            { "when_task_contains": "SUBTASK-x", "turns": slow },
            { "when_task_contains": "SUBTASK-y", "turns": slow }
        ]
    });
    let env = Env::new(&script);
    let _daemon = env.daemon_with(None);
    let started = start(&env.socket(), "start two and wait").await;
    let parent = started["run_id"].as_str().unwrap();
    env.wait_for("two running children", 10, |e| {
        e.children()
            .iter()
            .filter(|(_, s, _)| s == "running")
            .count()
            == 2
    });
    let active: Value = api(&env.socket())
        .get("http://tern/v1/runs/active")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(active.as_array().unwrap().len(), 3, "{active}");

    let r: Value = api(&env.socket())
        .post(format!("http://tern/v1/runs/{parent}/cancel"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["cancelled"], true);
    env.wait_for("everything to stop", 10, |e| {
        e.parent_status() == "interrupted"
            && e.children().iter().all(|(_, s, _)| s == "interrupted")
    });
    for (label, _, error) in env.children() {
        assert_eq!(
            error.as_deref(),
            Some("cancelled: the parent run ended"),
            "{label}"
        );
    }
    // Each child still reported (as cancelled) to its parent. The report is
    // posted when the child's drive ends, after its status is recorded as
    // interrupted, so wait for it rather than reading it straight away.
    env.wait_for("both cancelled children to report", 10, |e| {
        e.query::<i64>("SELECT count(*) FROM agent_mail WHERE kind = 'finished'") == 2
    });
    let reports: i64 = env.query("SELECT count(*) FROM agent_mail WHERE kind = 'finished'");
    assert_eq!(reports, 2);
}

#[test]
fn doctor_accepts_the_subagents_table_and_checks_its_models() {
    let ok = Env::with_config(
        &json!({}),
        "[subagents]\nenabled = true\nmax_concurrent = 2\nmodels = {}\n",
    );
    let o = ok.cmd(&["doctor", "--offline"]).output().unwrap();
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{text}");
    assert!(
        text.contains("enabled: depth 1, 2 at once per run"),
        "{text}"
    );

    let bad = Env::with_config(
        &json!({}),
        "[subagents]\nenabled = true\nmodels = { fast = \"nowhere:tiny\" }\n",
    );
    let o = bad.cmd(&["doctor", "--offline"]).output().unwrap();
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success(), "{text}");
    assert!(text.contains("subagents.models.fast"), "{text}");
}

/// Beyond `max_concurrent`, spawns succeed as `queued` and start as
/// siblings finish.
#[test]
fn spawns_beyond_the_concurrency_limit_queue_and_still_finish() {
    let env = Env::with_config(
        // Slow children: "queued" is only deterministic if the first child
        // is still running when the parent spawns the third.
        &fan_out_script(600),
        "[subagents]\nenabled = true\nmax_concurrent = 1\n",
    );
    let o = env
        .cmd(&["run", "--yes", "research three things"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), FINAL);
    assert_fan_out_finished(&env);
    let (_, all) = env.parent_transcript();
    assert_eq!(all.matches("\"status\":\"queued\"").count(), 2, "{all}");
}

/// agent_status lists children without waiting; agent_cancel stops one,
/// and the next agent_wait reports it as cancelled with the reason.
#[test]
fn status_and_cancel_tools() {
    let script = json!({
        "turns": [
            { "tool_calls": [
                { "name": "agent_spawn", "input": { "label": "slow", "task": "SUBTASK-slow" } },
                { "name": "agent_status", "input": {} },
                { "name": "agent_cancel", "input": { "agent": "slow", "reason": "not needed" } },
                { "name": "agent_wait", "input": { "agents": ["slow"] } }
            ] },
            { "text": "Cancelled it." }
        ],
        "conversations": [
            { "when_task_contains": "SUBTASK-slow", "turns": [{ "text": "late", "delay_ms": 60000 }] }
        ]
    });
    let env = Env::new(&script);
    let o = env
        .cmd(&["run", "--yes", "start and stop"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "Cancelled it.");
    let (_, all) = env.parent_transcript();
    assert!(all.contains("\"agent\":\"slow\""), "{all}");
    assert!(
        all.contains("\"error\":\"cancelled by the parent agent: not needed\""),
        "{all}"
    );
    assert!(all.contains("\"status\":\"cancelled\""), "{all}");
    let kids = env.children();
    assert_eq!(kids[0].1, "interrupted");
}

// ---- context isolation: what the parent's model calls actually carry ----

/// One line of the mock provider's request log (`TERN_MOCK_REQUEST_LOG`).
struct Req {
    first_user: String,
    messages: u64,
    messages_bytes: u64,
}

fn request_log(path: &Path) -> Vec<Req> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            Req {
                first_user: v["first_user"].as_str().unwrap().to_owned(),
                messages: v["messages"].as_u64().unwrap(),
                messages_bytes: v["messages_bytes"].as_u64().unwrap(),
            }
        })
        .collect()
}

/// Length of the noisy child's report: 4,998 x "finding " + "TAIL-MARKER".
const NOISY_REPORT_LEN: usize = 40_000;
const NOISY_TOTAL: usize = NOISY_REPORT_LEN - 16 + 11;

/// A noisy child: three 50 KB file reads, then a 40 KB report that ends in
/// a marker only the tail of the report carries.
fn noisy_script() -> Value {
    let read = |f: &str| json!({ "tool_calls": [{ "name": "read_file", "input": { "path": f } }] });
    let mut report = "finding ".repeat(NOISY_REPORT_LEN / 8 - 2);
    report.push_str("TAIL-MARKER");
    json!({
        "turns": [
            { "text": "Delegating.", "tool_calls": [
                { "name": "agent_spawn", "input": {
                    "label": "noisy", "task": "SUBTASK-noisy: read the big files", "tools": "read_only" } },
                { "name": "agent_wait", "input": {} }
            ] },
            { "text": "Done with the noisy child." }
        ],
        "conversations": [
            { "when_task_contains": "SUBTASK-noisy", "turns": [
                read("big1.txt"), read("big2.txt"), read("big3.txt"), { "text": report }
            ] }
        ]
    })
}

struct Noisy {
    env: Env,
    log: PathBuf,
}

impl Noisy {
    fn run(config: &str, script: &Value) -> Self {
        let env = Env::with_config(script, config);
        let ws = env.home.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        for f in ["big1.txt", "big2.txt", "big3.txt"] {
            std::fs::write(ws.join(f), format!("{f} ").repeat(50_000 / (f.len() + 1))).unwrap();
        }
        std::fs::write(
            env.tern_home.join("config.toml"),
            format!("workspace = {:?}\n{config}", ws.display().to_string()),
        )
        .unwrap();
        let log = env.home.join("requests.jsonl");
        let o = env
            .cmd(&["run", "--yes", "PARENT-TASK: investigate"])
            .env("TERN_MOCK_REQUEST_LOG", &log)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        Self { env, log }
    }

    fn parent_requests(&self) -> Vec<Req> {
        request_log(&self.log)
            .into_iter()
            .filter(|r| r.first_user.starts_with("PARENT-TASK"))
            .collect()
    }

    fn child_requests(&self) -> Vec<Req> {
        request_log(&self.log)
            .into_iter()
            .filter(|r| r.first_user.starts_with("SUBTASK-noisy"))
            .collect()
    }
}

/// The parent's last model call, as a noisy child leaves it.
fn parent_last(n: &Noisy) -> Req {
    n.parent_requests().pop().unwrap()
}

/// The parent's context holds the bounded report, never the child's work.
#[test]
fn a_noisy_child_does_not_fill_the_parents_context() {
    let n = Noisy::run("[subagents]\nenabled = true\n", &noisy_script());
    let parent = n.parent_requests();
    let child = n.child_requests();
    // Two parent calls (spawn+wait, then the answer); four child calls.
    assert_eq!((parent.len(), child.len()), (2, 4));
    let last = parent_last(&n);
    eprintln!(
        "parent final call: {} messages, {} bytes; child final call: {} messages, {} bytes",
        last.messages, last.messages_bytes, child[3].messages, child[3].messages_bytes
    );
    // 8,000 characters of report + the marker + JSON framing; nothing of the
    // three 50 KB reads or the 40 KB report's tail.
    assert!(last.messages_bytes < 10_000, "{}", last.messages_bytes);
    // The child's own context did grow: that is where the noise lives.
    assert!(
        child[3].messages_bytes > 90_000,
        "{}",
        child[3].messages_bytes
    );
    let (_, all) = n.env.parent_transcript();
    assert!(all.contains(&format!(
        "report truncated: showing the first 8000 of {NOISY_TOTAL} characters"
    )));
    assert!(all.contains("agent_result"));
    assert!(!all.contains("TAIL-MARKER"), "the cut tail stays out");
    assert!(!all.contains("big1.txt big1.txt"), "tool output stays out");
}

/// What the parent doesn't get is still there: the child's session holds its
/// whole transcript, and its steps are journaled.
#[test]
fn the_childs_full_work_stays_recorded() {
    let n = Noisy::run("[subagents]\nenabled = true\n", &noisy_script());
    let env = &n.env;
    let child_run: String = env.query("SELECT id FROM runs WHERE label = 'noisy'");
    let session: String = env.query("SELECT session_id FROM runs WHERE label = 'noisy'");
    let tool_steps: i64 = env.query(&format!(
        "SELECT count(*) FROM steps WHERE run_id = '{child_run}' AND kind = 'tool' AND status = 'completed'"
    ));
    assert_eq!(tool_steps, 3);
    let llm_steps: i64 = env.query(&format!(
        "SELECT count(*) FROM steps WHERE run_id = '{child_run}' AND kind = 'llm'"
    ));
    assert_eq!(llm_steps, 4);
    let session_bytes: i64 = env.query(&format!(
        "SELECT sum(length(content_json)) FROM messages WHERE session_id = '{session}'"
    ));
    assert!(session_bytes > 90_000, "{session_bytes}");
    let tail: i64 = env.query(&format!(
        "SELECT count(*) FROM messages WHERE session_id = '{session}' AND text LIKE '%TAIL-MARKER%'"
    ));
    assert_eq!(tail, 1, "the whole report is in the child's session");
    // Nothing of the child's working messages is in the parent's session.
    let parent_session: String =
        env.query("SELECT session_id FROM runs WHERE parent_run_id IS NULL");
    assert_ne!(parent_session, session);
    let leaked: i64 = env.query(&format!(
        "SELECT count(*) FROM messages WHERE session_id = '{parent_session}' AND text LIKE '%big1.txt%'"
    ));
    assert_eq!(leaked, 0);
}

/// The marker's pointer works: `agent_result` returns the rest, a slice at a
/// time, and only what the parent asks for enters its context.
#[test]
fn the_rest_of_a_cut_report_is_fetched_on_demand() {
    let mut script = noisy_script();
    // After the wait, the parent reads the last 8,000 characters.
    script["turns"] = json!([
        { "text": "Delegating.", "tool_calls": [
            { "name": "agent_spawn", "input": {
                "label": "noisy", "task": "SUBTASK-noisy: read the big files", "tools": "read_only" } },
            { "name": "agent_wait", "input": {} }
        ] },
        { "tool_calls": [
            { "name": "agent_result", "input": { "agent": "noisy", "offset": 32_000 } }
        ] },
        { "text": "Read the tail." }
    ]);
    let n = Noisy::run("[subagents]\nenabled = true\n", &script);
    let (last, all) = n.env.parent_transcript();
    assert_eq!(last, "Read the tail.");
    assert!(
        all.contains("TAIL-MARKER"),
        "the fetched slice has the tail"
    );
    assert!(
        all.contains(&format!("\"total_chars\":{NOISY_TOTAL}")),
        "{}",
        &all[all.len() - 400..]
    );
    assert!(all.contains("\"complete\":true"), "{all}");
    // Cut once, fetched once: the parent holds 8,000 + 8,000 characters.
    let l = parent_last(&n);
    assert!(l.messages_bytes < 22_000, "{}", l.messages_bytes);
}

/// `max_result_chars` is the knob.
#[test]
fn max_result_chars_sets_the_bound() {
    let n = Noisy::run(
        "[subagents]\nenabled = true\nmax_result_chars = 1000\n",
        &noisy_script(),
    );
    let (_, all) = n.env.parent_transcript();
    assert!(
        all.contains(&format!(
            "showing the first 1000 of {NOISY_TOTAL} characters"
        )),
        "{}",
        &all[all.len().saturating_sub(600)..]
    );
    assert!(parent_last(&n).messages_bytes < 3_000);
}

/// A short report is untouched: no marker, no extra field.
#[test]
fn a_short_report_arrives_whole() {
    let env = Env::new(&fan_out_script(0));
    let o = env
        .cmd(&["run", "--yes", "research three things"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (_, all) = env.parent_transcript();
    assert!(
        all.contains("report from a") && !all.contains("truncated"),
        "{all}"
    );
}
