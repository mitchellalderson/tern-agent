//! `http_fetch`: GET a URL, converting HTML to text.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::net::get_public;
#[cfg(test)]
use tern_core::net::is_public;
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;

use crate::{args, spec};

const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;

const TIMEOUT: Duration = Duration::from_secs(30);

pub struct HttpFetch {
    /// Tests fetch from a local mock server; only loopback is let through.
    allow_loopback: bool,
}

impl HttpFetch {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            allow_loopback: false,
        })
    }

    #[cfg(test)]
    fn allowing_loopback() -> Self {
        Self {
            allow_loopback: true,
        }
    }
}

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
    max_bytes: Option<usize>,
}

#[async_trait]
impl Tool for HttpFetch {
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn untrusted(&self) -> bool {
        true
    }

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

    fn egress_host(&self, args: &Value) -> Option<String> {
        let url = reqwest::Url::parse(args.get("url")?.as_str()?).ok()?;
        url.host_str().map(str::to_ascii_lowercase)
    }

    fn policy(&self, _ctx: &ToolCtx, _args: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: FetchArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let max = a
            .max_bytes
            .unwrap_or(DEFAULT_MAX_BYTES)
            .min(DEFAULT_MAX_BYTES * 4);
        let fetch = async {
            let f = get_public(&a.url, max, self.allow_loopback, TIMEOUT).await?;
            let is_html = f
                .content_type
                .as_deref()
                .is_some_and(|ct| ct.contains("html"));
            let text = if is_html {
                html2text::from_read(f.body.as_slice(), 100)
                    .unwrap_or_else(|_| String::from_utf8_lossy(&f.body).into_owned())
            } else {
                String::from_utf8_lossy(&f.body).into_owned()
            };
            Ok::<_, String>((f.status, text, f.truncated))
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
                    untrusted: false,
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
            scope: tern_core::workspace::WorkspaceScope::for_dir(std::env::temp_dir()),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let r = HttpFetch::allowing_loopback()
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

        // Without the test override, local and metadata addresses are refused.
        let fetch = HttpFetch::new().unwrap();
        for url in [
            server.uri(),
            "http://169.254.169.254/latest/meta-data/".into(),
            "http://[::1]:9/".into(),
            "http://10.0.0.1/".into(),
        ] {
            let r = fetch.call(&ctx, json!({ "url": url })).await;
            assert!(
                r.is_error && r.content.contains("refusing to fetch"),
                "{url}: {}",
                r.content
            );
        }
    }

    #[tokio::test]
    async fn redirects_are_checked_hop_by_hop() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path("/go"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/final"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path("/final"))
            .respond_with(ResponseTemplate::new(200).set_body_string("landed"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path("/meta"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "http://169.254.169.254/x"),
            )
            .mount(&server)
            .await;
        let ctx = ToolCtx {
            scope: tern_core::workspace::WorkspaceScope::for_dir(std::env::temp_dir()),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let fetch = HttpFetch::allowing_loopback();
        let ok = fetch
            .call(&ctx, json!({"url": format!("{}/go", server.uri())}))
            .await;
        assert!(ok.content.ends_with("landed"), "{}", ok.content);
        // The first host is allowed, but its redirect to the metadata address isn't.
        let meta = fetch
            .call(&ctx, json!({"url": format!("{}/meta", server.uri())}))
            .await;
        assert!(
            meta.is_error && meta.content.contains("169.254.169.254"),
            "{}",
            meta.content
        );
        assert!(!is_public("169.254.169.254".parse().unwrap()));
        assert!(!is_public("100.100.1.1".parse().unwrap()));
        assert!(!is_public("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!is_public("fd00::1".parse().unwrap()));
        assert!(is_public("1.1.1.1".parse().unwrap()));
        assert!(is_public("2606:4700::1111".parse().unwrap()));
    }
}
