//! T6a end-to-end (docs/personal-agent-tooling-spec.md §8.3, §12.1): a
//! `default_model` chain falls back on overload and not on a bad request,
//! through the real binary, chain and journal, with the mock provider.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::json;

fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    tern_home: PathBuf,
    script: PathBuf,
}

impl Env {
    /// `default_model = ["mock:down", "mock:up"]`; `down` fails with `status`.
    fn new(status: u16) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(&tern_home).unwrap();
        std::fs::write(
            tern_home.join("config.toml"),
            "default_model = [\"mock:down\", \"mock:up\"]\n",
        )
        .unwrap();
        let script = tmp.path().join("script.json");
        let body = json!({
            "fail_models": { "down": status },
            "turns": [{ "text": "hello from up" }],
        });
        std::fs::write(&script, body.to_string()).unwrap();
        Self {
            _tmp: tmp,
            home,
            tern_home,
            script,
        }
    }

    fn tern(&self, args: &[&str]) -> Output {
        Command::new(tern_bin())
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env("TERN_PROVIDER", format!("mock:{}", self.script.display()))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn run_id(stderr: &str) -> String {
    stderr
        .lines()
        .find_map(|l| l.strip_prefix("run "))
        .and_then(|l| l.split_whitespace().next())
        .expect("run id on stderr")
        .to_owned()
}

#[test]
fn overload_falls_back_and_the_run_says_so() {
    let env = Env::new(529);
    let o = env.tern(&["run", "--yes", "hi"]);
    let (stdout, stderr) = text(&o);
    assert!(o.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout.trim(), "hello from up");

    let run = run_id(&stderr);
    let (show, _) = text(&env.tern(&["runs", "show", &run]));
    assert!(show.contains("model    mock:down"), "{show}");
    assert!(
        show.contains("fallback 1 step(s) answered by a fallback model"),
        "{show}"
    );
    assert!(
        show.contains("answered by mock:up (mock:down HTTP 529 (overloaded))"),
        "{show}"
    );
}

#[test]
fn a_bad_request_does_not_fall_back() {
    let env = Env::new(400);
    let o = env.tern(&["run", "--yes", "hi"]);
    let (stdout, stderr) = text(&o);
    assert!(!o.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("400"), "{stderr}");
    assert!(!stdout.contains("hello from up"), "{stdout}");
}
