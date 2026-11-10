//! Shared by the S2 and S3 end-to-end tests: a tern home with sub-agents
//! enabled and a mock provider script, a daemon, and API helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The binary under test: `TERN_TEST_BIN` or this crate's build.
pub fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

/// A scratch dir outside the temp dirs (which the sandbox lets commands
/// write, so its check has nowhere to probe): the checkout's target dir,
/// or `$HOME` when the checkout itself lives under a temp dir.
pub fn outside_temp() -> tempfile::TempDir {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut temps = vec![canon(Path::new("/tmp")), canon(Path::new("/var/tmp"))];
    temps.extend(
        std::env::var_os("TMPDIR")
            .filter(|t| !t.is_empty())
            .map(|t| canon(Path::new(&t))),
    );
    temps.push(canon(&std::env::temp_dir()));
    let target = canon(Path::new(env!("CARGO_TARGET_TMPDIR")));
    if temps.iter().any(|t| target.starts_with(t)) {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        tempfile::Builder::new()
            .prefix(".tern-test-")
            .tempdir_in(home)
            .unwrap()
    } else {
        tempfile::tempdir_in(target).unwrap()
    }
}

pub struct Killed(pub Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub struct Env {
    _tmp: tempfile::TempDir,
    pub home: PathBuf,
    pub tern_home: PathBuf,
    script: PathBuf,
}

impl Env {
    pub fn new(script: &Value) -> Self {
        Self::with_config(script, "[subagents]\nenabled = true\n")
    }

    pub fn with_config(script: &Value, config: &str) -> Self {
        Self::in_dir(tempfile::tempdir().unwrap(), script, config)
    }

    /// Like [`Env::with_config`], but with the home outside the temp dirs
    /// ([`outside_temp`]), so the shell's sandbox check can verify. Under a
    /// temp dir it never can, and whether the shell then counts as unbounded
    /// (allowlist ignored, no "always") depends on how fast the check runs.
    pub fn with_config_sandboxable(script: &Value, config: &str) -> Self {
        Self::in_dir(outside_temp(), script, config)
    }

    fn in_dir(tmp: tempfile::TempDir, script: &Value, config: &str) -> Self {
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

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(tern_bin());
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env("TERN_PROVIDER", format!("mock:{}", self.script.display()))
            .env("TERN_SEED_AGENTS", "0")
            // Workspaces live in temp dirs here; the daemon allows them.
            .env("TERN_TEST_ALLOW_SCRATCH_WORKSPACES", "1")
            .stdin(Stdio::null());
        c
    }

    pub fn socket(&self) -> PathBuf {
        self.tern_home.join("tern.sock")
    }

    pub fn daemon_with(&self, crash_after: Option<u32>) -> Killed {
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
        while !self.socket().exists() {
            assert!(Instant::now() < end, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Killed(child)
    }

    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.tern_home.join("tern.db")).unwrap()
    }

    pub fn query<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// (label, status, error) of every sub-agent, by label.
    pub fn children(&self) -> Vec<(String, String, Option<String>)> {
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

    pub fn parent_status(&self) -> String {
        self.query("SELECT status FROM runs WHERE parent_run_id IS NULL")
    }

    pub fn wait_for(&self, what: &str, secs: u64, mut ok: impl FnMut(&Self) -> bool) {
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

    /// Every message text of the runs matching `where_` (on `runs r`),
    /// oldest first, joined; and the last assistant text.
    pub fn transcript(&self, where_: &str) -> (String, String) {
        let db = self.db();
        let mut stmt = db
            .prepare(&format!(
                "SELECT m.role, m.text FROM messages m JOIN runs r ON r.id = m.run_id
                 WHERE {where_} ORDER BY m.id"
            ))
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
    pub fn wait_abort(&self, d: &mut Killed) {
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

pub fn api(socket: &Path) -> reqwest::Client {
    reqwest::Client::builder()
        .unix_socket(socket)
        .build()
        .unwrap()
}

/// GET `path`: (status, JSON body).
pub async fn get(socket: &Path, path: &str) -> (u16, Value) {
    let r = api(socket)
        .get(format!("http://tern{path}"))
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

/// POST `body` to `path`: (status, JSON body).
pub async fn post(socket: &Path, path: &str, body: Value) -> (u16, Value) {
    let r = api(socket)
        .post(format!("http://tern{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

pub async fn start(socket: &Path, text: &str) -> Value {
    post(socket, "/v1/runs", json!({ "text": text })).await.1
}

/// Collects SSE events from `/v1/events`.
pub struct Events {
    resp: reqwest::Response,
    buf: String,
}

impl Events {
    pub async fn open(socket: &Path) -> Self {
        let resp = api(socket)
            .get("http://tern/v1/events")
            .send()
            .await
            .unwrap();
        Self {
            resp,
            buf: String::new(),
        }
    }

    /// Read until `done` holds for the events so far; returns them all.
    pub async fn until(&mut self, secs: u64, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            let evs: Vec<Value> = self
                .buf
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .filter_map(|l| serde_json::from_str(l.trim()).ok())
                .collect();
            if done(&evs) {
                return evs;
            }
            assert!(Instant::now() < end, "timed out; events so far: {evs:#?}");
            let chunk = tokio::time::timeout(Duration::from_secs(60), self.resp.chunk())
                .await
                .expect("an event within 60s")
                .unwrap()
                .unwrap();
            self.buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
}
