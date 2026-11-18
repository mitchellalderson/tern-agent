use std::sync::Mutex as StdMutex;

use tern_core::approval::{ApprovalRequest, Decision};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::offer_connection::OfferConnection;

/// Records sign-in cards; answers with `answer`, or never when `None`.
struct Cards {
    answer: Option<String>,
    seen: StdMutex<Vec<AuthPrompt>>,
}

impl Cards {
    fn new(answer: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            answer: answer.map(str::to_owned),
            seen: StdMutex::default(),
        })
    }
}

#[async_trait]
impl ApprovalHandler for Cards {
    async fn ask(&self, _: &ApprovalRequest) -> Decision {
        Decision::Deny
    }
    async fn ask_auth(&self, req: &AuthPrompt) -> Option<String> {
        self.seen.lock().unwrap().push(req.clone());
        match &self.answer {
            Some(a) => Some(a.clone()),
            None => std::future::pending().await,
        }
    }
}

fn ctx(run: &str) -> ToolCtx {
    ToolCtx {
        scope: tern_core::workspace::WorkspaceScope::for_dir(std::env::temp_dir()),
        run_id: run.into(),
        session_id: "s".into(),
        cancel: CancellationToken::new(),
    }
}

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    cards: Arc<Cards>,
    client: Arc<Composio>,
}

async fn env(key: Option<&str>, answer: Option<&str>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Secrets::load(&dir.path().join("secrets.toml")).unwrap();
    if let Some(k) = key {
        secrets.set(KEY, k).unwrap();
    }
    let server = MockServer::start().await;
    let cards = Cards::new(answer);
    let client = Composio::with_base(
        secrets,
        cards.clone(),
        &server.uri(),
        Duration::from_millis(20),
        Duration::from_millis(500),
    )
    .unwrap();
    Env {
        _dir: dir,
        server,
        cards,
        client,
    }
}

fn tool_json(slug: &str, toolkit: &str, tags: &[&str]) -> Value {
    json!({"slug": slug, "toolkit": {"slug": toolkit}, "tags": tags,
           "description": "d", "input_parameters": {}})
}

async fn mock_tool(e: &Env, slug: &str, toolkit: &str, tags: &[&str]) {
    Mock::given(method("GET"))
        .and(path(format!("/tools/{slug}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_json(slug, toolkit, tags)))
        .mount(&e.server)
        .await;
}

async fn mock_exec(e: &Env, slug: &str, data: Value) {
    Mock::given(method("POST"))
        .and(path(format!("/tools/execute/{slug}")))
        .and(header("x-api-key", "ck_test"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": data, "successful": true, "error": null})),
        )
        .mount(&e.server)
        .await;
}

async fn executed(e: &Env) -> usize {
    e.server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with("/tools/execute/"))
        .count()
}

#[test]
fn tags_decide_the_class_and_destructive_wins() {
    let t = |a: &[&str]| classify(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    assert_eq!(t(&["readOnlyHint"]), Class::ReadOnly);
    assert_eq!(t(&["readOnlyHint", "destructiveHint"]), Class::Destructive);
    assert_eq!(t(&["destructiveHint"]), Class::Destructive);
    // Nothing tagged fails closed.
    assert_eq!(t(&[]), Class::Write);
    assert_eq!(t(&["important"]), Class::Write);
    assert!(is_payment_toolkit("Stripe") && !is_payment_toolkit("gmail"));
}

#[test]
fn html_becomes_text_and_long_strings_are_capped() {
    let v = clean(json!({
        "body": "<html><body><h1>Hi</h1><p>Pay <b>now</b></p><script>x()</script></body></html>",
        "plain": "a".repeat(10_000),
        "nested": [{"s": "short"}],
        "n": 3,
    }));
    let body = v["body"].as_str().unwrap();
    assert!(body.contains("Hi") && body.contains("now"), "{body}");
    assert!(!body.contains("<p>") && !body.contains("x()"));
    let plain = v["plain"].as_str().unwrap();
    assert!(plain.chars().count() < MAX_STRING + 60 && plain.contains("more characters cut"));
    assert_eq!(v["nested"][0]["s"], "short");
    assert_eq!(v["n"], 3);
}

#[tokio::test]
async fn missing_key_asks_for_it_and_a_rejected_key_asks_to_replace() {
    let e = env(None, None).await;
    let r = SearchTools(e.client.clone())
        .call(&ctx("r"), json!({"query": "emails"}))
        .await;
    assert!(r.is_error);
    let v: Value = serde_json::from_str(&r.content).unwrap();
    assert_eq!(v["hint"]["tool"], "secret_request");
    assert_eq!(v["hint"]["names"][0], KEY);
    assert!(v["hint"].get("replace").is_none());
    assert!(e.server.received_requests().await.unwrap().is_empty());

    let e = env(Some("ck_bad"), None).await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "bad key"}})),
        )
        .mount(&e.server)
        .await;
    let r = ListConnections(e.client.clone())
        .call(&ctx("r"), json!({}))
        .await;
    assert!(r.is_error);
    let v: Value = serde_json::from_str(&r.content).unwrap();
    assert_eq!(v["hint"]["tool"], "secret_request");
    assert_eq!(v["hint"]["replace"], true);
    assert!(!r.content.contains("ck_bad"));
}

#[tokio::test]
async fn search_labels_each_tool_and_taints() {
    let e = env(Some("ck_test"), None).await;
    Mock::given(method("GET"))
        .and(path("/tools"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            tool_json("GMAIL_FETCH_EMAILS", "gmail", &["readOnlyHint"]),
            tool_json("GMAIL_SEND_EMAIL", "gmail", &[]),
            tool_json("GMAIL_DELETE_MESSAGE", "gmail", &["destructiveHint"]),
            tool_json("STRIPE_CREATE_REFUND", "stripe", &[]),
        ]})))
        .mount(&e.server)
        .await;
    let r = SearchTools(e.client.clone())
        .call(&ctx("r"), json!({"query": "mail", "app": "Gmail"}))
        .await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.untrusted());
    let v: Value = serde_json::from_str(&r.content).unwrap();
    let t = v["tools"].as_array().unwrap();
    assert_eq!(t[0]["run_with"], RUN_READ);
    assert_eq!(t[1]["run_with"], RUN);
    assert_eq!(t[2]["run_with"], RUN_DESTRUCTIVE);
    assert!(t[3]["run_with"].as_str().unwrap().starts_with("refused"));
    let reqs = e.server.received_requests().await.unwrap();
    assert!(reqs[0].url.query().unwrap().contains("toolkit_slug=gmail"));
}

#[tokio::test]
async fn read_tool_runs_only_read_tagged_tools_and_cleans_output() {
    let e = env(Some("ck_test"), None).await;
    mock_tool(&e, "GMAIL_FETCH_EMAILS", "gmail", &["readOnlyHint"]).await;
    mock_tool(&e, "GMAIL_SEND_EMAIL", "gmail", &[]).await;
    mock_exec(
        &e,
        "GMAIL_FETCH_EMAILS",
        json!({"messages": [{"html": format!("<div><p>Hello</p>{}</div>", "x".repeat(9000))}]}),
    )
    .await;
    let read = RunTool::read(e.client.clone());
    let p = read.policy(&ctx("r"), &json!({}));
    assert_eq!(p.effect, Effect::Read);
    assert!(!p.requires_approval);

    let ok = read
        .call(
            &ctx("r"),
            json!({"tool_slug": "gmail_fetch_emails", "arguments": {"max": 1}}),
        )
        .await;
    assert!(!ok.is_error, "{}", ok.content);
    assert!(ok.untrusted());
    let v: Value = serde_json::from_str(&ok.content).unwrap();
    let html = v["messages"][0]["html"].as_str().unwrap();
    assert!(html.starts_with("Hello") && !html.contains("<p>") && html.chars().count() < 4100);
    assert_eq!(executed(&e).await, 1);

    // A write-tagged tool never runs through the read tool.
    let no = read
        .call(&ctx("r"), json!({"tool_slug": "GMAIL_SEND_EMAIL"}))
        .await;
    assert!(no.is_error && no.content.contains(RUN), "{}", no.content);
    assert_eq!(executed(&e).await, 1);
}

#[tokio::test]
async fn write_and_destructive_tools_ask_and_refuse_the_wrong_class() {
    let e = env(Some("ck_test"), None).await;
    mock_tool(&e, "GMAIL_SEND_EMAIL", "gmail", &[]).await;
    mock_tool(&e, "GMAIL_DELETE_MESSAGE", "gmail", &["destructiveHint"]).await;
    mock_tool(&e, "GMAIL_FETCH_EMAILS", "gmail", &["readOnlyHint"]).await;
    mock_exec(&e, "GMAIL_SEND_EMAIL", json!({"id": "1"})).await;
    mock_exec(&e, "GMAIL_DELETE_MESSAGE", json!({"deleted": true})).await;
    let write = RunTool::write(e.client.clone());
    let destroy = RunTool::destructive(e.client.clone());
    let pw = write.policy(&ctx("r"), &json!({}));
    assert_eq!(pw.effect, Effect::Write);
    assert!(!pw.granted);
    assert_eq!(
        destroy.policy(&ctx("r"), &json!({})).effect,
        Effect::Destructive
    );

    assert!(
        !write
            .call(&ctx("r"), json!({"tool_slug": "GMAIL_SEND_EMAIL"}))
            .await
            .is_error
    );
    for slug in ["GMAIL_DELETE_MESSAGE", "GMAIL_FETCH_EMAILS"] {
        let r = write.call(&ctx("r"), json!({"tool_slug": slug})).await;
        assert!(r.is_error, "{slug}");
    }
    assert_eq!(executed(&e).await, 1);
    assert!(
        !destroy
            .call(&ctx("r"), json!({"tool_slug": "GMAIL_DELETE_MESSAGE"}))
            .await
            .is_error
    );
    let r = destroy
        .call(&ctx("r"), json!({"tool_slug": "GMAIL_SEND_EMAIL"}))
        .await;
    assert!(r.is_error);
    assert_eq!(executed(&e).await, 2);
}

#[tokio::test]
async fn payment_toolkits_are_read_only() {
    let e = env(Some("ck_test"), None).await;
    mock_tool(&e, "STRIPE_LIST_CHARGES", "stripe", &["readOnlyHint"]).await;
    mock_tool(&e, "STRIPE_CREATE_REFUND", "stripe", &[]).await;
    mock_tool(&e, "STRIPE_DELETE_CUSTOMER", "stripe", &["destructiveHint"]).await;
    mock_exec(&e, "STRIPE_LIST_CHARGES", json!({"charges": []})).await;
    mock_exec(&e, "STRIPE_CREATE_REFUND", json!({})).await;
    mock_exec(&e, "STRIPE_DELETE_CUSTOMER", json!({})).await;
    let read = RunTool::read(e.client.clone());
    let write = RunTool::write(e.client.clone());
    let destroy = RunTool::destructive(e.client.clone());
    assert!(
        !read
            .call(&ctx("r"), json!({"tool_slug": "STRIPE_LIST_CHARGES"}))
            .await
            .is_error
    );
    for (t, slug) in [
        (&write, "STRIPE_CREATE_REFUND"),
        (&destroy, "STRIPE_DELETE_CUSTOMER"),
        (&read, "STRIPE_CREATE_REFUND"),
    ] {
        let r = t.call(&ctx("r"), json!({"tool_slug": slug})).await;
        assert!(r.is_error, "{slug}");
    }
    assert_eq!(executed(&e).await, 1);
}

#[tokio::test]
async fn sign_in_link_goes_to_the_card_never_to_the_model() {
    let e = env(Some("ck_test"), None).await;
    Mock::given(method("GET"))
        .and(path("/connected_accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth_configs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": "ac_other", "is_composio_managed": false, "is_disabled": false},
            {"id": "ac_1", "is_composio_managed": true, "is_disabled": false}]})))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/connected_accounts/link"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "connected_account_id": "ca_1",
            "redirect_url": "https://backend.composio.dev/api/v3/s/SECRET123",
            "link_token": "t", "expires_at": "x"})))
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/connected_accounts/ca_1$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ACTIVE"})))
        .mount(&e.server)
        .await;
    let r = ConnectApp(e.client.clone())
        .call(&ctx("r"), json!({"app": "Gmail"}))
        .await;
    assert!(!r.is_error, "{}", r.content);
    let v: Value = serde_json::from_str(&r.content).unwrap();
    assert_eq!(v["connected"], true);
    assert!(!r.content.contains("SECRET123") && !r.content.contains("http"));
    let first = e.cards.seen.lock().unwrap()[0].card.clone();
    match first {
        AuthCard::SignIn { url, .. } => {
            assert_eq!(
                url.as_deref(),
                Some("https://backend.composio.dev/api/v3/s/SECRET123")
            );
        }
        other => panic!("{other:?}"),
    }
    let link = e
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.url.path() == "/connected_accounts/link")
        .unwrap();
    let body: Value = serde_json::from_slice(&link.body).unwrap();
    assert_eq!(body["auth_config_id"], "ac_1");
}

#[tokio::test]
async fn unsafe_sign_in_links_are_refused_and_cancel_stops_the_wait() {
    for bad in [
        "http://backend.composio.dev/x",
        "https://user:pw@evil.example/x",
        "javascript:alert(1)",
    ] {
        assert!(checked_sign_in_url(bad).is_err(), "{bad}");
    }
    assert!(checked_sign_in_url("https://backend.composio.dev/api/v3/s/a").is_ok());

    let e = env(Some("ck_test"), Some("cancel")).await;
    Mock::given(method("GET"))
        .and(path("/connected_accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth_configs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth_configs"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"auth_config": {"id": "ac_new"}})),
        )
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/connected_accounts/link"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "connected_account_id": "ca_1", "redirect_url": "http://evil.example/x"})))
        .mount(&e.server)
        .await;
    let r = ConnectApp(e.client.clone())
        .call(&ctx("r"), json!({"app": "slack"}))
        .await;
    assert!(
        r.is_error && r.content.contains("refusing"),
        "{}",
        r.content
    );
    assert!(e.cards.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn offer_connection_is_once_per_app_per_conversation() {
    let cards = Cards::new(Some("composio"));
    let offer = OfferConnection::new(cards.clone());
    let r = offer
        .call(&ctx("r"), json!({"app": "Gmail", "task": "read my mail"}))
        .await;
    let v: Value = serde_json::from_str(&r.content).unwrap();
    assert_eq!(v["choice"], "composio");
    assert!(v["next"].as_str().unwrap().contains("composio_connect_app"));
    match &cards.seen.lock().unwrap()[0].card {
        AuthCard::Choose { options, .. } => {
            let ids: Vec<_> = options.iter().map(|o| o.id.as_str()).collect();
            assert_eq!(ids, ["composio", "mcp", "build"]);
        }
        other => panic!("{other:?}"),
    }
    // Same app, same conversation (even another run): no second card.
    let again = offer.call(&ctx("r2"), json!({"app": "gmail"})).await;
    assert!(
        again.content.contains("\"offered\":false"),
        "{}",
        again.content
    );
    assert_eq!(cards.seen.lock().unwrap().len(), 1);
    // Another app is a new offer; so is another conversation.
    offer.call(&ctx("r"), json!({"app": "Slack"})).await;
    let mut other = ctx("r3");
    other.session_id = "s2".into();
    offer.call(&other, json!({"app": "Gmail"})).await;
    assert_eq!(cards.seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn offer_connection_with_nobody_to_ask_says_so() {
    let offer = OfferConnection::new(Cards::new(Some("bogus")));
    let r = offer.call(&ctx("r"), json!({"app": "Notion"})).await;
    assert!(r.content.contains("\"none\""), "{}", r.content);
}
