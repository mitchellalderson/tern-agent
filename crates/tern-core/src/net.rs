//! The one place HTTP clients are configured.

use std::net::IpAddr;

/// A reqwest builder with tern's user agent and TLS roots.
///
/// On Linux the platform verifier reads the system CA bundle, which minimal
/// systems (scratch containers, Alpine without `ca-certificates`) don't
/// have; with no roots every HTTPS request would fail. So Mozilla's roots
/// are merged in: system roots still work (e.g. corporate CAs), and the
/// static binary works without them. macOS and Windows use the OS verifier.
///
/// Redirects are not followed: provider and MCP clients carry API keys in
/// headers, and reqwest's default would send them to whatever host a server
/// redirects to. Callers that need to follow redirects set their own policy.
pub fn http_client() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder()
        .user_agent(concat!("tern/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none());
    #[cfg(target_os = "linux")]
    let builder = builder.tls_certs_merge(
        webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter_map(|c| reqwest::Certificate::from_der(c.as_ref()).ok()),
    );
    builder
}

/// Whether an address is on the public internet. Connectors and `http_fetch`
/// refuse to reach anything else, so a model can't read the local network or
/// a cloud metadata service through them.
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
                || v4 == std::net::Ipv4Addr::new(168, 63, 129, 16) // Azure WireServer
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

/// What [`get_public`] fetched.
#[derive(Debug)]
pub struct Fetched {
    pub status: reqwest::StatusCode,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    /// The body was longer than the limit and was cut there.
    pub truncated: bool,
}

/// Redirects [`get_public`] follows, each checked like the first request.
const MAX_REDIRECTS: usize = 5;

/// Resolve `url`'s host, refusing non-public addresses (loopback too when
/// `allow_loopback` is false), and return a client pinned to exactly those
/// addresses, so DNS can't answer differently between the check and the
/// connection. It follows no redirects itself.
pub async fn public_client(
    url: &reqwest::Url,
    allow_loopback: bool,
    timeout: std::time::Duration,
) -> Result<reqwest::Client, String> {
    let host = url.host_str().ok_or("url has no host")?.to_owned();
    let port = url.port_or_known_default().unwrap_or(80);
    let lookup_host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((lookup_host.as_str(), port))
        .await
        .map_err(|e| format!("resolving {host}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("{host} did not resolve"));
    }
    if let Some(bad) = addrs
        .iter()
        .find(|a| !is_public(a.ip()) && !(allow_loopback && a.ip().is_loopback()))
    {
        return Err(format!(
            "refusing to fetch {host} ({}): loopback, private, and link-local addresses aren't allowed",
            bad.ip()
        ));
    }
    let mut builder = http_client().timeout(timeout);
    // Pin hostnames to the checked addresses; IP literals need no pinning.
    if lookup_host.parse::<IpAddr>().is_err() {
        builder = builder.resolve_to_addrs(&host, &addrs);
    }
    builder.build().map_err(|e| e.to_string())
}

/// GET a public URL the way `http_fetch` does: http(s) only, every hop's
/// address checked and pinned, redirects followed by hand (at most five),
/// and the body read up to `max` bytes. No credentials are ever sent.
pub async fn get_public(
    url: &str,
    max: usize,
    allow_loopback: bool,
    timeout: std::time::Duration,
) -> Result<Fetched, String> {
    use futures::StreamExt;
    let mut url = match reqwest::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => u,
        Ok(u) => return Err(format!("unsupported scheme {:?}", u.scheme())),
        Err(e) => return Err(format!("invalid url: {e}")),
    };
    let mut hops = 0;
    let resp = loop {
        let client = public_client(&url, allow_loopback, timeout).await?;
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
                    return Err(format!("redirect to unsupported scheme {:?}", url.scheme()));
                }
            }
            _ => break resp,
        }
    };
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
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
    Ok(Fetched {
        status,
        content_type,
        body,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn builds() {
        assert!(super::http_client().build().is_ok());
    }

    #[test]
    fn public_addresses() {
        use super::is_public;
        assert!(!is_public("169.254.169.254".parse().unwrap()));
        assert!(!is_public("100.100.1.1".parse().unwrap()));
        assert!(!is_public("168.63.129.16".parse().unwrap()));
        assert!(!is_public("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!is_public("fd00::1".parse().unwrap()));
        assert!(is_public("1.1.1.1".parse().unwrap()));
        assert!(is_public("2606:4700::1111".parse().unwrap()));
    }
}
