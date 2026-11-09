//! The OAuth helper against a mock identity provider on 127.0.0.1.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use tern_auth::{AuthError, ConnKey, Guard, OAuthConfig, Tokens, flow, pkce};
use tern_core::Secrets;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn cfg(server: &MockServer) -> OAuthConfig {
    OAuthConfig {
        provider: "mock".into(),
        issuer: Some(server.uri()),
        authorize_url: format!("{}/authorize", server.uri()),
        token_url: format!("{}/token", server.uri()),
        device_url: Some(format!("{}/device", server.uri())),
        revocation_url: Some(format!("{}/revoke", server.uri())),
        userinfo_url: Some(format!("{}/userinfo", server.uri())),
        client_id: "tern-test".into(),
        scopes: vec!["read".into(), "write".into()],
        ..OAuthConfig::default()
    }
}

fn tokens(server: &MockServer) -> (tempfile::TempDir, Tokens) {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Secrets::load(&dir.path().join("secrets.toml")).unwrap();
    (dir, Tokens::new(secrets, Guard::new(&[server.uri()])))
}

fn query(url: &str, key: &str) -> String {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .unwrap()
}

/// Answers the token endpoint only when the verifier matches the challenge.
struct CheckVerifier(Arc<Mutex<Option<String>>>);

impl Respond for CheckVerifier {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&req.body).into_owned();
        let form: Vec<(String, String)> = reqwest::Url::parse(&format!("http://x/?{body}"))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        let get = |k: &str| form.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let challenge = self.0.lock().unwrap().clone().unwrap();
        let ok = get("grant_type").as_deref() == Some("authorization_code")
            && get("code").as_deref() == Some("the-code")
            && get("client_id").as_deref() == Some("tern-test")
            && get("redirect_uri").is_some_and(|r| r.starts_with("http://127.0.0.1:"))
            && get("code_verifier").is_some_and(|v| pkce::challenge(&v) == challenge);
        if ok {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT-first-0001", "token_type": "Bearer",
                "expires_in": 3600, "refresh_token": "RT-first-0001"
            }))
        } else {
            ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant", "error_description": format!("bad request: {body}")
            }))
        }
    }
}

#[tokio::test]
async fn pkce_sign_in_with_a_loopback_redirect() {
    let server = MockServer::start().await;
    let challenge = Arc::new(Mutex::new(None));
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(CheckVerifier(challenge.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"email": "me@example.com"})),
        )
        .mount(&server)
        .await;
    let cfg = cfg(&server);
    let (_dir, tokens) = tokens(&server);
    let pending = flow::begin(&cfg, &tokens.guard, None).await.unwrap();
    let url = pending.url().to_owned();
    assert!(url.starts_with(&format!("{}/authorize?", server.uri())));
    assert_eq!(query(&url, "code_challenge_method"), "S256");
    assert_eq!(query(&url, "response_type"), "code");
    assert_eq!(query(&url, "scope"), "read write");
    *challenge.lock().unwrap() = Some(query(&url, "code_challenge"));
    let state = query(&url, "state");
    let redirect = query(&url, "redirect_uri");
    assert_eq!(redirect, pending.redirect_uri());

    let key = ConnKey::new(tern_auth::Kind::Connector, "mock", "work");
    let client = tokens.client(&key, &cfg);
    let finish = tokio::spawn({
        let cfg = cfg.clone();
        let guard = tokens.guard.clone();
        async move {
            let client = tern_auth::Client {
                cfg: &cfg,
                secret: None,
                guard: &guard,
            };
            pending.finish(&client).await
        }
    });
    let browser = reqwest::Client::new();
    let port = reqwest::Url::parse(&redirect).unwrap().port().unwrap();
    // Wrong path, wrong state, another Host: refused, and the wait goes on.
    let r = browser
        .get(format!(
            "http://127.0.0.1:{port}/other?code=x&state={state}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = browser
        .get(format!("{redirect}?code=the-code&state=guessed"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = browser
        .get(format!("{redirect}?code=the-code&state={state}"))
        .header("Host", format!("rebound.example:{port}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    // The real redirect.
    let r = browser
        .get(format!("{redirect}?code=the-code&state={state}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(
        !r.text().await.unwrap().contains("the-code"),
        "nothing is echoed"
    );
    let set = finish.await.unwrap().unwrap();
    assert_eq!(set.access_token, "AT-first-0001");
    assert!(!format!("{set:?}").contains("AT-first"));
    // The listener is gone once it has a code.
    assert!(browser.get(&redirect).send().await.is_err());

    assert_eq!(
        client.account(&set).await.as_deref(),
        Some("me@example.com")
    );
    tokens.save(&key, &set).unwrap();
    assert_eq!(
        tokens.access_token(&key, &cfg, None).await.unwrap(),
        "AT-first-0001"
    );
    assert!(
        tokens
            .secrets()
            .redact("x AT-first-0001 RT-first-0001")
            .matches("[REDACTED]")
            .count()
            == 2
    );
}

#[tokio::test]
async fn a_declined_or_mixed_up_sign_in_ends_the_wait() {
    let server = MockServer::start().await;
    let (_dir, tokens) = tokens(&server);
    let mut cfg = cfg(&server);
    for (extra, want) in [
        ("error=access_denied", AuthError::Denied),
        (
            "code=x&iss=https%3A%2F%2Fevil.example",
            AuthError::Invalid(String::new()),
        ),
    ] {
        cfg.issuer = Some(server.uri());
        let pending = flow::begin(&cfg, &tokens.guard, None).await.unwrap();
        let (url, redirect) = (pending.url().to_owned(), pending.redirect_uri().to_owned());
        let state = query(&url, "state");
        let guard = tokens.guard.clone();
        let c2 = cfg.clone();
        let wait = tokio::spawn(async move {
            let client = tern_auth::Client {
                cfg: &c2,
                secret: None,
                guard: &guard,
            };
            pending.finish(&client).await
        });
        reqwest::get(format!("{redirect}?state={state}&{extra}"))
            .await
            .unwrap();
        let err = wait.await.unwrap().unwrap_err();
        assert_eq!(
            std::mem::discriminant(&err),
            std::mem::discriminant(&want),
            "{err}"
        );
    }
}

#[tokio::test]
async fn refresh_rotates_and_a_dead_grant_needs_sign_in() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=RT-old-0001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "AT-new-0002", "token_type": "bearer", "expires_in": 3600,
            "refresh_token": "RT-new-0002"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=RT-new-0002"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "token RT-new-0002 was revoked"
        })))
        .mount(&server)
        .await;
    let cfg = cfg(&server);
    let (_dir, tokens) = tokens(&server);
    let key = ConnKey::new(tern_auth::Kind::Connector, "mock", "work");
    tokens
        .save(
            &key,
            &tern_auth::TokenSet {
                access_token: "AT-old-0001".into(),
                refresh_token: Some("RT-old-0001".into()),
                expires_at: Some(tern_store::now_ms() - 1000),
                scope: None,
                id_token: None,
            },
        )
        .unwrap();
    // Expired: refreshed once, even with two callers at the same time.
    let (a, b) = tokio::join!(
        tokens.access_token(&key, &cfg, None),
        tokens.access_token(&key, &cfg, None)
    );
    assert_eq!(
        (a.unwrap(), b.unwrap()),
        ("AT-new-0002".into(), "AT-new-0002".into())
    );
    assert_eq!(
        tokens.load(&key).unwrap().refresh_token.as_deref(),
        Some("RT-new-0002")
    );
    // A 401 with a token someone already replaced doesn't refresh again.
    assert_eq!(
        tokens
            .access_token(&key, &cfg, Some("AT-old-0001"))
            .await
            .unwrap(),
        "AT-new-0002"
    );
    // A 401 on the current one does, and the dead grant says "sign in again"
    // without repeating the token the server echoed.
    let err = tokens
        .access_token(&key, &cfg, Some("AT-new-0002"))
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::NeedsReauth(_)), "{err}");
    let msg = err.to_string();
    assert!(
        !msg.contains("RT-new-0002") && msg.contains("[REDACTED]"),
        "{msg}"
    );
    // No tokens at all: sign in.
    let other = ConnKey::new(tern_auth::Kind::Connector, "mock", "home");
    assert!(matches!(
        tokens.access_token(&other, &cfg, None).await,
        Err(AuthError::NeedsReauth(_))
    ));
}

#[tokio::test]
async fn device_flow_polls_until_the_user_finishes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/device"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "DC-1234", "user_code": "WDJB-MJHT",
            "verification_url": "https://example.com/device", "interval": 1, "expires_in": 60
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("device_code=DC-1234"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({"error": "authorization_pending"})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("device_code=DC-1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "AT-device-01", "token_type": "Bearer"
        })))
        .mount(&server)
        .await;
    let cfg = cfg(&server);
    let (_dir, tokens) = tokens(&server);
    let client = tokens.client(
        &ConnKey::new(tern_auth::Kind::Connector, "mock", "default"),
        &cfg,
    );
    let code = client.device_start().await.unwrap();
    assert_eq!(code.user_code, "WDJB-MJHT");
    assert_eq!(code.verification_uri, "https://example.com/device");
    assert!(!format!("{code:?}").contains("DC-1234"));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let set = client.device_poll(&code, deadline).await.unwrap();
    assert_eq!(set.access_token, "AT-device-01");
}

#[tokio::test]
async fn private_endpoints_are_refused_unless_configured() {
    let server = MockServer::start().await;
    let cfg = cfg(&server);
    let guard = Guard::default();
    let client = tern_auth::Client {
        cfg: &cfg,
        secret: None,
        guard: &guard,
    };
    let err = client.refresh("RT-x").await.unwrap_err();
    assert!(matches!(err, AuthError::Refused(_)), "{err}");
    let https = reqwest::Url::parse("https://127.0.0.1:9/token").unwrap();
    let err = guard.client(&https).await.unwrap_err();
    assert!(err.to_string().contains("local_issuers"), "{err}");
    // The authorize page isn't fetched by tern, but a plain-http one is
    // refused all the same.
    assert!(matches!(
        flow::begin(&cfg, &guard, None).await,
        Err(AuthError::Refused(_))
    ));
}

#[tokio::test]
async fn forgetting_revokes_and_deletes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=RT-gone-01"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let cfg = cfg(&server);
    let (_dir, tokens) = tokens(&server);
    let key = ConnKey::new(tern_auth::Kind::Connector, "mock", "work");
    tokens
        .save(
            &key,
            &tern_auth::TokenSet {
                access_token: "AT-gone-01".into(),
                refresh_token: Some("RT-gone-01".into()),
                expires_at: None,
                scope: None,
                id_token: None,
            },
        )
        .unwrap();
    assert!(tokens.has(&key));
    tokens.forget(&key, Some(&cfg)).await.unwrap();
    assert!(!tokens.has(&key));
    assert_eq!(tokens.secrets().redact("RT-gone-01"), "[REDACTED]");
}
