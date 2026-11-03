//! `http_fetch`: GET a URL, converting HTML to text.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::tool::{Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;

use crate::{args, spec};

const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;

pub struct HttpFetch {
    client: reqwest::Client,
}

impl HttpFetch {
    pub fn new() -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("tern/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { client })
    }
}

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
    max_bytes: Option<usize>,
}

#[async_trait]
impl Tool for HttpFetch {
    fn spec(&self) -> ToolSpec {
        spec(
            "http_fetch",
            "HTTP GET a URL. HTML is converted to plain text. Returns the status line and body.",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "max_bytes": { "type": "integer", "minimum": 1 }
                },
                "required": ["url"]
            }),
        )
    }

    fn policy(&self, _ctx: &ToolCtx, _args: &Value) -> ToolPolicy {
        ToolPolicy {
            idempotent: true,
            requires_approval: false,
        }
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: FetchArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let url = match reqwest::Url::parse(&a.url) {
            Ok(u) if matches!(u.scheme(), "http" | "https") => u,
            Ok(u) => return ToolResult::error(format!("unsupported scheme {:?}", u.scheme())),
            Err(e) => return ToolResult::error(format!("invalid url: {e}")),
        };
        let max = a
            .max_bytes
            .unwrap_or(DEFAULT_MAX_BYTES)
            .min(DEFAULT_MAX_BYTES * 4);
        let fetch = async {
            let resp = self
                .client
                .get(url)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let is_html = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.contains("html"));
            let mut body = Vec::new();
            let mut truncated = false;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| e.to_string())?;
                let room = max - body.len();
                if chunk.len() > room {
                    body.extend_from_slice(&chunk[..room]);
                    truncated = true;
                    break;
                }
                body.extend_from_slice(&chunk);
            }
            let text = if is_html {
                html2text::from_read(body.as_slice(), 100)
                    .unwrap_or_else(|_| String::from_utf8_lossy(&body).into_owned())
            } else {
                String::from_utf8_lossy(&body).into_owned()
            };
            Ok::<_, String>((status, text, truncated))
        };
        let result = tokio::select! {
            r = fetch => r,
            () = ctx.cancel.cancelled() => return ToolResult::error("cancelled"),
        };
        match result {
            Ok((status, text, truncated)) => {
                let mut out = format!("HTTP {status}\n\n{text}");
                if truncated {
                    out.push_str(&format!("\n[body truncated at {max} bytes]"));
                }
                ToolResult {
                    content: out,
                    is_error: !status.is_success(),
                }
            }
            Err(e) => ToolResult::error(format!("fetch failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn fetches_and_converts_html() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<html><body><h1>Title</h1><p>Hello <b>world</b></p><script>x()</script></body></html>",
                "text/html",
            ))
            .mount(&server)
            .await;
        let ctx = ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let r = HttpFetch::new()
            .unwrap()
            .call(&ctx, json!({"url": server.uri()}))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.starts_with("HTTP 200"));
        assert!(
            r.content.contains("Title") && r.content.contains("world"),
            "{}",
            r.content
        );
        assert!(!r.content.contains("<p>"));

        let bad = HttpFetch::new()
            .unwrap()
            .call(&ctx, json!({"url": "file:///etc/passwd"}))
            .await;
        assert!(bad.is_error);
    }
}
