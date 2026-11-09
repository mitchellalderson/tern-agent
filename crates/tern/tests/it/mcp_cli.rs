//! `tern mcp` (docs/mcp-management-spec.md, M1): import, test, list,
//! toggle and remove, once against the embedded runtime and once through a
//! running daemon (the remote backend). The server is tern-mcp's Python
//! fixture.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const TOKEN: &str = "sk-fixturetoken0123456789";

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
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(&tern_home).unwrap();
        std::fs::write(tern_home.join("script.json"), r#"{"turns": []}"#).unwrap();
        Self {
            _tmp: tmp,
            home,
            tern_home,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tern"));
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env(
                "TERN_PROVIDER",
                format!("mock:{}", self.tern_home.join("script.json").display()),
            )
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "tern {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn daemon(&self) -> Killed {
        let child = self
            .cmd(&["daemon"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while !self.tern_home.join("tern.sock").exists() {
            assert!(Instant::now() < end, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Killed(child)
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.tern_home.join(file)).unwrap_or_default()
    }
}

fn fixture() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tern-mcp/tests/fixtures/mcp_server.py"
    )
    .into()
}

fn scenario(env: &Env) {
    // Import a Claude Desktop style snippet: the key-like env value becomes
    // a secret, stored in secrets.toml only.
    let snippet = env.tern_home.join("paste.json");
    std::fs::write(
        &snippet,
        serde_json::json!({ "mcpServers": { "fx": {
            "command": "python3",
            "args": [fixture()],
            "env": { "FIXTURE_TOKEN": TOKEN, "LOG_LEVEL": "warn" },
        }}})
        .to_string(),
    )
    .unwrap();
    let out = env.ok(&["mcp", "import", snippet.to_str().unwrap()]);
    assert!(
        out.contains("fx: connected (fixture 1.0), 5 tools"),
        "{out}"
    );
    assert!(out.contains("added fx"), "{out}");
    assert!(env.read("secrets.toml").contains(TOKEN));
    let config = env.read("config.toml");
    assert!(
        config.contains("[mcp.servers.fx]") && config.contains("LOG_LEVEL"),
        "{config}"
    );
    assert!(!config.contains(TOKEN));

    let out = env.ok(&["mcp", "test", "fx"]);
    assert!(out.contains("connected"), "{out}");
    let out = env.ok(&["mcp", "list"]);
    assert!(out.contains("fx") && out.contains("stdio"), "{out}");
    let json: serde_json::Value =
        serde_json::from_str(&env.ok(&["mcp", "list", "--json"])).unwrap();
    assert_eq!(json[0]["name"], "fx");
    let out = env.ok(&["mcp", "show", "fx"]);
    assert!(out.contains("secret     FIXTURE_TOKEN (set)"), "{out}");

    env.ok(&["mcp", "disable", "fx"]);
    assert!(env.ok(&["mcp", "list"]).contains("disabled"));
    env.ok(&["mcp", "enable", "fx"]);

    // A server that won't start isn't saved, unless asked.
    let out = env.run(&["mcp", "add", "ghost", "--", "definitely-not-an-mcp-server"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Not saved"), "{err}");
    assert!(!env.read("config.toml").contains("ghost"));
    env.ok(&[
        "mcp",
        "add",
        "ghost",
        "--no-test",
        "--",
        "definitely-not-an-mcp-server",
    ]);
    assert!(env.read("config.toml").contains("[mcp.servers.ghost]"));

    // A secret that isn't set can't be typed without a terminal.
    let out = env.run(&[
        "mcp",
        "add",
        "needy",
        "--secret",
        "NOT_SET_X",
        "--",
        "python3",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs secret NOT_SET_X"));

    env.ok(&["mcp", "rm", "ghost"]);
    env.ok(&["mcp", "rm", "fx"]);
    assert!(!env.read("secrets.toml").contains(TOKEN));
    assert!(env.ok(&["mcp", "list"]).contains("No MCP servers yet"));
}

#[test]
fn mcp_commands_embedded() {
    scenario(&Env::new());
}

#[test]
fn mcp_commands_through_the_daemon() {
    let env = Env::new();
    let _daemon = env.daemon();
    scenario(&env);
}
