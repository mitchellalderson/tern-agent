//! Requests to OAuth servers. Token, device, registration, revocation,
//! userinfo and metadata endpoints are fetched the way connectors fetch:
//! https only, no redirects, a size cap, and never a private, loopback or
//! link-local address (the client is pinned to the addresses that were
//! checked), unless the user listed that server in `[auth] local_issuers`.

use std::net::SocketAddr;
use std::time::Duration;

use self::futures_lite::read_body;
use reqwest::Url;

use crate::error::{AuthError, short};

/// Largest response tern reads from an OAuth server.
const MAX_BODY: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(20);

/// Which OAuth servers may be on private addresses.
#[derive(Debug, Clone, Default)]
pub struct Guard {
    /// `scheme://host[:port]` origins from `[auth] local_issuers`.
    local: Vec<String>,
}

impl Guard {
    pub fn new(local_issuers: &[String]) -> Self {
        Self {
            local: local_issuers
                .iter()
                .filter_map(|o| Url::parse(o).ok())
                .map(|u| origin(&u))
                .collect(),
        }
    }

    /// Whether `url` is on a server the user allowed on a private address.
    pub fn is_local(&self, url: &Url) -> bool {
        self.local.contains(&origin(url))
    }

    /// A client for one request to `url`, after the checks above.
    pub async fn client(&self, url: &Url) -> Result<reqwest::Client, AuthError> {
        let local = self.is_local(url);
        if url.scheme() != "https" && !(local && url.scheme() == "http") {
            return Err(AuthError::Refused(format!(
                "{} isn't https; OAuth endpoints must be",
                origin(url)
            )));
        }
        let host = url
            .host_str()
            .ok_or_else(|| AuthError::Refused(format!("{url} has no host")))?;
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let builder = tern_core::net::http_client().timeout(TIMEOUT);
        if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
            if !local && !tern_core::net::is_public(ip) {
                return Err(private(host));
            }
            return builder.build().map_err(|e| AuthError::Http(e.to_string()));
        }
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((bare, port))
            .await
            .map_err(|e| AuthError::Http(format!("resolving {host}: {e}")))?
            .collect();
        if addrs.is_empty() {
            return Err(AuthError::Http(format!("{host} did not resolve")));
        }
        if !local && addrs.iter().any(|a| !tern_core::net::is_public(a.ip())) {
            return Err(private(host));
        }
        builder
            .resolve_to_addrs(bare, &addrs)
            .build()
            .map_err(|e| AuthError::Http(e.to_string()))
    }

    /// Send `req` (built from a client [`Guard::client`] returned) and read
    /// at most 1 MB of the answer.
    pub async fn send(
        &self,
        what: &str,
        req: reqwest::RequestBuilder,
    ) -> Result<(u16, reqwest::header::HeaderMap, String), AuthError> {
        let resp = req
            .send()
            .await
            .map_err(|e| AuthError::Http(format!("{what}: {}", without_url(&e))))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = read_body(resp, MAX_BODY)
            .await
            .map_err(|e| AuthError::Http(format!("{what}: {e}")))?;
        Ok((status, headers, body))
    }

    /// GET `url` as JSON (metadata documents).
    pub async fn get_json(&self, url: &Url) -> Result<serde_json::Value, AuthError> {
        let client = self.client(url).await?;
        let (status, _, body) = self
            .send(
                url.as_str(),
                client
                    .get(url.clone())
                    .header(reqwest::header::ACCEPT, "application/json"),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(AuthError::Http(format!("{url}: HTTP {status}")));
        }
        serde_json::from_str(&body).map_err(|e| {
            AuthError::Invalid(format!("{url}: not JSON ({})", short(&e.to_string(), 80)))
        })
    }
}

fn private(host: &str) -> AuthError {
    AuthError::Refused(format!(
        "{host} is a private, loopback or link-local address; add it to [auth] local_issuers if it is your own identity provider"
    ))
}

/// `scheme://host[:port]`, lowercased, the port only when not the default.
pub fn origin(u: &Url) -> String {
    let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
    match u.port() {
        Some(p) => format!("{}://{host}:{p}", u.scheme()),
        None => format!("{}://{host}", u.scheme()),
    }
}

/// reqwest's message without the URL it carries (a query could hold
/// something private).
fn without_url(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    if let Some(u) = e.url() {
        msg = msg.replace(u.as_str(), &origin(u));
    }
    let mut cur = std::error::Error::source(e);
    while let Some(c) = cur {
        msg = format!("{msg}: {c}");
        cur = c.source();
    }
    msg
}

mod futures_lite {
    /// The body as text, refusing more than `max` bytes.
    pub async fn read_body(mut resp: reqwest::Response, max: usize) -> Result<String, String> {
        let mut raw = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
            if raw.len() + chunk.len() > max {
                return Err(format!("response is larger than {max} bytes"));
            }
            raw.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }
}
