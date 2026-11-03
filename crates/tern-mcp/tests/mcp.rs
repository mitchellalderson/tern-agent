//! MCP client against an independent (Python, stdlib-only) server.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tern_core::Secrets;
use tern_core::config::{McpServerConfig, McpTransport};
use tern_core::tool::{Effect, ToolCtx, ToolSource};
use tern_core::types::ToolCall;
use tern_mcp::{McpServer, McpServers};
use tokio_util::sync::CancellationToken;

fn fixture() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp_server.py")
        .display()
        .to_string()
}

fn stdio_cfg(env_secrets: Vec<String>) -> McpServerConfig {
    McpServerConfig {
        transport: McpTransport::Stdio,
        command: Some("python3".into()),
        args: vec![fixture()],
        url: None,
        env_secrets,
        idempotent: false,
        requires_approval: false,
        effects: Default::default(),
    }
}

fn secrets(pairs: &[(&str, &str)]) -> (Arc<Secrets>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("secrets.toml");
    std::fs::write(
        &p,
        pairs
            .iter()
            .map(|(k, v)| format!("{k} = \"{v}\"\n"))
            .collect::<String>(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    (Arc::new(Secrets::load(&p).unwrap()), tmp)
}

fn ctx() -> ToolCtx {
    ToolCtx {
        workspace: std::env::temp_dir(),
        run_id: "r".into(),
        session_id: "s".into(),
        cancel: CancellationToken::new(),
    }
}

async fn call(
    set: &McpServers,
    name: &str,
    input: serde_json::Value,
) -> tern_core::tool::ToolResult {
    let mut reg = tern_core::tool::ToolRegistry::new();
    let set = Arc::new(McpServers::new(set.servers().to_vec()));
    reg.add_source(set);
    reg.call(
        &ctx(),
        &ToolCall {
            id: "1".into(),
            name: name.into(),
            input,
        },
    )
    .await
}

#[tokio::test]
async fn stdio_server_tools_are_listed_and_callable() {
    let (secrets, _tmp) = secrets(&[("FIXTURE_TOKEN", "tok-123")]);
    let set = McpServers::new(vec![McpServer::new(
        "fixture",
        stdio_cfg(vec!["FIXTURE_TOKEN".into()]),
        secrets,
    )]);
    assert!(set.tools().is_empty(), "lazy: nothing until prepared");
    set.prepare().await;
    let mut names: Vec<String> = set.tools().iter().map(|t| t.spec().name).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "mcp__fixture__add",
            "mcp__fixture__crash",
            "mcp__fixture__echo",
            "mcp__fixture__env",
            "mcp__fixture__fail"
        ]
    );
    let echo = set
        .tools()
        .into_iter()
        .find(|t| t.spec().name == "mcp__fixture__echo")
        .unwrap();
    assert_eq!(echo.spec().input_schema["required"], json!(["text"]));
    assert!(
        set.describe()[0].contains("MCP server fixture: 5 tools"),
        "{:?}",
        set.describe()
    );

    let r = call(&set, "mcp__fixture__echo", json!({"text": "hello"})).await;
    assert_eq!((r.content.as_str(), r.is_error), ("hello", false));
    assert_eq!(
        call(&set, "mcp__fixture__add", json!({"a": 2, "b": 3}))
            .await
            .content,
        "5"
    );
    let fail = call(&set, "mcp__fixture__fail", json!({})).await;
    assert!(fail.is_error && fail.content.contains("went wrong"));
    // Declared secrets reach the server; everything else from our env doesn't.
    assert_eq!(
        call(&set, "mcp__fixture__env", json!({"name": "FIXTURE_TOKEN"}))
            .await
            .content,
        "tok-123"
    );
    assert_eq!(
        call(
            &set,
            "mcp__fixture__env",
            json!({"name": "CARGO_MANIFEST_DIR"})
        )
        .await
        .content,
        "<unset>"
    );
}

#[tokio::test]
async fn crashed_servers_restart_with_backoff() {
    let (secrets, _tmp) = secrets(&[]);
    let server = McpServer::with_backoff(
        "fixture",
        stdio_cfg(vec![]),
        secrets,
        Duration::from_millis(200),
    );
    let set = McpServers::new(vec![server]);
    set.prepare().await;
    let crash = call(&set, "mcp__fixture__crash", json!({})).await;
    assert!(crash.is_error, "{}", crash.content);
    // Immediately after, the server is in backoff.
    let during = call(&set, "mcp__fixture__echo", json!({"text": "x"})).await;
    assert!(
        during.is_error && during.content.contains("retrying in"),
        "{}",
        during.content
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = call(&set, "mcp__fixture__echo", json!({"text": "back"})).await;
    assert_eq!((after.content.as_str(), after.is_error), ("back", false));
}

#[tokio::test]
async fn missing_commands_are_reported_not_fatal() {
    let (secrets, _tmp) = secrets(&[]);
    let mut cfg = stdio_cfg(vec![]);
    cfg.command = Some("definitely-not-a-real-mcp-server".into());
    let set = McpServers::new(vec![McpServer::new("ghost", cfg, secrets)]);
    set.prepare().await;
    assert!(set.tools().is_empty());
    assert!(
        set.describe()[0].contains("unavailable"),
        "{:?}",
        set.describe()
    );
}

#[tokio::test]
async fn missing_env_secrets_are_reported() {
    let (secrets, _tmp) = secrets(&[]);
    let server = McpServer::new(
        "fixture",
        stdio_cfg(vec!["NOT_SET_ANYWHERE_123".into()]),
        secrets,
    );
    let err = server.ensure().await.unwrap_err();
    assert!(
        err.contains("secret NOT_SET_ANYWHERE_123 is not set"),
        "{err}"
    );
}

#[tokio::test]
async fn streamable_http_transport() {
    /// Kills and reaps the fixture server even if an assertion fails.
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut server = Server(
        std::process::Command::new("python3")
            .args([fixture(), "--http".into()])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    std::io::BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line.trim().parse().unwrap();

    let (secrets, _tmp) = secrets(&[]);
    let cfg = McpServerConfig {
        transport: McpTransport::Http,
        command: None,
        args: vec![],
        url: Some(format!("http://127.0.0.1:{port}/mcp")),
        env_secrets: vec![],
        idempotent: true,
        requires_approval: true,
        effects: Default::default(),
    };
    let set = McpServers::new(vec![McpServer::new("remote", cfg, secrets)]);
    set.prepare().await;
    let tools = set.tools();
    assert_eq!(tools.len(), 5, "{:?}", set.describe());
    let policy = tools[0].policy(&ctx(), &json!({}));
    assert!(
        policy.idempotent && policy.requires_approval,
        "per-server policy overrides"
    );
    let r = call(&set, "mcp__remote__echo", json!({"text": "over http"})).await;
    assert_eq!(
        (r.content.as_str(), r.is_error),
        ("over http", false),
        "{}",
        r.content
    );
}

#[tokio::test]
async fn annotations_map_to_effects_and_config_overrides_them() {
    let (secrets, _tmp) = secrets(&[]);
    let effects_of = |set: &McpServers| -> Vec<(String, Effect)> {
        set.tools()
            .iter()
            .map(|t| (t.spec().name, t.effect()))
            .collect()
    };
    let set = McpServers::new(vec![McpServer::new(
        "fx",
        stdio_cfg(vec![]),
        secrets.clone(),
    )]);
    set.prepare().await;
    let mut got = effects_of(&set);
    got.sort();
    assert_eq!(
        got,
        [
            ("mcp__fx__add".to_owned(), Effect::Write), // no annotations
            ("mcp__fx__crash".to_owned(), Effect::Write), // destructiveHint false
            ("mcp__fx__echo".to_owned(), Effect::Read), // readOnlyHint
            ("mcp__fx__env".to_owned(), Effect::Write),
            ("mcp__fx__fail".to_owned(), Effect::Destructive), // destructiveHint
        ]
    );

    let mut cfg = stdio_cfg(vec![]);
    cfg.effects.insert("echo".into(), Effect::Destructive);
    cfg.effects.insert("*".into(), Effect::Read);
    let set = McpServers::new(vec![McpServer::new("fx", cfg, secrets)]);
    set.prepare().await;
    let mut got = effects_of(&set);
    got.sort();
    assert_eq!(
        got.iter().map(|(_, e)| *e).collect::<Vec<_>>(),
        [
            Effect::Read,
            Effect::Read,
            Effect::Destructive, // by name, over the hint and "*"
            Effect::Read,
            Effect::Read, // "*" over the destructive hint
        ]
    );
    let echo = set
        .tools()
        .into_iter()
        .find(|t| t.spec().name == "mcp__fx__echo")
        .unwrap();
    assert_eq!(echo.policy(&ctx(), &json!({})).effect, Effect::Destructive);
}
