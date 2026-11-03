//! M1/M2 end-to-end: the real binary with the scripted mock provider.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::json;

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    tern_home: PathBuf,
    script: PathBuf,
}

impl Env {
    fn new(script: serde_json::Value) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let script_path = tmp.path().join("script.json");
        std::fs::write(&script_path, script.to_string()).unwrap();
        Self {
            tern_home: home.join(".tern"),
            home,
            script: script_path,
            _tmp: tmp,
        }
    }

    fn workspace(&self) -> PathBuf {
        self.home.join("tern-workspace")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tern"));
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env("TERN_PROVIDER", format!("mock:{}", self.script.display()))
            .stdin(std::process::Stdio::null());
        c
    }

    fn tern(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }
}

fn out(o: &Output) -> (String, String) {
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
fn create_then_read_back_a_file() {
    let env = Env::new(json!({ "turns": [
        { "text": "Writing.", "tool_calls": [{ "name": "write_file", "input": { "path": "hello.txt", "content": "hi" } }] },
        { "tool_calls": [{ "name": "read_file", "input": { "path": "hello.txt" } }] },
        { "text": "hello.txt contains: hi" }
    ]}));
    let o = env.tern(&[
        "run",
        "--yes",
        "create hello.txt containing hi, then read it back",
    ]);
    let (stdout, stderr) = out(&o);
    assert!(o.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout.trim(), "hello.txt contains: hi");
    assert!(stderr.contains("→ write_file hello.txt"), "{stderr}");
    assert!(stderr.contains("→ read_file hello.txt"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(env.workspace().join("hello.txt")).unwrap(),
        "hi"
    );
}

#[test]
fn shell_is_denied_without_yes_when_not_interactive() {
    let env = Env::new(json!({ "turns": [
        { "tool_calls": [{ "name": "shell", "input": { "command": "touch made.txt" } }] },
        { "text": "Could not run it." }
    ]}));
    let o = env.tern(&["run", "make a file"]);
    let (stdout, stderr) = out(&o);
    assert!(o.status.success(), "{stderr}");
    assert_eq!(stdout.trim(), "Could not run it.");
    assert!(stderr.contains("shell: not approved"), "{stderr}");
    assert!(!env.workspace().join("made.txt").exists());
}

#[test]
fn sessions_carry_conversation_across_runs() {
    let env = Env::new(json!({ "turns": [ { "text": "first" }, { "text": "second" } ] }));
    let o = env.tern(&["run", "one"]);
    let (_, stderr) = out(&o);
    let session = stderr
        .split("session ")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .to_owned();
    // The mock picks its reply by the number of prior assistant turns.
    let o = env.tern(&["run", "--session", &session, "two"]);
    assert_eq!(out(&o).0.trim(), "second");
}

#[test]
fn missing_model_without_mock_is_explained() {
    let env = Env::new(json!({ "turns": [] }));
    let o = env
        .cmd(&["run", "hi"])
        .env_remove("TERN_PROVIDER")
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(out(&o).1.contains("no model selected"), "{}", out(&o).1);
}

/// Spec §16 M2 acceptance: three tool calls, crash while the second is in
/// flight, resume; a counter file proves each completed tool ran exactly
/// once and the in-flight non-idempotent one was not re-run.
#[test]
fn crash_mid_run_then_resume_runs_each_tool_at_most_once() {
    let shell = |n: u32| json!({ "name": "shell", "input": { "command": format!("echo {n} >> counter.txt") } });
    let env = Env::new(json!({ "turns": [
        { "tool_calls": [shell(1)] },
        { "tool_calls": [shell(2)] },
        { "tool_calls": [shell(3)] },
        { "text": "all done" }
    ]}));
    // shell always gets a journaled approval step (auto-approved by --yes):
    // 0 llm, 1 approval, 2 tool#1, 3 llm, 4 approval, 5 tool#2 <- abort here.
    let o = env
        .cmd(&["run", "--yes", "count"])
        .env("TERN_CRASH_AFTER_STEP", "5")
        .output()
        .unwrap();
    let (stdout, stderr) = out(&o);
    assert!(
        !o.status.success(),
        "process should have aborted\n{stdout}\n{stderr}"
    );
    let run = run_id(&stderr);
    let counter = env.workspace().join("counter.txt");
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "1\n");

    let o = env.tern(&["resume", "--yes", &run]);
    let (stdout, stderr) = out(&o);
    assert!(o.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stdout.trim(), "all done");
    assert!(stderr.contains("(replayed)"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap(),
        "1\n3\n",
        "tool#2 must not re-run"
    );

    // Resuming a completed run is refused.
    assert!(!env.tern(&["resume", &run]).status.success());
}

#[test]
fn secrets_never_reach_the_shell_environment() {
    let env = Env::new(json!({ "turns": [
        { "tool_calls": [{ "name": "shell", "input": { "command": "env > env.txt" } }] },
        { "text": "ok" }
    ]}));
    let o = env
        .cmd(&["run", "--yes", "dump env"])
        .env("ANTHROPIC_API_KEY", "sk-should-not-leak")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", out(&o).1);
    let dumped = std::fs::read_to_string(env.workspace().join("env.txt")).unwrap();
    assert!(!dumped.contains("sk-should-not-leak"));
    assert!(!dumped.contains("TERN_PROVIDER"));
}

/// Spec §16 M1 acceptance against a real provider. Needs network and a key:
///
/// ```sh
/// TERN_SMOKE_MODEL=anthropic:claude-sonnet-4-5 ANTHROPIC_API_KEY=... \
///   cargo test -p tern --test m1 -- --ignored real_provider
/// ```
///
/// The provider block is generated from the model's provider name:
/// `anthropic` uses ANTHROPIC_API_KEY; anything else is treated as
/// OpenAI-compatible with TERN_SMOKE_BASE_URL and optional TERN_SMOKE_API_KEY.
#[test]
#[ignore = "needs network and a real API key"]
fn real_provider_creates_and_reads_back_a_file() {
    let model = std::env::var("TERN_SMOKE_MODEL").expect("set TERN_SMOKE_MODEL=provider:model");
    let provider = model.split(':').next().unwrap().to_owned();
    let env = Env::new(json!({ "turns": [] }));
    std::fs::create_dir_all(&env.tern_home).unwrap();
    let block = if provider == "anthropic" {
        "kind = \"anthropic\"\napi_key_secret = \"ANTHROPIC_API_KEY\"\n".to_owned()
    } else {
        let base = std::env::var("TERN_SMOKE_BASE_URL").expect("set TERN_SMOKE_BASE_URL");
        format!(
            "kind = \"openai_compat\"\nbase_url = \"{base}\"\napi_key_secret = \"TERN_SMOKE_API_KEY\"\n"
        )
    };
    std::fs::write(
        env.tern_home.join("config.toml"),
        format!("default_model = \"{model}\"\n\n[providers.{provider}]\n{block}"),
    )
    .unwrap();
    let mut cmd = env.cmd(&[
        "run",
        "--yes",
        "create hello.txt containing hi, then read it back",
    ]);
    cmd.env_remove("TERN_PROVIDER");
    for key in ["ANTHROPIC_API_KEY", "TERN_SMOKE_API_KEY"] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    let o = cmd.output().unwrap();
    let (stdout, stderr) = out(&o);
    assert!(o.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        std::fs::read_to_string(env.workspace().join("hello.txt"))
            .unwrap()
            .trim(),
        "hi"
    );
    assert!(stderr.contains("read_file"), "{stderr}");
    println!("{stdout}");
}
