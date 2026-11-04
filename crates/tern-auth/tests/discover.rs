//! Discovery from metadata documents, registration, and research checks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;
use tern_auth::discover::{self, Clients};
use tern_auth::{AuthPlan, ClientKind, Guard, Source, register};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mcp_fixture() -> MockServer {
    let s = MockServer::start().await;
    let uri = s.uri();
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(401).insert_header(
            "WWW-Authenticate",
            format!(r#"Bearer error="invalid_token", resource_metadata="{uri}/.well-known/oauth-protected-resource/mcp""#).as_str(),
        ))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource": format!("{uri}/mcp"),
            "authorization_servers": [uri],
            "scopes_supported": ["mcp:read", "mcp:write"]
        })))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": uri,
            "authorization_endpoint": format!("{uri}/authorize"),
            "token_endpoint": format!("{uri}/token"),
            "registration_endpoint": format!("{uri}/register"),
            "revocation_endpoint": format!("{uri}/revoke"),
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"]
        })))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/register"))
        .and(body_string_contains(
            "\"token_endpoint_auth_method\":\"none\"",
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"client_id": "dyn-123"})))
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn an_mcp_server_with_rfc9728_metadata_and_registration_needs_no_setup() {
    let s = mcp_fixture().await;
    let guard = Guard::new(&[s.uri()]);
    let url = format!("{}/mcp", s.uri());
    let plan = discover::discover(&guard, &Clients::default(), "Acme", Some(&url), true)
        .await
        .unwrap();
    assert_eq!(plan.method, "oauth2");
    assert_eq!(plan.source, Some(Source::McpMetadata));
    assert_eq!(plan.client, Some(ClientKind::DynamicRegistration));
    assert!(plan.client_ready && !plan.confidential && plan.no_setup());
    assert_eq!(plan.resource.as_deref(), Some(url.as_str()));
    assert_eq!(plan.scopes_supported, ["mcp:read", "mcp:write"]);
    assert_eq!(plan.token_url, Some(format!("{}/token", s.uri())));
    assert!(plan.verified);
    assert_eq!(tern_auth::tools::recommend(&plan), "oauth");

    let reg = register::register(
        &guard,
        plan.registration_url.as_deref().unwrap(),
        "http://127.0.0.1:5555/callback",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(reg.client_id, "dyn-123");
    let cfg = plan
        .oauth_config(&reg.client_id, vec!["mcp:read".into()])
        .unwrap();
    assert_eq!(cfg.resource.as_deref(), Some(url.as_str()));
}

#[tokio::test]
async fn metadata_that_speaks_for_another_server_is_ignored() {
    let s = MockServer::start().await;
    let uri = s.uri();
    // Protected resource metadata about a different resource.
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource": "https://elsewhere.example/mcp",
            "authorization_servers": [uri]
        })))
        .mount(&s)
        .await;
    // Authorization server metadata claiming another issuer.
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": "https://accounts.evil.example",
            "authorization_endpoint": "https://accounts.evil.example/a",
            "token_endpoint": "https://accounts.evil.example/t"
        })))
        .mount(&s)
        .await;
    let guard = Guard::new(std::slice::from_ref(&uri));
    let plan = discover::discover(
        &guard,
        &Clients::default(),
        "Acme",
        Some(&format!("{uri}/mcp")),
        true,
    )
    .await
    .unwrap();
    assert_eq!(plan.method, "unknown", "{plan:?}");
}

#[tokio::test]
async fn an_api_host_with_rfc8414_metadata() {
    let s = MockServer::start().await;
    let uri = s.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": uri,
            "authorization_endpoint": format!("{uri}/authorize"),
            "token_endpoint": format!("{uri}/token"),
            "device_authorization_endpoint": format!("{uri}/device"),
            "userinfo_endpoint": format!("{uri}/userinfo"),
            "code_challenge_methods_supported": ["plain", "S256"],
            "scopes_supported": ["openid", "read"]
        })))
        .mount(&s)
        .await;
    let guard = Guard::new(std::slice::from_ref(&uri));
    let plan = discover::discover(
        &guard,
        &Clients::default(),
        "Acme",
        Some(&format!("{uri}/v1")),
        false,
    )
    .await
    .unwrap();
    assert_eq!(plan.source, Some(Source::WellKnown));
    assert_eq!(plan.device_url, Some(format!("{uri}/device")));
    // No registration endpoint and no configured client: OAuth needs setup.
    assert_eq!(plan.client, Some(ClientKind::UserSupplied));
    assert!(!plan.client_ready && !plan.no_setup());
    // With the user's own app configured for it, it is ready.
    let clients = Clients {
        configured: [(plan.provider.clone(), vec![uri.clone()])].into(),
        cimd: false,
    };
    let plan = discover::discover(&guard, &clients, "Acme", Some(&format!("{uri}/v1")), false)
        .await
        .unwrap();
    assert!(plan.client_ready);
}

#[tokio::test]
async fn private_hosts_are_not_probed_unless_configured() {
    let s = mcp_fixture().await;
    let plan = discover::discover(
        &Guard::default(),
        &Clients::default(),
        "Acme",
        Some(&format!("{}/mcp", s.uri())),
        true,
    )
    .await
    .unwrap();
    assert_eq!(plan.method, "unknown");
    assert!(
        s.received_requests().await.unwrap().is_empty(),
        "nothing was sent"
    );
}

#[tokio::test]
async fn built_in_profiles_need_no_network() {
    let plan = discover::discover(&Guard::offline(), &Clients::default(), "Gmail", None, false)
        .await
        .unwrap();
    assert_eq!(plan.source, Some(Source::Builtin));
    assert_eq!(plan.provider, "google");
    assert!(plan.verified && !plan.client_ready);
    assert!(plan.notes.iter().any(|n| n.contains("7 days")));
}

fn researched(authorize: &str, token: &str) -> AuthPlan {
    AuthPlan {
        method: "oauth2".into(),
        service: "Notion".into(),
        authorize_url: Some(authorize.into()),
        token_url: Some(token.into()),
        api_hosts: vec!["api.notion.com".into()],
        evidence: vec!["https://developers.notion.com/docs/authorization".into()],
        ..AuthPlan::default()
    }
}

#[tokio::test]
async fn a_researched_token_url_on_another_domain_is_rejected() {
    let guard = Guard::offline();
    for (a, t) in [
        (
            "https://api.notion.com/v1/oauth/authorize",
            "https://notion-oauth.com/token",
        ),
        (
            "https://api.notion.com.evil.test/authorize",
            "https://api.notion.com/v1/oauth/token",
        ),
        (
            "https://api.notion.com/v1/oauth/authorize",
            "http://api.notion.com/v1/oauth/token",
        ),
        (
            "https://evil.okta.com/authorize",
            "https://evil.okta.com/token",
        ),
    ] {
        let mut plan = researched(a, t);
        let err = discover::validate_research(&guard, &mut plan)
            .await
            .unwrap_err();
        assert!(!err.is_empty(), "{a} {t}");
    }
    // Same registrable domain, no metadata to confirm it: allowed, labelled.
    let mut plan = researched(
        "https://api.notion.com/v1/oauth/authorize",
        "https://api.notion.com/v1/oauth/token",
    );
    plan.issuer = Some("https://203.0.113.9".into());
    let err = discover::validate_research(&guard, &mut plan)
        .await
        .unwrap_err();
    assert!(err.contains("issuer"), "{err}");
    // `.invalid` never resolves (RFC 6761), so there is no metadata to
    // confirm with: allowed, and labelled unverified.
    let mut plan = researched(
        "https://auth.acme.invalid/authorize",
        "https://api.acme.invalid/token",
    );
    plan.api_hosts = vec!["api.acme.invalid".into()];
    discover::validate_research(&guard, &mut plan)
        .await
        .unwrap();
    assert!(!plan.verified);
    assert_eq!(plan.source, Some(Source::Research));
    assert!(plan.notes.iter().any(|n| n.starts_with("Unverified")));
}

#[tokio::test]
async fn research_is_confirmed_or_refused_by_the_issuers_metadata() {
    let s = MockServer::start().await;
    let uri = s.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": uri,
            "authorization_endpoint": "https://api.notion.com/v1/oauth/authorize",
            "token_endpoint": "https://api.notion.com/v1/oauth/token"
        })))
        .mount(&s)
        .await;
    let guard = Guard::new(std::slice::from_ref(&uri));
    let mut plan = researched(
        "https://api.notion.com/v1/oauth/authorize",
        "https://api.notion.com/v1/oauth/token",
    );
    plan.issuer = Some(uri.clone());
    discover::validate_research(&guard, &mut plan)
        .await
        .unwrap();
    assert!(plan.verified, "{plan:?}");
    // The issuer's metadata disagrees with what the page said.
    let mut plan = researched(
        "https://api.notion.com/v1/oauth/authorize",
        "https://api.notion.com/v2/token",
    );
    plan.issuer = Some(uri.clone());
    let err = discover::validate_research(&guard, &mut plan)
        .await
        .unwrap_err();
    assert!(err.contains("different endpoints"), "{err}");
}
