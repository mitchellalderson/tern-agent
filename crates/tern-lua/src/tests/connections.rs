//! T4a: two accounts of one connector, OAuth tokens sent by the host,
//! refresh on 401, and sign-in-again when the grant is gone.

use std::sync::Arc;

use tern_auth::{ConnAuth, Connections, Guard, Kind, OAuthConfig, TokenSet, Tokens};
use wiremock::matchers::body_string_contains;

use super::*;

const WORK: &str = "AT-work-token-1";
const HOME: &str = "AT-home-token-1";

fn oauth_cfg(server: &MockServer) -> OAuthConfig {
    OAuthConfig {
        provider: "mock".into(),
        authorize_url: format!("{}/authorize", server.uri()),
        token_url: format!("{}/token", server.uri()),
        client_id: "tern".into(),
        ..OAuthConfig::default()
    }
}

/// A host whose connector `gmail` has two OAuth accounts.
async fn two_accounts(server: &MockServer) -> (Host, tempfile::TempDir, Arc<Connections>) {
    let (mut host, tmp) = host_with(&[]);
    let tokens = Arc::new(Tokens::new(
        (*host.secrets).clone(),
        Guard::new(&[server.uri()]),
    ));
    let conns = Arc::new(Connections::new(host.store.clone(), tokens.clone(), 10));
    let auth = ConnAuth::OAuth(Box::new(oauth_cfg(server)));
    for (slug, account, token) in [("work", "me@work.com", WORK), ("home", "me@home.com", HOME)] {
        let c = conns
            .add(Kind::Connector, "gmail", slug, None, Some(account), &auth)
            .unwrap();
        tokens
            .save(
                &c.key(),
                &TokenSet {
                    access_token: token.into(),
                    refresh_token: Some(format!("RT-{slug}-1")),
                    expires_at: None,
                    scope: None,
                    id_token: None,
                },
            )
            .unwrap();
    }
    host.connections = Some(conns.clone());
    (host, tmp, conns)
}

fn gmail(server: &MockServer) -> String {
    connector(
        "gmail",
        r#"http = { "127.0.0.1" }, oauth = { mock = { scopes = { "mail.read", "mail.send" } } }"#,
        &format!(
            r#"inbox = {{ description = "d", effect = "read", run = function(ctx)
                 local n = (ctx.kv.get("seen") or 0) + 1
                 ctx.kv.set("seen", n)
                 local res = ctx.http.get("{uri}/inbox")
                 print(res.body)
                 return {{ who = ctx.connection.slug, account = ctx.connection.account,
                          seen = n, body = res.body, status = res.status }}
               end }},
               send = {{ description = "d", effect = "write", params = {{ type = "object",
                   properties = {{ to = {{ type = "string" }} }} }},
                 run = function(ctx, a)
                   local res = ctx.http.post("{uri}/send", {{ json = {{ to = a.to }} }})
                   return {{ who = ctx.connection.slug, status = res.status, body = res.body }}
                 end }}"#,
            uri = server.uri()
        ),
    )
}

/// Echoes the Authorization header back.
struct WhoAmI;

impl wiremock::Respond for WhoAmI {
    fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
        let auth = req
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("none")
            .to_owned();
        let status = if auth.contains("expired") { 401 } else { 200 };
        ResponseTemplate::new(status).set_body_json(json!({ "auth": auth }))
    }
}

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::path("/inbox"))
        .respond_with(WhoAmI)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::path("/send"))
        .respond_with(WhoAmI)
        .mount(&server)
        .await;
    server
}

fn tool(connectors: &Connectors, name: &str) -> Arc<dyn Tool> {
    connectors
        .tools()
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap()
}

#[tokio::test]
async fn two_accounts_are_routed_and_kept_apart() {
    let server = server().await;
    let (host, tmp, conns) = two_accounts(&server).await;
    let dir = tmp.path().join("c");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("gmail.luau"), gmail(&server)).unwrap();
    let connectors = Connectors::open(host.clone(), dir);
    let inbox = tool(&connectors, "lua__gmail__inbox");
    let send = tool(&connectors, "lua__gmail__send");

    // The host added the account parameter, described with the accounts.
    let schema = send.spec().input_schema;
    assert_eq!(
        schema["properties"]["connection"]["enum"],
        json!(["work", "home"])
    );
    assert!(
        schema["properties"]["connection"]["description"]
            .as_str()
            .unwrap()
            .contains("me@home.com")
    );
    assert!(
        connectors.describe()[0]
            .contains("accounts: work (me@work.com, default), home (me@home.com)")
    );

    // A read without `connection` uses the default; the token reached the
    // server and the script saw only [REDACTED].
    let out = inbox.call(&ctx(), json!({})).await;
    assert!(!out.is_error, "{}", out.content);
    let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(
        (v["who"].as_str(), v["account"].as_str()),
        (Some("work"), Some("me@work.com"))
    );
    assert_eq!(v["body"], json!({"auth": "Bearer [REDACTED]"}).to_string());
    assert!(!out.content.contains(WORK));

    // The other account: its own token, its own kv.
    let out = inbox.call(&ctx(), json!({"connection": "home"})).await;
    let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(
        (v["who"].as_str(), v["seen"].as_i64()),
        (Some("home"), Some(1))
    );
    let out = inbox.call(&ctx(), json!({})).await;
    let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(v["seen"], 2, "work's counter, untouched by home");
    let got: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter_map(|r| {
            r.headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(
        got,
        [
            format!("Bearer {WORK}"),
            format!("Bearer {HOME}"),
            format!("Bearer {WORK}")
        ]
    );

    // A write without `connection` is refused with the options, before
    // anything is sent, and nobody is asked to approve it.
    let args = json!({"to": "x@y.z"});
    assert_eq!(send.policy(&ctx(), &args).effect, Effect::Read);
    let out = send.call(&ctx(), args).await;
    assert!(out.is_error);
    assert!(
        out.content.contains("must say which") && out.content.contains("home (me@home.com)"),
        "{}",
        out.content
    );
    let before = server.received_requests().await.unwrap().len();
    // With one named, it asks as a write, and the card names the account.
    let args = json!({"to": "x@y.z", "connection": "home"});
    assert_eq!(send.policy(&ctx(), &args).effect, Effect::Write);
    assert_eq!(
        send.summary(&args).as_deref(),
        Some("lua__gmail__send as gmail · home (me@home.com)")
    );
    let out = send.call(&ctx(), args).await;
    let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(v["who"], "home");
    assert_eq!(server.received_requests().await.unwrap().len(), before + 1);

    // Removing one leaves the other working, and drops its tokens and kv.
    let home = conns.get(Kind::Connector, "gmail", "home").unwrap();
    conns.remove(&home).await.unwrap();
    assert!(
        host.secrets
            .get("conn/connector:gmail/home/oauth/access_token")
            .is_none()
    );
    assert_eq!(host.store.kv_get("lua:gmail:home", "seen").unwrap(), None);
    let send = tool(&connectors, "lua__gmail__send");
    assert!(
        send.spec().input_schema["properties"]
            .get("connection")
            .is_none(),
        "one account: no parameter"
    );
    let out = send.call(&ctx(), json!({"to": "x@y.z"})).await;
    let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(v["who"], "work");
}

#[tokio::test]
async fn a_refused_token_is_refreshed_once_and_a_dead_grant_asks_to_reconnect() {
    let server = server().await;
    let (host, _tmp, conns) = two_accounts(&server).await;
    // work's token has expired at the service (401), and the refresh works.
    let work = conns.get(Kind::Connector, "gmail", "work").unwrap();
    conns
        .tokens
        .save(
            &work.key(),
            &TokenSet {
                access_token: "AT-expired-work".into(),
                refresh_token: Some("RT-work-1".into()),
                expires_at: None,
                scope: None,
                id_token: None,
            },
        )
        .unwrap();
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=RT-work-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "AT-work-fresh", "token_type": "Bearer", "refresh_token": "RT-work-2"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=RT-home-1"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    let vm = load(&host, &gmail(&server));
    let r = conns
        .resolve(Kind::Connector, "gmail", Some("work"), Effect::Read)
        .unwrap();
    let out = vm.call_as("inbox", &json!({}), r).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["status"], 200, "{out}");
    assert!(
        !out.contains("AT-work-fresh") && !out.contains("expired-work"),
        "{out}"
    );
    assert_eq!(
        host.secrets
            .get("conn/connector:gmail/work/oauth/access_token")
            .as_deref(),
        Some("AT-work-fresh")
    );

    // home's token is refused and its grant is dead: the call fails with
    // "sign in again", the connection is marked, and later calls say so
    // without reaching the service.
    let home = conns.get(Kind::Connector, "gmail", "home").unwrap();
    conns
        .tokens
        .save(
            &home.key(),
            &TokenSet {
                access_token: "AT-expired-home".into(),
                refresh_token: Some("RT-home-1".into()),
                expires_at: None,
                scope: None,
                id_token: None,
            },
        )
        .unwrap();
    let r = conns
        .resolve(Kind::Connector, "gmail", Some("home"), Effect::Read)
        .unwrap();
    let err = vm.call_as("inbox", &json!({}), r).await.unwrap_err();
    assert!(err.contains("needs signing in again"), "{err}");
    assert!(
        !err.contains("RT-home-1") && !err.contains("AT-expired-home"),
        "{err}"
    );
    assert_eq!(
        conns.get(Kind::Connector, "gmail", "home").unwrap().status,
        "needs_reauth"
    );
    let err = conns
        .resolve(Kind::Connector, "gmail", Some("home"), Effect::Read)
        .unwrap_err();
    assert!(err.contains("reconnect"), "{err}");
}

#[test]
fn oauth_capabilities_are_validated_and_summarised() {
    let (host, _tmp) = host_with(&[]);
    let ok = connector(
        "g",
        r#"http = { "gmail.googleapis.com" }, oauth = { google = { scopes = { "gmail.readonly" } } }"#,
        r#"go = { description = "d", effect = "read", run = function() end }"#,
    );
    let vm = load(&host, &ok);
    let caps = &vm.spec.capabilities;
    assert_eq!(caps.auth.len(), 1, "a Bearer entry by default");
    assert!(
        caps.all_secrets().is_empty(),
        "an OAuth token is not a named secret"
    );
    let summary = vm.capability_summary();
    assert!(
        summary.contains("OAuth sign-in: google (scopes: gmail.readonly)"),
        "{summary}"
    );
    for (caps, want) in [
        (
            r#"http = { "a.com" }, auth = "oauth""#,
            "needs capabilities.oauth",
        ),
        (
            r#"http = { "a.com" }, oauth = { google = {} }, auth = "oauth:slack""#,
            "signs in with",
        ),
        (r#"oauth = { google = {} }"#, "needs capabilities.http"),
        (
            r#"http = { "a.com" }, oauth = { google = {}, slack = {} }"#,
            "more than one",
        ),
        (
            r#"http = { "a.com" }, oauth = { google = { scope = {} } }"#,
            "unknown key",
        ),
        (
            r#"http = { "a.com" }, oauth = { google = { scopes = { "a b" } } }"#,
            "without spaces",
        ),
    ] {
        let src = connector(
            "g",
            caps,
            r#"go = { description = "d", run = function() end }"#,
        );
        let err = match Vm::load(&host, "g.luau", &src) {
            Ok(_) => panic!("{caps} loaded"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains(want), "{caps}: {err}");
    }
    // Explicit forms are the same thing.
    let explicit = connector(
        "g",
        r#"http = { "a.com" }, oauth = { google = {} }, auth = { secret = "oauth:google", header = "Authorization" }"#,
        r#"go = { description = "d", run = function() end }"#,
    );
    assert!(Vm::load(&host, "g.luau", &explicit).is_ok());
}

#[tokio::test]
async fn an_oauth_connector_with_no_account_asks_for_one() {
    let server = server().await;
    let (host, _tmp) = host_with(&[]);
    let vm = load(&host, &gmail(&server));
    let err = vm.call("inbox", &json!({})).await.unwrap_err();
    assert!(err.contains("connection_add"), "{err}");
    assert!(server.received_requests().await.unwrap().is_empty());
}
