//! The OpenRouter 404 (`HTTP 404: {"error":{"message":"Not Found","code":404}}`
//! on every run): a local server that answers like OpenRouter pins down the
//! exact request tern makes.

use futures::StreamExt;
use serde_json::Value;
use tern_core::types::{ChatRequest, Message, Provider, ProviderError, StreamEvent};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::OpenAiCompat;

const OK: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

fn req() -> ChatRequest {
    ChatRequest {
        model: "minimax/minimax-m3.1-flash".into(),
        system: String::new(),
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        max_tokens: 10,
        forbid_tool_calls: false,
    }
}

/// Answers like OpenRouter: only `POST /api/v1/chat/completions` exists;
/// every other path gets the bare `Not Found` the user saw.
async fn openrouter_like() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-or-test"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(OK, "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_string(r#"{"error":{"message":"Not Found","code":404}}"#),
        )
        .mount(&server)
        .await;
    server
}

async fn run(base: &str) -> Result<Vec<StreamEvent>, ProviderError> {
    let p = OpenAiCompat::new("openrouter", base, Some("sk-or-test".into())).unwrap();
    let s = p.stream(req(), CancellationToken::new()).await?;
    s.collect::<Vec<_>>().await.into_iter().collect()
}

#[tokio::test]
async fn a_base_url_without_api_v1_reproduces_the_reported_404() {
    let server = openrouter_like().await;
    // What a hand-typed `https://openrouter.ai/v1` amounts to.
    let err = run(&format!("{}/v1", server.uri())).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"HTTP 404: {"error":{"message":"Not Found","code":404}}"#
    );
}

#[tokio::test]
async fn posts_to_base_chat_completions_with_a_bearer_key() {
    let server = openrouter_like().await;
    let events = run(&format!("{}/api/v1/", server.uri())).await.unwrap();
    assert!(events.contains(&StreamEvent::TextDelta("hi".into())));
    let seen = &server.received_requests().await.unwrap()[0];
    assert_eq!(seen.url.path(), "/api/v1/chat/completions");
    let body: Value = serde_json::from_slice(&seen.body).unwrap();
    assert_eq!(body["model"], "minimax/minimax-m3.1-flash");
}

#[tokio::test]
async fn a_pasted_chat_completions_url_is_not_doubled() {
    let server = openrouter_like().await;
    let base = format!("{}/api/v1/chat/completions", server.uri());
    assert!(run(&base).await.is_ok());
}

#[tokio::test]
async fn openrouter_gets_app_headers_and_others_do_not() {
    let server = openrouter_like().await;
    let base = format!("{}/api/v1", server.uri());
    let mut p = OpenAiCompat::new("openrouter", &base, Some("sk-or-test".into())).unwrap();
    p.attribution = true;
    let events = p.stream(req(), CancellationToken::new()).await.unwrap();
    events.collect::<Vec<_>>().await;
    run(&base).await.unwrap();
    let seen: Vec<Request> = server.received_requests().await.unwrap();
    let with = |r: &Request, h: &str| r.headers.get(h).map(|v| v.to_str().unwrap().to_owned());
    assert_eq!(with(&seen[0], "x-title").as_deref(), Some("tern"));
    assert!(
        with(&seen[0], "http-referer")
            .unwrap()
            .starts_with("https://")
    );
    assert_eq!(with(&seen[1], "x-title"), None);
    assert_eq!(with(&seen[1], "http-referer"), None);
}

#[test]
fn the_openrouter_host_turns_attribution_on_and_fixes_the_path() {
    let p = OpenAiCompat::new("o", "https://openrouter.ai/v1", None).unwrap();
    assert!(p.attribution);
    assert_eq!(p.base_url, "https://openrouter.ai/api/v1");
    let other = OpenAiCompat::new("o", "https://api.openai.com/v1", None).unwrap();
    assert!(!other.attribution);
}

/// The user's real config: `base_url = ".../api/v1/responses"` (OpenRouter's
/// Responses API address), built through the same path the daemon uses.
#[tokio::test]
async fn a_pasted_responses_endpoint_still_reaches_chat_completions() {
    use tern_core::Secrets;
    use tern_core::config::{ProviderConfig, ProviderKind};
    let server = openrouter_like().await;
    let cfg = ProviderConfig {
        kind: ProviderKind::OpenaiCompat,
        base_url: Some(format!("{}/api/v1/responses", server.uri())),
        api_key_secret: Some("OPENROUTER_TEST_KEY".into()),
        context_window: None,
        prices: Default::default(),
    };
    // The key comes from a secrets file, not the environment.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.toml");
    std::fs::write(&path, "OPENROUTER_TEST_KEY = \"sk-or-test\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let secrets = Secrets::load(&path).unwrap();
    let p = crate::build("openrouter", &cfg, &secrets).unwrap();
    let stream = p.stream(req(), CancellationToken::new()).await.unwrap();
    let events: Vec<_> = stream.collect().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ok(StreamEvent::TextDelta(t)) if t == "hi"))
    );
    let seen = &server.received_requests().await.unwrap()[0];
    assert_eq!(seen.url.path(), "/api/v1/chat/completions");
}
