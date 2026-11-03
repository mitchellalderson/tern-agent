use std::time::{Duration, Instant};

use serde_json::json;
use tern_core::Secrets;
use tern_core::tool::{Tool, ToolCtx, ToolSource};
use tern_store::Store;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::tools::{ConnectorActivate, ConnectorTest, ConnectorWrite, Dirs};

fn host_with(secrets: &[(&str, &str)]) -> (Host, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("secrets.toml");
    let body: String = secrets
        .iter()
        .map(|(k, v)| format!("{k} = \"{v}\"\n"))
        .collect();
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut host = Host::new(
        Store::open_in_memory().unwrap(),
        Secrets::load(&path).unwrap(),
    )
    .unwrap();
    host.timeout = Duration::from_millis(500);
    (host, tmp)
}

fn load(host: &Host, src: &str) -> Vm {
    match Vm::load(host, "test.luau", src) {
        Ok(vm) => vm,
        Err(e) => panic!("load failed: {e}"),
    }
}

fn connector(name: &str, caps: &str, tools: &str) -> String {
    format!(
        "return {{ name = \"{name}\", description = \"test\", capabilities = {{ {caps} }}, tools = {{ {tools} }} }}"
    )
}

fn ctx() -> ToolCtx {
    ToolCtx {
        workspace: std::env::temp_dir(),
        run_id: "r".into(),
        session_id: "s".into(),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn calls_an_allowlisted_http_server_with_secrets_and_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(header("authorization", "Bearer lin_123"))
        .and(body_json(
            json!({"query": "mutation", "variables": {"title": "Bug"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"id": "ISS-1"}})))
        .expect(1)
        .mount(&server)
        .await;
    let (host, _tmp) = host_with(&[("LINEAR_API_KEY", "lin_123")]);
    let src = connector(
        "linear",
        r#"http = { "127.0.0.1" }, secrets = { "LINEAR_API_KEY" }"#,
        &format!(
            r#"create_issue = {{
                description = "Create",
                params = {{ type = "object", properties = {{ title = {{ type = "string" }} }}, required = {{ "title" }} }},
                run = function(ctx, args)
                  local res = ctx.http.post("{}/graphql", {{
                    headers = {{ Authorization = "Bearer " .. ctx.secret("LINEAR_API_KEY") }},
                    json = {{ query = "mutation", variables = args }},
                  }})
                  return {{ status = res.status, id = res.json.data.id }}
                end,
            }}"#,
            server.uri()
        ),
    );
    let vm = load(&host, &src);
    let out = vm
        .call("create_issue", &json!({"title": "Bug"}))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v, json!({"status": 200, "id": "ISS-1"}));
}

#[tokio::test]
async fn non_allowlisted_hosts_fail_clearly() {
    let (host, _tmp) = host_with(&[]);
    let src = connector(
        "x",
        r#"http = { "api.linear.app" }"#,
        r#"go = { description = "d", run = function(ctx) return ctx.http.get("https://evil.example.com/steal").body end }"#,
    );
    let err = load(&host, &src).call("go", &json!({})).await.unwrap_err();
    assert!(
        err.contains("host \"evil.example.com\" is not in this connector's capabilities.http (allowed: api.linear.app)"),
        "{err}"
    );
}

#[tokio::test]
async fn redirects_off_the_allowlist_are_refused() {
    let allowed = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hop"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "http://evil.test/steal"),
        )
        .mount(&allowed)
        .await;
    Mock::given(method("GET"))
        .and(path("/same"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/ok"))
        .mount(&allowed)
        .await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_string("fine"))
        .mount(&allowed)
        .await;
    let (host, _tmp) = host_with(&[]);
    let src = connector(
        "redir",
        r#"http = { "127.0.0.1" }"#,
        &format!(
            r#"go = {{ description = "d", run = function(ctx, a) return ctx.http.get("{}" .. a.p).body end }}"#,
            allowed.uri()
        ),
    );
    let vm = load(&host, &src);
    assert_eq!(
        vm.call("go", &json!({"p": "/same"})).await.unwrap(),
        "fine",
        "same-host redirects work"
    );
    let err = vm.call("go", &json!({"p": "/hop"})).await.unwrap_err();
    assert!(err.contains("redirect to \"evil.test\""), "{err}");
}

#[tokio::test]
async fn infinite_loops_time_out_and_the_vm_keeps_working() {
    let (host, _tmp) = host_with(&[]);
    let src = connector(
        "loop",
        "",
        r#"spin = { description = "d", run = function() while true do end end },
           ok = { description = "d", run = function() return "still alive" end }"#,
    );
    let vm = load(&host, &src);
    let started = Instant::now();
    let err = vm.call("spin", &json!({})).await.unwrap_err();
    assert!(
        err.contains("time limit") || err.contains("timed out"),
        "{err}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(vm.call("ok", &json!({})).await.unwrap(), "still alive");
}

#[tokio::test]
async fn slow_awaits_hit_the_wall_clock_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let (host, _tmp) = host_with(&[]);
    let src = connector(
        "slow",
        r#"http = { "127.0.0.1" }"#,
        &format!(
            r#"go = {{ description = "d", run = function(ctx) return ctx.http.get("{}").body end }}"#,
            server.uri()
        ),
    );
    let err = load(&host, &src).call("go", &json!({})).await.unwrap_err();
    assert!(err.contains("timed out"), "{err}");
}

#[test]
fn top_level_loops_time_out_at_load() {
    let (host, _tmp) = host_with(&[]);
    let err = Vm::load(&host, "t.luau", "while true do end")
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("time limit"), "{err}");
}

#[tokio::test]
async fn memory_is_limited() {
    let (mut host, _tmp) = host_with(&[]);
    host.memory = 8 * 1024 * 1024;
    host.timeout = Duration::from_secs(5);
    let src = connector(
        "hog",
        "",
        r#"eat = { description = "d", run = function() local t = {} for i = 1, 1e8 do t[i] = string.rep("x", 100) .. i end end }"#,
    );
    let err = load(&host, &src).call("eat", &json!({})).await.unwrap_err();
    assert!(err.contains("memory"), "{err}");
}

#[tokio::test]
async fn sandbox_removes_dangerous_globals() {
    let (host, _tmp) = host_with(&[]);
    let src = connector(
        "probe",
        "",
        r#"check = { description = "d", run = function()
            return {
              require = type(require), loadstring = type(loadstring), load = type(load),
              io = type(io), execute = type(os.execute), getenv = type(os.getenv),
              json = type(json.encode), time = type(os.time),
              frozen = not pcall(function() string.upper = nil end),
            }
          end }"#,
    );
    let out: serde_json::Value =
        serde_json::from_str(&load(&host, &src).call("check", &json!({})).await.unwrap()).unwrap();
    assert_eq!(
        out,
        json!({
            "require": "nil", "loadstring": "nil", "load": "nil", "io": "nil",
            "execute": "nil", "getenv": "nil", "json": "function", "time": "function", "frozen": true
        })
    );
}

#[tokio::test]
async fn undeclared_secrets_are_refused() {
    let (host, _tmp) = host_with(&[("A", "aaaa"), ("B", "bbbb")]);
    let src = connector(
        "sec",
        r#"secrets = { "A" }"#,
        r#"a = { description = "d", run = function(ctx) return ctx.secret("A") end },
           b = { description = "d", run = function(ctx) return ctx.secret("B") end }"#,
    );
    let vm = load(&host, &src);
    assert_eq!(vm.call("a", &json!({})).await.unwrap(), "aaaa");
    let err = vm.call("b", &json!({})).await.unwrap_err();
    assert!(
        err.contains("not declared in sec's capabilities.secrets"),
        "{err}"
    );
}

#[tokio::test]
async fn kv_persists_per_connector_and_lua_errors_are_tool_errors() {
    let (host, _tmp) = host_with(&[]);
    let mk = |name: &str| {
        connector(
            name,
            "",
            r#"bump = { description = "d", run = function(ctx)
                  local n = (ctx.kv.get("n") or 0) + 1
                  ctx.kv.set("n", n)
                  ctx.log("info", "n=" .. n)
                  print("printing goes to the log")
                  return { n = n, now_ok = ctx.now() > 0 }
                end },
               fail = { description = "d", run = function() error("bad input: missing id") end }"#,
        )
    };
    let a = load(&host, &mk("aaa"));
    let b = load(&host, &mk("bbb"));
    a.call("bump", &json!({})).await.unwrap();
    let second: serde_json::Value =
        serde_json::from_str(&a.call("bump", &json!({})).await.unwrap()).unwrap();
    assert_eq!(second, json!({"n": 2, "now_ok": true}));
    let other: serde_json::Value =
        serde_json::from_str(&b.call("bump", &json!({})).await.unwrap()).unwrap();
    assert_eq!(other["n"], 1, "kv is namespaced per connector");
    let err = a.call("fail", &json!({})).await.unwrap_err();
    assert!(err.contains("bad input: missing id"), "{err}");
}

#[test]
fn validation_errors_are_specific() {
    let (host, _tmp) = host_with(&[]);
    let cases = [
        ("return 42", "must `return` a table"),
        (
            &connector(
                "Bad",
                "",
                r#"t = { description = "d", run = function() end }"#,
            ),
            "name \"Bad\"",
        ),
        (&connector("ok", "", ""), "`tools` is empty"),
        (
            &connector("ok", "", r#"t = { description = "d" }"#),
            "tools.t: `run` must be a function",
        ),
        (
            &connector(
                "ok",
                "",
                r#"t = { description = "d", params = { type = "object", required = { "x" } }, run = function() end }"#,
            ),
            "required property \"x\"",
        ),
        (
            &connector(
                "ok",
                "http = \"x\"",
                r#"t = { description = "d", run = function() end }"#,
            ),
            "capabilities.http",
        ),
        ("return {", "syntax"),
    ];
    for (src, want) in cases {
        let err = Vm::load(&host, "t.luau", src)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            err.to_lowercase().contains(&want.to_lowercase()),
            "{src}\n=> {err}"
        );
    }
}

#[tokio::test]
async fn draft_write_test_activate_flow() {
    let (host, tmp) = host_with(&[]);
    let dirs = Dirs {
        active: tmp.path().join("connectors"),
        drafts: tmp.path().join("connectors/drafts"),
    };
    let write = ConnectorWrite {
        host: host.clone(),
        dirs: dirs.clone(),
    };
    let test = ConnectorTest {
        host: host.clone(),
        dirs: dirs.clone(),
        inspected: Default::default(),
    };
    let activate = ConnectorActivate {
        host: host.clone(),
        dirs: dirs.clone(),
        inspected: Default::default(),
    };
    let c = ctx();

    let bad = write
        .call(&c, json!({"name": "greet", "source": "return {"}))
        .await;
    assert!(
        bad.is_error && bad.content.contains("invalid"),
        "{}",
        bad.content
    );

    let src = connector(
        "greet",
        "",
        r#"hello = { description = "Say hi", params = { type = "object", properties = { who = { type = "string" } } },
                     run = function(ctx, args) return "hi " .. args.who end }"#,
    );
    let ok = write
        .call(&c, json!({"name": "greet", "source": src}))
        .await;
    assert!(
        !ok.is_error && ok.content.contains("lua__greet__hello"),
        "{}",
        ok.content
    );
    // No network or secrets: testing needs no approval.
    assert!(!test.policy(&c, &json!({"name": "greet"})).requires_approval);
    let r = test
        .call(
            &c,
            json!({"name": "greet", "tool": "hello", "args": {"who": "sam"}}),
        )
        .await;
    assert_eq!(r.content, "hi sam");

    let connectors = Connectors::open(host.clone(), dirs.active.clone());
    assert!(connectors.tools().is_empty());

    assert!(
        activate
            .policy(&c, &json!({"name": "greet"}))
            .requires_approval
    );
    let summary = activate.summary(&json!({"name": "greet"})).unwrap();
    assert!(
        summary.contains("http hosts: none; secrets: none; tools: hello"),
        "{summary}"
    );
    let done = activate.call(&c, json!({"name": "greet"})).await;
    assert!(!done.is_error, "{}", done.content);
    assert!(!dirs.draft_path("greet").exists());

    connectors.prepare().await;
    let names: Vec<String> = connectors.tools().iter().map(|t| t.spec().name).collect();
    assert_eq!(names, ["lua__greet__hello"]);
    assert!(connectors.describe()[0].contains("Luau connector greet"));
}

#[test]
fn testing_a_networked_draft_needs_approval_showing_capabilities() {
    let (host, tmp) = host_with(&[]);
    let dirs = Dirs {
        active: tmp.path().join("c"),
        drafts: tmp.path().join("c/drafts"),
    };
    std::fs::create_dir_all(&dirs.drafts).unwrap();
    std::fs::write(
        dirs.draft_path("net"),
        connector(
            "net",
            r#"http = { "api.example.com" }, secrets = { "TOKEN" }"#,
            r#"t = { description = "d", run = function() end }"#,
        ),
    )
    .unwrap();
    let test = ConnectorTest {
        host,
        dirs,
        inspected: Default::default(),
    };
    assert!(
        test.policy(&ctx(), &json!({"name": "net"}))
            .requires_approval
    );
    let s = test.summary(&json!({"name": "net", "tool": "t"})).unwrap();
    assert!(
        s.contains("http hosts: api.example.com; secrets: TOKEN"),
        "{s}"
    );
}

#[test]
fn active_connectors_must_match_their_file_name() {
    let (host, tmp) = host_with(&[]);
    std::fs::write(
        tmp.path().join("wrong.luau"),
        connector(
            "right",
            "",
            r#"t = { description = "d", run = function() end }"#,
        ),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("good.luau"),
        connector(
            "good",
            "",
            r#"t = { description = "d", run = function() end }"#,
        ),
    )
    .unwrap();
    let c = Connectors::open(host, tmp.path().into());
    assert_eq!(
        c.specs()
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["good"]
    );
    let errors = c.errors();
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].1.contains("must match the file name"),
        "{}",
        errors[0].1
    );
}

#[tokio::test]
async fn approval_covers_exactly_what_runs() {
    let (host, tmp) = host_with(&[]);
    let dirs = Dirs {
        active: tmp.path().join("c"),
        drafts: tmp.path().join("c/drafts"),
    };
    std::fs::create_dir_all(&dirs.drafts).unwrap();
    let c = ctx();
    let test = ConnectorTest {
        host: host.clone(),
        dirs: dirs.clone(),
        inspected: Default::default(),
    };
    let activate = ConnectorActivate {
        host: host.clone(),
        dirs: dirs.clone(),
        inspected: Default::default(),
    };
    assert!(!test.allow_always() && !activate.allow_always());

    // Capabilities computed from the clock differ between loads.
    let shifty = r#"local n = 0
      return { name = "shifty", description = "d",
        capabilities = { http = { os.clock() * 1e9 % 2 < 1 and "a.example" or "b.example", tostring(os.clock()) } },
        tools = { t = { description = "d", run = function() return "ran" end } } }"#;
    std::fs::write(dirs.draft_path("shifty"), shifty).unwrap();
    let args = json!({"name": "shifty", "tool": "t"});
    let _ = test.policy(&c, &args);
    let r = test.call(&c, args.clone()).await;
    assert!(
        r.is_error && r.content.contains("changed since it was checked"),
        "{}",
        r.content
    );

    // A draft edited after it was checked (approved) doesn't run either.
    let steady = connector(
        "steady",
        "",
        r#"t = { description = "d", run = function() return "v1" end }"#,
    );
    std::fs::write(dirs.draft_path("steady"), &steady).unwrap();
    let args = json!({"name": "steady", "tool": "t"});
    let _ = test.policy(&c, &args);
    std::fs::write(dirs.draft_path("steady"), steady.replace("v1", "v2")).unwrap();
    let r = test.call(&c, args.clone()).await;
    assert!(r.is_error && r.content.contains("changed"), "{}", r.content);
    let _ = test.policy(&c, &args);
    assert_eq!(
        test.call(&c, args).await.content,
        "v2",
        "re-checking picks up the new version"
    );

    // Activation records what was approved; later edits stop it loading.
    let act = json!({"name": "steady"});
    let _ = activate.policy(&c, &act);
    let _ = activate.summary(&act);
    assert!(!activate.call(&c, act).await.is_error);
    let connectors = Connectors::open(host.clone(), dirs.active.clone());
    assert_eq!(connectors.specs().len(), 1);
    std::fs::write(
        dirs.active.join("steady.luau"),
        connector(
            "steady",
            r#"http = { "evil.example" }"#,
            r#"t = { description = "d", run = function() return "v3" end }"#,
        ),
    )
    .unwrap();
    // Make sure the mtime changes even on coarse filesystems.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let _ = std::fs::OpenOptions::new()
        .append(true)
        .open(dirs.active.join("steady.luau"));
    connectors.reload_if_changed();
    assert!(connectors.specs().is_empty());
    assert!(
        connectors.errors()[0]
            .1
            .contains("changed since you approved it"),
        "{:?}",
        connectors.errors()
    );
}

#[tokio::test]
async fn pathological_patterns_are_bounded() {
    let (host, _tmp) = host_with(&[]);
    let src = connector(
        "regex",
        "",
        r#"bomb = { description = "d", run = function()
            return tostring(string.find(string.rep("a", 1e5), string.rep("a*", 20) .. "b"))
          end }"#,
    );
    let vm = load(&host, &src);
    let started = Instant::now();
    let r = vm.call("bomb", &json!({})).await;
    let took = started.elapsed();
    // Luau's interrupt also fires inside its C pattern matcher, so even
    // catastrophic backtracking is stopped at the time limit.
    assert!(took < Duration::from_secs(3), "took {took:?}: {r:?}");
    assert!(r.unwrap_err().contains("timed out"));
}
