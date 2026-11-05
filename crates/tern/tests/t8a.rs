//! T8a end-to-end: `/v1/status`, `/v1/shutdown` (dry run, exit code 0,
//! runs resume), owner-only shutdown over TCP, `tern start`/`stop` with
//! escalation against a daemon that ignores shutdown, and `tern service`
//! writing its plist or unit. The service manager is always a fake script
//! (`TERN_LAUNCHCTL`/`TERN_SYSTEMCTL`): nothing touches launchd or systemd.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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
    script: PathBuf,
    /// Every call to the fake launchctl/systemctl, one per line.
    calls: PathBuf,
    fake: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(&tern_home).unwrap();
        let calls = tmp.path().join("calls.txt");
        let fake = tmp.path().join("fake-service-manager");
        // Nothing is loaded or enabled; every other command succeeds.
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1 $2\" in\n  print*|*is-enabled*|*is-active*) exit 1 ;;\nesac\nexit 0\n",
                calls.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            script: tmp.path().join("script.json"),
            home,
            tern_home,
            calls,
            fake,
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
            .env("TERN_LAUNCHCTL", &self.fake)
            .env("TERN_SYSTEMCTL", &self.fake)
            .stdin(Stdio::null());
        c
    }

    fn out(&self, args: &[&str]) -> (bool, String) {
        let out = self.cmd(args).output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
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

    fn pid(&self) -> u32 {
        std::fs::read_to_string(self.tern_home.join("tern.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
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

async fn get(http: &reqwest::Client, path: &str) -> Value {
    http.get(format!("http://tern{path}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn post(http: &reqwest::Client, path: &str, body: Value) -> Value {
    let r = http
        .post(format!("http://tern{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{path}: {}", r.status());
    r.json().await.unwrap()
}

fn process_exists(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test(flavor = "multi_thread")]
async fn status_dry_run_and_shutdown_then_resume() {
    let env = Env::new();
    env.script(json!({ "turns": [{ "text": "Report summarized.", "delay_ms": 2500 }] }));
    let mut daemon = env.daemon();
    let http = api(&env.socket());

    let status = get(&http, "/v1/status").await;
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(status["pid"], env.pid());
    assert!(status["uptime_secs"].is_u64());
    assert_eq!(status["managed_by"], Value::Null);
    assert_eq!(status["running_runs"], 0);
    assert_eq!(status["pending_approvals"], 0);
    assert_eq!(status["active_schedules"], 0);
    assert_eq!(status["gateways"], json!([]));

    // Nothing running: a dry run lists nothing and stops nothing.
    let dry = post(&http, "/v1/shutdown?dry_run=1", json!({})).await;
    assert_eq!(dry, json!({ "stopping": false, "runs": [] }));

    let started = post(
        &http,
        "/v1/runs",
        json!({ "text": "Summarize the Q3 report" }),
    )
    .await;
    let run_id = started["run_id"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(get(&http, "/v1/status").await["running_runs"], 1);

    let dry = post(&http, "/v1/shutdown?dry_run=1", json!({})).await;
    assert_eq!(dry["stopping"], false);
    assert_eq!(dry["runs"][0]["run_id"], run_id.as_str());
    assert_eq!(dry["runs"][0]["title"], "Summarize the Q3 report");
    assert!(
        daemon.0.try_wait().unwrap().is_none(),
        "a dry run stops nothing"
    );

    let reply = post(&http, "/v1/shutdown", json!({})).await;
    assert_eq!(reply["stopping"], true);
    assert_eq!(reply["runs"].as_array().unwrap().len(), 1);
    let end = Instant::now() + Duration::from_secs(10);
    let code = loop {
        if let Some(s) = daemon.0.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < end, "daemon did not exit");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(code.code(), Some(0), "a requested shutdown exits 0");
    let status: String = env
        .db()
        .query_row("SELECT status FROM runs WHERE id = ?1", [&run_id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "running", "stopping leaves the run to resume");

    // The next start resumes it.
    let _daemon = env.daemon();
    let http = api(&env.socket());
    let done = get(&http, &format!("/v1/runs/{run_id}/wait")).await;
    assert_eq!(done["status"], "completed");
    assert_eq!(done["final_text"], "Report summarized.");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_over_tcp_needs_owner_scope() {
    let env = Env::new();
    env.script(json!({ "turns": [] }));
    let http = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    // A port picked by binding and dropping can be taken before the daemon
    // binds it (another test's listener). Then the daemon exits, or the port
    // answers but isn't ours; try another port. Ready means *our* daemon
    // answers over TLS (401: no token), not just that something accepts.
    let (port, _daemon) = 'start: {
        for _ in 0..5 {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            std::fs::write(
                env.tern_home.join("config.toml"),
                format!("[api]\nlisten = [\"unix\", \"tcp://127.0.0.1:{port}\"]\ntls = \"auto\"\n"),
            )
            .unwrap();
            let mut daemon = env.daemon();
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                if daemon.0.try_wait().unwrap().is_some() {
                    break; // could not bind; pick another port
                }
                let r = http
                    .post(format!("https://127.0.0.1:{port}/v1/shutdown"))
                    .send()
                    .await;
                if matches!(&r, Ok(r) if r.status() == 401) {
                    break 'start (port, daemon);
                }
                assert!(Instant::now() < end, "tcp listener did not start");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        panic!("no free port after 5 tries");
    };
    let url = |p: &str| format!("https://127.0.0.1:{port}{p}");
    let stdout = |args: &[&str]| {
        let out = env.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let token = |scope: &str| {
        let link = stdout(&["devices", "pair", "--scope", scope]);
        format!("Bearer {}", stdout(&["devices", "join", &link]))
    };

    let r = http.post(url("/v1/shutdown")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    for scope in ["chat", "read"] {
        let auth = token(scope);
        let r = http
            .get(url("/v1/status"))
            .header("authorization", &auth)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{scope} may read the status");
        for path in ["/v1/shutdown", "/v1/shutdown?dry_run=1"] {
            let r = http
                .post(url(path))
                .header("authorization", &auth)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 403, "{scope} {path}");
        }
    }
    let owner = token("owner");
    let r = http
        .post(url("/v1/shutdown?dry_run=1"))
        .header("authorization", &owner)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // Still up after all of that.
    assert!(
        api(&env.socket())
            .get("http://tern/v1/health")
            .send()
            .await
            .is_ok()
    );
}

#[test]
fn stop_escalates_on_a_daemon_that_ignores_shutdown() {
    let env = Env::new();
    env.script(json!({ "turns": [] }));
    let (ok, out) = env.out(&["stop"]);
    assert!(ok, "{out}");
    assert!(out.contains("tern isn't running."), "{out}");

    let start = |extra: &[(&str, &str)]| {
        let mut c = env.cmd(&["start"]);
        for (k, v) in extra {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        text
    };
    let wedged = [
        ("TERN_TEST_IGNORE_SHUTDOWN", "1"),
        ("TERN_TEST_STOP_WAIT_MS", "1000"),
    ];
    let out = start(&wedged);
    assert!(out.contains("tern is running (pid"), "{out}");
    let pid = env.pid();
    assert!(process_exists(pid));
    let out = start(&[]);
    assert!(out.contains("already running"), "{out}");

    let begun = Instant::now();
    let mut c = env.cmd(&["stop"]);
    c.env("TERN_TEST_STOP_WAIT_MS", "1000");
    let out = c.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("stopped by force"), "{text}");
    assert!(begun.elapsed() < Duration::from_secs(20));
    let end = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) {
        assert!(Instant::now() < end, "pid {pid} is still there");
        std::thread::sleep(Duration::from_millis(100));
    }

    // A well-behaved daemon stops on the first ask.
    start(&[]);
    let pid = env.pid();
    let (ok, out) = env.out(&["stop"]);
    assert!(ok, "{out}");
    assert!(out.contains("tern is stopped."), "{out}");
    assert!(!out.contains("force"), "{out}");
    let end = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) {
        assert!(Instant::now() < end, "pid {pid} is still there");
        std::thread::sleep(Duration::from_millis(100));
    }
    // No service was involved beyond asking whether one is loaded.
    let calls = std::fs::read_to_string(&env.calls).unwrap_or_default();
    assert!(
        calls
            .lines()
            .all(|l| l.starts_with("print") || l.contains("is-enabled") || l.contains("is-active")),
        "{calls}"
    );
}

#[test]
fn service_install_writes_the_plist_or_unit() {
    let env = Env::new();
    let (path, at_login, not_at_login) = if cfg!(target_os = "macos") {
        (
            env.home.join("Library/LaunchAgents/ai.tern.daemon.plist"),
            "<key>RunAtLoad</key>\n\t<true/>",
            "<key>RunAtLoad</key>\n\t<false/>",
        )
    } else {
        (
            env.home.join(".config/systemd/user/tern-daemon.service"),
            "--user enable tern-daemon.service",
            "--user disable tern-daemon.service",
        )
    };

    let (ok, out) = env.out(&["service", "install"]);
    assert!(ok, "{out}");
    assert!(out.contains("tern will start when you log in"), "{out}");
    let text = std::fs::read_to_string(&path).unwrap();
    let exe = tern_bin().canonicalize().unwrap();
    let calls = std::fs::read_to_string(&env.calls).unwrap();
    let tern_home = env.tern_home.canonicalize().unwrap();
    if cfg!(target_os = "macos") {
        assert!(text.contains("<string>ai.tern.daemon</string>"), "{text}");
        assert!(
            text.contains(&format!("<string>{}</string>", exe.display())),
            "{text}"
        );
        assert!(
            text.contains("<key>SuccessfulExit</key>\n\t\t<false/>"),
            "{text}"
        );
        assert!(text.contains(at_login), "{text}");
        assert!(
            text.contains(&format!("{}/logs/daemon.err.log", tern_home.display())),
            "{text}"
        );
        assert!(calls.contains("bootstrap gui/"), "{calls}");
    } else {
        assert!(
            text.contains(&format!("ExecStart=\"{}\" daemon", exe.display())),
            "{text}"
        );
        assert!(text.contains("Restart=on-failure"), "{text}");
        assert!(text.contains("TERN_SERVICE=systemd"), "{text}");
        assert!(calls.contains(at_login), "{calls}");
    }

    let (ok, out) = env.out(&["service", "status"]);
    assert!(ok, "{out}");
    assert!(out.contains("installed: yes"), "{out}");
    assert!(out.contains("running:   no"), "{out}");

    // Doctor names the method.
    let (_, out) = env.out(&["doctor", "--offline"]);
    assert!(out.contains("managed by:"), "{out}");

    let _ = std::fs::remove_file(&env.calls);
    let (ok, out) = env.out(&["service", "install", "--no-login"]);
    assert!(ok, "{out}");
    assert!(out.contains("won't start at login"), "{out}");
    let calls = std::fs::read_to_string(&env.calls).unwrap_or_default();
    if cfg!(target_os = "macos") {
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(not_at_login), "{text}");
        assert!(!calls.contains("bootstrap"), "{calls}");
    } else {
        assert!(calls.contains(not_at_login), "{calls}");
    }

    let (ok, out) = env.out(&["service", "uninstall"]);
    assert!(ok, "{out}");
    assert!(!path.exists());
    let (_, out) = env.out(&["service", "uninstall"]);
    assert!(out.contains("No tern service was installed."), "{out}");
}
