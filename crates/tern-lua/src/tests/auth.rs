//! T4b-i: host-injected credentials, path allowlists and rate limits.

use wiremock::matchers::query_param;
use wiremock::{Request, Respond};

use super::*;

const TOKEN: &str = "ntn_secret_value_123";

/// Answers with everything the request carried, in the body and in a
/// header: the deliberate echo endpoint of the spec's T4b acceptance.
struct Echo;

impl Respond for Echo {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let headers: serde_json::Map<String, serde_json::Value> = req
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().into()))
            .collect();
        let auth = req
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("none")
            .to_owned();
        ResponseTemplate::new(200)
            .insert_header("x-echo-auth", auth.as_str())
            .set_body_json(json!({"url": req.url.to_string(), "headers": headers}))
    }
}

async fn echo_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(Echo)
        .mount(&server)
        .await;
    server
}

/// Calls `go` and fails the test if the token shows up anywhere the script
/// could see.
async fn run_clean(vm: &Vm, args: serde_json::Value) -> String {
    let out = match vm.call("go", &args).await {
        Ok(o) | Err(o) => o,
    };
    assert!(!out.contains(TOKEN), "the script saw the token: {out}");
    out
}

#[tokio::test]
async fn injected_auth_reaches_the_server_and_never_the_script() {
    let server = echo_server().await;
    let (host, _tmp) = host_with(&[("NOTION_TOKEN", TOKEN)]);
    let src = connector(
        "notion",
        r#"http = { "127.0.0.1" }, auth = { secret = "NOTION_TOKEN" }"#,
        &format!(
            r#"go = {{ description = "d", run = function(ctx)
                local seen = {{}}
                -- Everything ctx offers, printed and collected.
                for k, v in pairs(ctx) do
                  print(k, v)
                  table.insert(seen, k .. "=" .. tostring(v))
                end
                local ok, err = pcall(ctx.secret, "NOTION_TOKEN")
                local res = ctx.http.get("{}/v1/pages")
                print(res.body)
                return {{ seen = seen, secret_err = tostring(err), ok = ok,
                         body = res.body, echoed = res.headers["x-echo-auth"] }}
              end }}"#,
            server.uri()
        ),
    );
    let vm = load(&host, &src);
    let out = run_clean(&vm, json!({})).await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], false);
    assert!(
        v["secret_err"]
            .as_str()
            .unwrap()
            .contains("sent by tern itself (capabilities.auth)"),
        "{v}"
    );
    assert_eq!(v["echoed"], "Bearer [REDACTED]", "{v}");
    assert!(v["body"].as_str().unwrap().contains("Bearer [REDACTED]"));

    // The server did get the real value.
    let got = server.received_requests().await.unwrap();
    assert_eq!(
        got[0].headers.get("authorization").unwrap(),
        &format!("Bearer {TOKEN}")
    );
}

#[tokio::test]
async fn header_query_and_basic_forms() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/header"))
        .and(header("x-api-key", TOKEN))
        .respond_with(ResponseTemplate::new(200).set_body_string("header ok"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/query"))
        .and(query_param("api_key", "a b&c=d"))
        .respond_with(Echo)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/basic"))
        // base64("jira@example.com:" + TOKEN)
        .and(header(
            "authorization",
            "Basic amlyYUBleGFtcGxlLmNvbTpudG5fc2VjcmV0X3ZhbHVlXzEyMw==",
        ))
        .respond_with(Echo)
        .expect(1)
        .mount(&server)
        .await;
    let (host, _tmp) = host_with(&[
        ("API_KEY", TOKEN),
        ("Q_KEY", "a b&c=d"),
        ("JIRA_EMAIL", "jira@example.com"),
        ("JIRA_TOKEN", TOKEN),
    ]);
    let tool = |name: &str, caps: &str, p: &str| {
        connector(
            name,
            caps,
            &format!(
                r#"go = {{ description = "d", run = function(ctx) return ctx.http.get("{}{p}").body end }}"#,
                server.uri()
            ),
        )
    };
    let vm = load(
        &host,
        &tool(
            "h",
            r#"http = { "127.0.0.1" }, auth = { secret = "API_KEY", header = "X-Api-Key" }"#,
            "/header",
        ),
    );
    assert_eq!(run_clean(&vm, json!({})).await, "header ok");

    let vm = load(
        &host,
        &tool(
            "q",
            r#"http = { "127.0.0.1" }, auth = { secret = "Q_KEY", query = "api_key" }"#,
            "/query",
        ),
    );
    let out = run_clean(&vm, json!({})).await;
    assert!(out.contains("api_key=[REDACTED]"), "{out}");
    assert!(!out.contains("a+b%26c%3Dd"), "{out}");

    let vm = load(
        &host,
        &tool(
            "b",
            r#"http = { "127.0.0.1" }, auth = { basic = { user = "JIRA_EMAIL", password = "JIRA_TOKEN" } }"#,
            "/basic",
        ),
    );
    let out = run_clean(&vm, json!({})).await;
    assert!(out.contains("Basic [REDACTED]"), "{out}");
}

#[tokio::test]
async fn auth_goes_only_to_its_hosts_and_when_asked() {
    let server = echo_server().await;
    let (host, _tmp) = host_with(&[("T", TOKEN), ("U", "other_value_456")]);
    let src = connector(
        "scoped",
        r#"http = { "127.0.0.1", "api.example.com" },
           auth = { { secret = "T", hosts = { "api.example.com" } },
                    { secret = "U", header = "X-Other" } }"#,
        &format!(
            r#"go = {{ description = "d", run = function(ctx, a)
                local opts = {{}}
                if a.auth ~= nil then opts.auth = a.auth end
                return ctx.http.get("{}/x", opts).json.headers
              end }}"#,
            server.uri()
        ),
    );
    let vm = load(&host, &src);
    let headers = |out: String| -> serde_json::Value { serde_json::from_str(&out).unwrap() };

    // T is only for api.example.com; U goes to every host.
    let h = headers(run_clean(&vm, json!({})).await);
    assert!(h.get("authorization").is_none(), "{h}");
    assert_eq!(h["x-other"], "[REDACTED]");
    let h = headers(run_clean(&vm, json!({"auth": false})).await);
    assert!(h.get("x-other").is_none(), "{h}");
    let h = headers(run_clean(&vm, json!({"auth": "U"})).await);
    assert_eq!(h["x-other"], "[REDACTED]");
    let err = vm.call("go", &json!({"auth": "NOPE"})).await.unwrap_err();
    assert!(
        err.contains("isn't in this connector's capabilities.auth"),
        "{err}"
    );

    let received = server.received_requests().await.unwrap();
    assert!(
        received
            .iter()
            .all(|r| r.headers.get("authorization").is_none()),
        "T never reached 127.0.0.1"
    );
}

#[tokio::test]
async fn errors_never_carry_the_value() {
    // Nothing listens on port 9; reqwest's error names the URL, which has
    // the key in its query string.
    let (host, _tmp) = host_with(&[("K", TOKEN)]);
    let src = connector(
        "down",
        r#"http = { "127.0.0.1" }, auth = { secret = "K", query = "key" }"#,
        r#"go = { description = "d", run = function(ctx)
             local ok, err = pcall(ctx.http.get, "http://127.0.0.1:9/x")
             print(err)
             error(err)
           end }"#,
    );
    let out = run_clean(&load(&host, &src), json!({})).await;
    assert!(
        out.contains("request to 127.0.0.1 failed") && out.contains("key=[REDACTED]"),
        "{out}"
    );
}

#[tokio::test]
async fn missing_auth_secrets_say_how_to_set_them() {
    let (host, _tmp) = host_with(&[]);
    let src = connector(
        "nokey",
        r#"http = { "127.0.0.1" }, auth = "MISSING_KEY""#,
        r#"go = { description = "d", run = function(ctx) return ctx.http.get("http://127.0.0.1:9/").body end }"#,
    );
    let err = load(&host, &src).call("go", &json!({})).await.unwrap_err();
    assert!(
        err.contains("secret \"MISSING_KEY\" is not set") && err.contains("secret_request"),
        "{err}"
    );
}

#[tokio::test]
async fn path_allowlists_cover_requests_and_redirects() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/items"))
        .respond_with(ResponseTemplate::new(200).set_body_string("items"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/moved"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/v1/items"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/escape"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/admin/keys"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/admin/keys"))
        .respond_with(ResponseTemplate::new(200).set_body_string("leaked"))
        .expect(0)
        .mount(&server)
        .await;
    let (host, _tmp) = host_with(&[("T", TOKEN)]);
    let src = connector(
        "paths",
        r#"http = { "127.0.0.1/v1/*" }, auth = "T""#,
        &format!(
            r#"go = {{ description = "d", run = function(ctx, a) return ctx.http.get("{}" .. a.p).body end }}"#,
            server.uri()
        ),
    );
    let vm = load(&host, &src);
    assert_eq!(run_clean(&vm, json!({"p": "/v1/items"})).await, "items");
    assert_eq!(
        run_clean(&vm, json!({"p": "/v1/moved"})).await,
        "items",
        "a redirect inside the paths is followed"
    );
    let err = run_clean(&vm, json!({"p": "/admin/keys"})).await;
    assert!(
        err.contains("127.0.0.1/admin/keys is outside the paths capabilities.http allows"),
        "{err}"
    );
    let err = run_clean(&vm, json!({"p": "/v1/escape"})).await;
    assert!(
        err.contains("redirect to 127.0.0.1/admin/keys is outside the paths"),
        "{err}"
    );
    let err = run_clean(&vm, json!({"p": "/v1/%2e%2e/admin/keys"})).await;
    assert!(err.contains("outside the paths"), "{err}");

    // Credentials are added afresh on each hop of an allowed redirect.
    let got = server.received_requests().await.unwrap();
    let moved_hops: Vec<_> = got
        .iter()
        .filter(|r| r.url.path() == "/v1/items")
        .map(|r| r.headers.get("authorization").cloned())
        .collect();
    assert!(moved_hops.iter().all(|h| h.is_some()), "{moved_hops:?}");
}

#[tokio::test]
async fn rate_limits_give_a_clean_error_and_respect_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/busy"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120"))
        .mount(&server)
        .await;
    let (host, _tmp) = host_with(&[]);
    let tool = |name: &str, rate: &str| {
        connector(
            name,
            &format!(r#"http = {{ "127.0.0.1" }}, rate = "{rate}""#),
            &format!(
                r#"go = {{ description = "d", run = function(ctx, a) return ctx.http.get("{}" .. a.p).status end }}"#,
                server.uri()
            ),
        )
    };
    let vm = load(&host, &tool("limited", "2/min"));
    for _ in 0..2 {
        assert_eq!(vm.call("go", &json!({"p": "/ok"})).await.unwrap(), "200");
    }
    let err = vm.call("go", &json!({"p": "/ok"})).await.unwrap_err();
    assert!(
        err.starts_with("http: rate limited: connector limited may send 2/min")
            && err.contains("try again in")
            && err.contains("Nothing was sent"),
        "{err}"
    );
    // The budget belongs to the connector, not the VM: reloading it (or a
    // script calling it in a loop through another path) shares it.
    let again = load(&host, &tool("limited", "2/min"));
    assert!(again.call("go", &json!({"p": "/ok"})).await.is_err());
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/ok")
            .count(),
        2
    );

    let vm = load(&host, &tool("busy", "100/min"));
    assert_eq!(vm.call("go", &json!({"p": "/busy"})).await.unwrap(), "429");
    let err = vm.call("go", &json!({"p": "/ok"})).await.unwrap_err();
    assert!(err.contains("asked busy to wait (Retry-After)"), "{err}");
}

#[test]
fn undeclared_rates_use_the_host_default() {
    let (mut host, _tmp) = host_with(&[]);
    host.default_rate = "7/sec".parse().unwrap();
    let vm = load(
        &host,
        &connector(
            "d",
            r#"http = { "api.example.com" }"#,
            r#"go = { description = "d", run = function() end }"#,
        ),
    );
    assert!(
        vm.capability_summary().contains("rate: 7/sec (default)"),
        "{}",
        vm.capability_summary()
    );
}

#[test]
fn auth_validation_errors_are_specific() {
    let (host, _tmp) = host_with(&[]);
    let t = r#"t = { description = "d", run = function() end }"#;
    let cases = [
        (r#"auth = "T""#, "capabilities.auth needs capabilities.http"),
        (
            r#"http = { "a.com" }, auth = { secret = "T", hosts = { "b.com" } }"#,
            "\"b.com\" must be one of the hosts in capabilities.http",
        ),
        (
            r#"http = { "a.com" }, auth = "T", secrets = { "T" }"#,
            "T is in both capabilities.auth and capabilities.secrets",
        ),
        (
            r#"http = { "a.com" }, auth = { secret = "T", hedaer = "X" }"#,
            "unknown key \"hedaer\"",
        ),
        (
            r#"http = { "a.com" }, auth = { secret = "T", query = "k", header = "X" }"#,
            "`query` can't be combined",
        ),
        (
            r#"http = { "a.com" }, auth = { secret = "T", header = "Host" }"#,
            "can't be \"Host\"",
        ),
        (
            r#"http = { "a.com" }, auth = { header = "X" }"#,
            "needs `secret`",
        ),
        (
            r#"http = { "a.com" }, auth = { basic = {} }"#,
            "needs a user or password",
        ),
        (
            r#"http = { "a.com" }, auth = "not a name""#,
            "isn't a valid secret name",
        ),
        (r#"http = { "a.com" }, rate = "lots""#, "\"60/min\""),
        (r#"http = { "a.com/v1/../admin" }"#, "\"..\" segments"),
    ];
    for (caps, want) in cases {
        let err = Vm::load(&host, "t.luau", &connector("c", caps, t))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains(want), "{caps}\n=> {err}");
    }
    // A list of names is a list of Bearer tokens (spec §6.1's form).
    let vm = load(
        &host,
        &connector("c", r#"http = { "a.com" }, auth = { "A", "B" }"#, t),
    );
    assert_eq!(vm.spec.capabilities.auth_secrets(), ["A", "B"]);
}

#[test]
fn legacy_ctx_secret_is_flagged() {
    let (host, tmp) = host_with(&[]);
    let dir = tmp.path().join("connectors");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("old.luau"),
        connector(
            "old",
            r#"http = { "api.old.com" }, secrets = { "OLD_KEY" }"#,
            r#"t = { description = "d", run = function() end }"#,
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("new.luau"),
        connector(
            "new",
            r#"http = { "api.new.com" }, auth = "NEW_KEY""#,
            r#"t = { description = "d", run = function() end }"#,
        ),
    )
    .unwrap();
    let connectors = Connectors::open(host.clone(), dir.clone());
    let specs = connectors.specs();
    let old = specs.iter().find(|s| s.name == "old").unwrap();
    let new = specs.iter().find(|s| s.name == "new").unwrap();
    assert!(old.capabilities.legacy_secrets() && !new.capabilities.legacy_secrets());
    assert!(
        old.capability_summary(host.default_rate)
            .contains("⚠ legacy: the script reads OLD_KEY with ctx.secret")
    );
    assert!(!new.capability_summary(host.default_rate).contains("legacy"));
    let mut uses = connectors.secret_uses();
    uses.sort();
    assert_eq!(
        uses,
        [
            (
                "NEW_KEY".to_owned(),
                "connector new · sent by tern to api.new.com".to_owned()
            ),
            (
                "OLD_KEY".to_owned(),
                "connector old · reads the value itself (legacy ctx.secret)".to_owned()
            ),
        ]
    );
    let mut names = connectors.secret_names();
    names.sort();
    assert_eq!(names, ["NEW_KEY", "OLD_KEY"]);
}

#[tokio::test]
async fn activation_shows_new_capabilities_and_what_changed() {
    let (host, tmp) = host_with(&[]);
    let dirs = Dirs {
        active: tmp.path().join("connectors"),
        drafts: tmp.path().join("connectors/drafts"),
    };
    let write = ConnectorWrite {
        host: host.clone(),
        dirs: dirs.clone(),
    };
    let activate = ConnectorActivate {
        host: host.clone(),
        dirs: dirs.clone(),
        inspected: Default::default(),
    };
    let c = ctx();
    let t = r#"t = { description = "d", effect = "read", run = function() end }"#;
    let v1 = connector(
        "gh",
        r#"http = { "api.github.com/repos/*" }, auth = "GH""#,
        t,
    );
    assert!(
        !write
            .call(&c, json!({"name": "gh", "source": v1}))
            .await
            .is_error
    );
    let s = activate.summary(&json!({"name": "gh"})).unwrap();
    assert!(
        s.contains("http hosts: api.github.com/repos/*")
            && s.contains(
                "auth (tern sends it, the script never sees it): GH as \"Authorization: Bearer …\" to every http host"
            )
            && s.contains("rate: 120/min (default)")
            && !s.contains("Changes"),
        "{s}"
    );
    activate.policy(&c, &json!({"name": "gh"}));
    let done = activate.call(&c, json!({"name": "gh"})).await;
    assert!(!done.is_error, "{}", done.content);
    // The approval record round-trips auth and rate: it still loads.
    assert!(load_active(&host, &dirs.active.join("gh.luau")).is_ok());

    let v2 = connector(
        "gh",
        r#"http = { "api.github.com/repos/*", "api.github.com/search/*" }, auth = "GH", rate = "30/min""#,
        t,
    );
    write.call(&c, json!({"name": "gh", "source": v2})).await;
    let s = activate.summary(&json!({"name": "gh"})).unwrap();
    assert!(
        s.contains("(replacing the active one)")
            && s.contains(
                "Changes from the active one: + http api.github.com/search/*, + rate 30/min, - rate 120/min (default)"
            ),
        "{s}"
    );
    activate.policy(&c, &json!({"name": "gh"}));
    assert!(!activate.call(&c, json!({"name": "gh"})).await.is_error);
    assert!(load_active(&host, &dirs.active.join("gh.luau")).is_ok());

    // Changing the declared auth after approval keeps it from loading.
    std::fs::write(
        dirs.active.join("gh.luau"),
        connector(
            "gh",
            r#"http = { "api.github.com/repos/*", "api.github.com/search/*" }, auth = { secret = "GH", header = "X-Leak" }, rate = "30/min""#,
            t,
        ),
    )
    .unwrap();
    let err = load_active(&host, &dirs.active.join("gh.luau"))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("changed since you approved it"), "{err}");
}
