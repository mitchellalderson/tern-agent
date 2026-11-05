//! MCP client against an independent (Python, stdlib-only) server.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tern_core::Secrets;
use tern_core::config::{McpServerConfig, McpTransport};
use tern_core::mcp::{McpErrorKind, McpState};
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
        enabled: true,
        command: Some("python3".into()),
        args: vec![fixture()],
        env_secrets,
        ..Default::default()
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
    let set = Arc::new(McpServers::new(set.servers()));
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
        // Long enough that a stalled CI box can't outlast it before the
        // "in backoff" check below.
        Duration::from_secs(2),
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
    // Retry until the backoff has run out.
    let end = std::time::Instant::now() + Duration::from_secs(20);
    let after = loop {
        let r = call(&set, "mcp__fixture__echo", json!({"text": "back"})).await;
        if !r.is_error || std::time::Instant::now() > end {
            break r;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
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
        enabled: true,
        url: Some(format!("http://127.0.0.1:{port}/mcp")),
        idempotent: true,
        requires_approval: true,
        ..Default::default()
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

async fn pid(set: &McpServers, server: &str) -> u32 {
    let r = call(set, &format!("mcp__{server}__env"), json!({"name": "$$"})).await;
    r.content
        .parse()
        .unwrap_or_else(|_| panic!("{}", r.content))
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn apply_keeps_unchanged_servers_and_kills_removed_ones() {
    let (secrets, _tmp) = secrets(&[]);
    let seen = Arc::new(std::sync::Mutex::new(
        Vec::<(String, Option<McpState>)>::new(),
    ));
    let log = seen.clone();
    let notify: tern_mcp::Notify = Arc::new(move |name, state| {
        log.lock().unwrap().push((name.to_owned(), state));
    });
    let mut cfg = BTreeMap::from([
        ("a".to_owned(), stdio_cfg(vec![])),
        ("b".to_owned(), stdio_cfg(vec![])),
    ]);
    let set = McpServers::from_config(&cfg, secrets, Some(notify));
    set.prepare().await;
    let (a, b) = (pid(&set, "a").await, pid(&set, "b").await);

    // Change b only: a keeps its process, b gets a new one.
    cfg.get_mut("b").unwrap().requires_approval = true;
    set.apply(&cfg);
    set.prepare().await;
    assert_eq!(
        pid(&set, "a").await,
        a,
        "unchanged server kept its connection"
    );
    let b2 = pid(&set, "b").await;
    assert_ne!(b2, b);

    // Remove b: its process goes away.
    cfg.remove("b");
    set.apply(&cfg);
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while alive(b2) {
        assert!(
            std::time::Instant::now() < end,
            "removed server still running"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(set.status("b").is_none());
    assert!(alive(a));
    let seen = seen.lock().unwrap();
    assert!(seen.contains(&("b".to_owned(), None)), "{seen:?}");
    assert!(
        seen.iter()
            .any(|(n, s)| n == "a" && matches!(s, Some(McpState::Running { tools: 5, .. }))),
        "{seen:?}"
    );
}

#[tokio::test]
async fn disabled_servers_and_tools_are_hidden() {
    let (secrets, _tmp) = secrets(&[]);
    let mut on = stdio_cfg(vec![]);
    on.disabled_tools = vec!["crash".into(), "fail".into()];
    let mut off = stdio_cfg(vec![]);
    off.enabled = false;
    let set = McpServers::from_config(
        &BTreeMap::from([("on".to_owned(), on), ("off".to_owned(), off)]),
        secrets,
        None,
    );
    set.prepare().await;
    let mut names: Vec<String> = set.tools().iter().map(|t| t.spec().name).collect();
    names.sort();
    assert_eq!(names, ["mcp__on__add", "mcp__on__echo", "mcp__on__env"]);
    let on = set.status("on").unwrap();
    assert!(
        matches!(on.state, McpState::Running { tools: 3, .. }),
        "{:?}",
        on.state
    );
    assert_eq!(on.tools.iter().filter(|t| t.disabled).count(), 2);
    assert_eq!(on.server_info.as_deref(), Some("fixture 1.0"));
    assert!(
        on.stderr_tail
            .iter()
            .any(|l| l.contains("fixture server starting"))
    );
    assert_eq!(set.status("off").unwrap().state, McpState::Disabled);
    assert!(set.describe().iter().all(|d| !d.contains("server off")));
    assert!(set.restart("off").await.is_err());
}

#[tokio::test]
async fn verify_reports_tools_and_startup_errors() {
    let (secrets, _tmp) = secrets(&[]);
    let ok = tern_mcp::verify("fx", &stdio_cfg(vec![]), secrets.clone(), BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(ok.tools.len(), 5);
    assert_eq!(ok.server_info.as_deref(), Some("fixture 1.0"));

    let mut crashing = stdio_cfg(vec![]);
    crashing.args.extend([
        "--fail-start".into(),
        "Error: GITHUB_TOKEN is invalid".into(),
    ]);
    let e = tern_mcp::verify("fx", &crashing, secrets.clone(), BTreeMap::new())
        .await
        .unwrap_err();
    assert_eq!(e.kind, McpErrorKind::ExitedEarly, "{e:?}");
    assert!(
        e.message.contains("GITHUB_TOKEN is invalid"),
        "{}",
        e.message
    );
    assert_eq!(e.stderr_tail, ["Error: GITHUB_TOKEN is invalid"]);

    let mut ghost = stdio_cfg(vec![]);
    ghost.command = Some("definitely-not-a-real-mcp-server".into());
    let e = tern_mcp::verify("fx", &ghost, secrets.clone(), BTreeMap::new())
        .await
        .unwrap_err();
    assert_eq!(e.kind, McpErrorKind::CommandNotFound);

    let needs = stdio_cfg(vec!["FIXTURE_TOKEN".into()]);
    let e = tern_mcp::verify("fx", &needs, secrets.clone(), BTreeMap::new())
        .await
        .unwrap_err();
    assert_eq!(e.kind, McpErrorKind::MissingSecret);
    // A value typed in the form but not saved yet is enough.
    let typed = BTreeMap::from([("FIXTURE_TOKEN".to_owned(), "typed".to_owned())]);
    assert!(tern_mcp::verify("fx", &needs, secrets, typed).await.is_ok());
}

#[tokio::test]
async fn http_bearer_tokens_come_from_secrets() {
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut server = Server(
        std::process::Command::new("python3")
            .args([
                fixture(),
                "--http".into(),
                "--token".into(),
                "s3cret".into(),
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    std::io::BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line.trim().parse().unwrap();
    let cfg = McpServerConfig {
        transport: McpTransport::Http,
        enabled: true,
        url: Some(format!("http://127.0.0.1:{port}/mcp")),
        bearer_token_secret: Some("FX_TOKEN".into()),
        ..Default::default()
    };
    let (secrets, _tmp) = secrets(&[("FX_TOKEN", "wrong")]);
    let e = tern_mcp::verify("fx", &cfg, secrets.clone(), BTreeMap::new())
        .await
        .unwrap_err();
    assert_eq!(e.kind, McpErrorKind::Unauthorized, "{e:?}");
    let right = BTreeMap::from([("FX_TOKEN".to_owned(), "s3cret".to_owned())]);
    let ok = tern_mcp::verify("fx", &cfg, secrets, right).await.unwrap();
    assert_eq!(ok.tools.len(), 5);
}

#[tokio::test]
async fn two_accounts_get_their_own_process_and_secrets() {
    use tern_auth::{ConnAuth, Connections, Guard, Kind, Tokens};
    let (secrets, _tmp) = secrets(&[
        ("FIXTURE_TOKEN", "tok-default"),
        ("\"conn/mcp:fixture/side/FIXTURE_TOKEN\"", "tok-side"),
    ]);
    let tokens = Arc::new(Tokens::new((*secrets).clone(), Guard::offline()));
    let conns = Arc::new(Connections::new(
        tern_store::Store::open_in_memory().unwrap(),
        tokens,
        10,
    ));
    let key = ConnAuth::Key {
        names: vec!["FIXTURE_TOKEN".into()],
    };
    conns
        .add(Kind::Mcp, "fixture", "default", None, Some("me@work"), &key)
        .unwrap();
    conns
        .add(Kind::Mcp, "fixture", "side", None, Some("me@side"), &key)
        .unwrap();
    let set = McpServers::new(vec![McpServer::new(
        "fixture",
        stdio_cfg(vec!["FIXTURE_TOKEN".into()]),
        secrets,
    )])
    .with_connections(conns.clone(), Duration::from_millis(50));
    set.prepare().await;
    let tool = |name: &str| {
        set.tools()
            .into_iter()
            .find(|t| t.spec().name == name)
            .unwrap()
    };
    let env = tool("mcp__fixture__env");
    assert_eq!(
        env.spec().input_schema["properties"]["connection"]["enum"],
        json!(["default", "side"])
    );
    assert!(set.describe()[0].contains("accounts: default (me@work, default), side (me@side)"));
    let run = |args: serde_json::Value| {
        let env = env.clone();
        async move { env.call(&ctx(), args).await }
    };
    let side = run(json!({"name": "FIXTURE_TOKEN", "connection": "side"})).await;
    assert_eq!(side.content, "tok-side");
    let default = run(json!({"name": "FIXTURE_TOKEN", "connection": "default"})).await;
    assert_eq!(default.content, "tok-default");
    // `env` is a write (no annotations): with two accounts it must say which.
    let none = run(json!({"name": "FIXTURE_TOKEN"})).await;
    assert!(
        none.is_error && none.content.contains("must say which"),
        "{}",
        none.content
    );
    assert_eq!(
        env.summary(&json!({"name": "X", "connection": "side"}))
            .as_deref(),
        Some("mcp__fixture__env as fixture · side (me@side)")
    );
    // A read uses the default.
    let echo = tool("mcp__fixture__echo")
        .call(&ctx(), json!({"text": "hi"}))
        .await;
    assert_eq!(echo.content, "hi");
    // One process per account; the extra one stops when idle.
    let side_pid: u32 = run(json!({"name": "$$", "connection": "side"}))
        .await
        .content
        .parse()
        .unwrap();
    let default_pid: u32 = run(json!({"name": "$$", "connection": "default"}))
        .await
        .content
        .parse()
        .unwrap();
    assert_ne!(side_pid, default_pid);
    tokio::time::sleep(Duration::from_millis(100)).await;
    set.prepare().await;
    for _ in 0..50 {
        if !alive(side_pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(side_pid), "the idle account's server stopped");
    assert!(alive(default_pid), "the default account's stays");
    // And starts again on its next use.
    let again = run(json!({"name": "FIXTURE_TOKEN", "connection": "side"})).await;
    assert_eq!(again.content, "tok-side");
}

#[tokio::test]
async fn an_oauth_account_signs_in_to_an_http_server_and_refreshes_on_401() {
    use tern_auth::{ConnAuth, Connections, Guard, Kind, OAuthConfig, TokenSet, Tokens};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut server = Server(
        std::process::Command::new("python3")
            .args([
                fixture(),
                "--http".into(),
                "--token".into(),
                "AT-fresh-1".into(),
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    std::io::BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line.trim().parse().unwrap();
    let idp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=RT-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "AT-fresh-1", "token_type": "Bearer", "expires_in": 3600
        })))
        .expect(1)
        .mount(&idp)
        .await;
    let (secrets, _tmp) = secrets(&[]);
    let tokens = Arc::new(Tokens::new((*secrets).clone(), Guard::new(&[idp.uri()])));
    let conns = Arc::new(Connections::new(
        tern_store::Store::open_in_memory().unwrap(),
        tokens.clone(),
        10,
    ));
    let cfg = OAuthConfig {
        provider: "fx".into(),
        authorize_url: format!("{}/authorize", idp.uri()),
        token_url: format!("{}/token", idp.uri()),
        client_id: "tern".into(),
        ..OAuthConfig::default()
    };
    let work = conns
        .add(
            Kind::Mcp,
            "fx",
            "work",
            None,
            Some("me@fx"),
            &ConnAuth::OAuth(Box::new(cfg)),
        )
        .unwrap();
    // The stored token is stale at the server.
    tokens
        .save(
            &work.key(),
            &TokenSet {
                access_token: "AT-stale-0".into(),
                refresh_token: Some("RT-1".into()),
                expires_at: None,
                scope: None,
                id_token: None,
            },
        )
        .unwrap();
    let set = McpServers::new(vec![McpServer::new(
        "fx",
        McpServerConfig {
            transport: McpTransport::Http,
            enabled: true,
            url: Some(format!("http://127.0.0.1:{port}/mcp")),
            ..Default::default()
        },
        secrets.clone(),
    )])
    .with_connections(conns, Duration::from_secs(600));
    set.prepare().await;
    let echo = set
        .tools()
        .into_iter()
        .find(|t| t.spec().name == "mcp__fx__echo")
        .expect("listed through the work account");
    assert_eq!(echo.call(&ctx(), json!({"text": "hi"})).await.content, "hi");
    assert_eq!(
        secrets
            .get("conn/mcp:fx/work/oauth/access_token")
            .as_deref(),
        Some("AT-fresh-1")
    );
    assert_eq!(secrets.redact("Bearer AT-fresh-1"), "Bearer [REDACTED]");
}
