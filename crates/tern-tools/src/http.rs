//! `http_fetch`: GET a URL, converting HTML to text.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;

use crate::{args, spec};

const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;

const MAX_REDIRECTS: usize = 5;

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

    /// Resolve `url`'s host, refusing non-public addresses, and return a
    /// client pinned to exactly those addresses (no DNS rebinding between
    /// the check and the connection) that doesn't follow redirects itself.
    async fn client_for(&self, url: &reqwest::Url) -> Result<reqwest::Client, String> {
        let host = url.host_str().ok_or("url has no host")?.to_owned();
        let port = url.port_or_known_default().unwrap_or(80);
        let lookup_host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((lookup_host.as_str(), port))
            .await
            .map_err(|e| format!("resolving {host}: {e}"))?
            .collect();
        if addrs.is_empty() {
            return Err(format!("{host} did not resolve"));
        }
        if let Some(bad) = addrs
            .iter()
            .find(|a| !is_public(a.ip()) && !(self.allow_loopback && a.ip().is_loopback()))
        {
            return Err(format!(
                "refusing to fetch {host} ({}): loopback, private, and link-local addresses aren't allowed",
                bad.ip()
            ));
        }
        let mut builder = tern_core::net::http_client()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none());
        // Pin hostnames to the checked addresses; IP literals need no pinning.
        if lookup_host.parse::<IpAddr>().is_err() {
            builder = builder.resolve_to_addrs(&host, &addrs);
        }
        builder.build().map_err(|e| e.to_string())
    }
}

/// Whether an address is on the public internet.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64/10 shared (CGNAT)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0/24
                || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18/15 benchmarking
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80) // link local
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
            // Follow redirects by hand so every hop is checked.
            let mut url = url;
            let mut hops = 0;
            let resp = loop {
                let client = self.client_for(&url).await?;
                let resp = client
                    .get(url.clone())
                    .send()
                    .await
                    .map_err(|e| e.without_url().to_string())?;
                let location = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                match location {
                    Some(loc) if resp.status().is_redirection() => {
                        hops += 1;
                        if hops > MAX_REDIRECTS {
                            return Err(format!("more than {MAX_REDIRECTS} redirects"));
                        }
                        url = url
                            .join(&loc)
                            .map_err(|e| format!("bad redirect {loc:?}: {e}"))?;
                        if !matches!(url.scheme(), "http" | "https") {
                            return Err(format!(
                                "redirect to unsupported scheme {:?}",
                                url.scheme()
                            ));
                        }
                    }
                    _ => break resp,
                }
            };
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
            workspace: std::env::temp_dir(),
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
            workspace: std::env::temp_dir(),
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
