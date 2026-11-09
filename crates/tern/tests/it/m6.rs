//! Diagnostics: crash files and `tern report`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use serde_json::json;

/// The binary under test: `TERN_TEST_BIN` (e.g. a `--profile dist` build,
/// whose settings can differ from test builds) or this crate's build.
fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

const PLANTED: &str = "super-secret-value-123";

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
        std::fs::create_dir_all(tern_home.join("logs")).unwrap();
        Self {
            script: tmp.path().join("script.json"),
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

    fn tern(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }
}

#[test]
fn panics_leave_a_crash_file_and_say_where() {
    let env = Env::new();
    let o = env
        .cmd(&["doctor"])
        .env("TERN_TEST_PANIC", "1")
        .output()
        .unwrap();
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains("tern hit an internal error") && stderr.contains("crash-"),
        "{stderr}"
    );
    let crash = std::fs::read_dir(env.tern_home.join("logs"))
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("crash-"))
        .expect("crash file");
    let body = std::fs::read_to_string(crash.path()).unwrap();
    assert!(
        body.contains("message: TERN_TEST_PANIC is set") && body.contains("backtrace:"),
        "{body}"
    );
}

#[test]
fn report_collects_the_problem_run_crashes_and_logs_without_secrets() {
    let env = Env::new();
    std::fs::write(
        env.tern_home.join("config.toml"),
        format!("# pasted by mistake: {PLANTED}\nmax_iterations = 10\n"),
    )
    .unwrap();
    let secrets = env.tern_home.join("secrets.toml");
    std::fs::write(&secrets, format!("PLANTED = \"{PLANTED}\"\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(
        &env.script,
        json!({ "turns": [
            { "tool_calls": [{ "name": "read_file", "input": { "path": "missing.txt" } }] },
            { "tool_calls": [{ "name": "write_file", "input": { "path": "x.txt", "content": "x" } }] },
            { "text": "never reached" }
        ]})
        .to_string(),
    )
    .unwrap();
    // Abort while the second tool call is in flight: the run stays unfinished.
    let o = env
        .cmd(&["run", "--yes", "go"])
        .env("TERN_CRASH_AFTER_STEP", "3")
        .output()
        .unwrap();
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    let run = stderr
        .lines()
        .find_map(|l| l.strip_prefix("run "))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let _ = env
        .cmd(&["doctor"])
        .env("TERN_TEST_PANIC", "1")
        .output()
        .unwrap();

    let o = env.tern(&["report", "-o", "-"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let report = String::from_utf8_lossy(&o.stdout).into_owned();
    for want in [
        "# tern bug report",
        "## tern doctor --offline",
        "## config.toml",
        &format!("## Run {run} (most recent running run)"),
        "tool       completed",
        "## Crash: crash-",
        "## Log: last",
        "tool call failed",
        "run started",
    ] {
        assert!(report.contains(want), "missing {want:?} in:\n{report}");
    }
    assert!(!report.contains(PLANTED), "secret leaked:\n{report}");
    assert!(
        report.contains("# pasted by mistake: [REDACTED]"),
        "{report}"
    );

    // By default it's written next to the logs, readable only by the user.
    let o = env.tern(&["report"]);
    let path = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    assert!(
        path.starts_with(env.tern_home.join("logs")) && path.exists(),
        "{}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
