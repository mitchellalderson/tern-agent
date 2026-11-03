//! M2 end-to-end: inspection commands, memory, compaction, cancellation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// The binary under test: `TERN_TEST_BIN` (e.g. a `--profile dist` build,
/// whose settings can differ from test builds) or this crate's build.
fn tern_bin() -> std::path::PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

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

    fn set_script(&self, script: serde_json::Value) {
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
            .stdin(Stdio::null());
        c
    }

    fn tern(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn ids(stderr: &str) -> (String, String) {
    let line = stderr
        .lines()
        .find(|l| l.starts_with("run "))
        .expect("run line");
    let run = line.split_whitespace().nth(1).unwrap().to_owned();
    let session = line
        .split("session ")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .to_owned();
    (run, session)
}

#[test]
fn runs_show_prints_the_journal() {
    let env = Env::new(json!({ "turns": [
        { "text": "Listing.", "tool_calls": [{ "name": "shell", "input": { "command": "echo hi" } }] },
        { "text": "Saw hi." }
    ]}));
    let o = env.tern(&["run", "--yes", "say hi"]);
    let (run, session) = ids(&text(&o).1);

    let show = text(&env.tern(&["runs", "show", &run])).0;
    assert!(show.contains("status   completed"), "{show}");
    let lines: Vec<&str> = show
        .lines()
        .filter(|l| l.trim_start().starts_with(char::is_numeric))
        .collect();
    assert_eq!(lines.len(), 4, "{show}");
    assert!(
        lines[0].contains("llm") && lines[0].contains("→ shell"),
        "{show}"
    );
    assert!(
        lines[1].contains("approval") && lines[1].contains("approved"),
        "{show}"
    );
    assert!(
        lines[2].contains("tool") && lines[2].contains("✓ exit code 0"),
        "{show}"
    );
    assert!(lines[3].contains("\"Saw hi.\""), "{show}");

    assert!(text(&env.tern(&["runs", "list"])).0.contains(&run));
    let list = text(&env.tern(&["sessions"])).0;
    assert!(list.contains(&session) && list.contains("say hi"), "{list}");
    let shown = text(&env.tern(&["sessions", "show", &session])).0;
    assert!(
        shown.contains("[user] say hi")
            && shown.contains("→ shell")
            && shown.contains("[assistant] Saw hi."),
        "{shown}"
    );
}

#[test]
fn memory_persists_across_sessions() {
    let env = Env::new(json!({ "turns": [
        { "tool_calls": [{ "name": "memory_save", "input": { "content": "User's favorite color is teal", "tags": ["prefs"] } }] },
        { "text": "Noted." }
    ]}));
    assert!(env.tern(&["run", "remember teal"]).status.success());

    env.set_script(json!({ "turns": [
        { "tool_calls": [{ "name": "memory_search", "input": { "query": "favorite color" } }] },
        { "text": "Teal." }
    ]}));
    let o = env.tern(&["run", "what is my favorite color?"]);
    let (run, _) = ids(&text(&o).1);
    let show = text(&env.tern(&["runs", "show", &run])).0;
    assert!(show.contains("favorite color is teal"), "{show}");

    env.set_script(json!({ "turns": [
        { "tool_calls": [{ "name": "session_search", "input": { "query": "remember" } }] },
        { "text": "Found it." }
    ]}));
    let o = env.tern(&["run", "when did we talk about it?"]);
    let (run, _) = ids(&text(&o).1);
    let show = text(&env.tern(&["runs", "show", &run])).0;
    assert!(
        show.contains("remember teal"),
        "session_search finds the earlier session: {show}"
    );
}

#[test]
fn long_runs_are_compacted() {
    let mut turns: Vec<_> = (0..10)
        .map(|i| json!({ "tool_calls": [{ "name": "write_file", "input": { "path": format!("f{i}.txt"), "content": "x".repeat(400) } }] }))
        .collect();
    turns.push(json!({ "text": "wrote ten files" }));
    let env = Env::new(json!({ "turns": turns }));
    std::fs::create_dir_all(&env.tern_home).unwrap();
    // Built-in tool specs alone are ~1k tokens; leave room for a few turns.
    std::fs::write(env.tern_home.join("config.toml"), "context_window = 2000\n").unwrap();

    let o = env.tern(&["run", "--yes", "write ten files"]);
    let (stdout, stderr) = text(&o);
    assert!(o.status.success(), "{stderr}");
    assert_eq!(
        stdout.trim(),
        "wrote ten files",
        "turn counting survived compaction"
    );
    let (run, session) = ids(&stderr);
    let show = text(&env.tern(&["runs", "show", &run])).0;
    assert!(show.contains("compaction"), "{show}");
    assert!(
        text(&env.tern(&["sessions", "show", &session]))
            .0
            .contains("[summary]")
    );
    for i in 0..10 {
        assert!(env.home.join(format!("tern-workspace/f{i}.txt")).exists());
    }
}

#[cfg(unix)]
#[test]
fn ctrl_c_interrupts_and_resume_finishes() {
    let env = Env::new(json!({ "turns": [ { "text": "eventually", "delay_ms": 1500 } ] }));
    use std::io::{BufRead, Read};
    let mut child = env
        .cmd(&["run", "slow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The banner is printed once Ctrl+C handling is installed.
    let mut err = std::io::BufReader::new(child.stderr.take().unwrap());
    let mut stderr = String::new();
    err.read_line(&mut stderr).unwrap();
    assert!(stderr.starts_with("run "), "{stderr}");
    std::thread::sleep(Duration::from_millis(100));
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    err.read_to_string(&mut stderr).unwrap();
    let o = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(o.status.code(), Some(130), "{stdout}\n{stderr}");
    assert!(
        stderr.contains("interrupted; continue with `tern resume"),
        "{stderr}"
    );
    let (run, _) = ids(&stderr);
    assert!(
        text(&env.tern(&["runs", "show", &run]))
            .0
            .contains("status   interrupted")
    );

    let o = env.tern(&["resume", &run]);
    assert!(o.status.success(), "{}", text(&o).1);
    assert_eq!(text(&o).0.trim(), "eventually");
}
