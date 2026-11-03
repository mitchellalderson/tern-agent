//! The one place HTTP clients are configured.

/// A reqwest builder with tern's user agent and TLS roots.
///
/// On Linux the platform verifier reads the system CA bundle, which minimal
/// systems (scratch containers, Alpine without `ca-certificates`) don't
/// have; with no roots every HTTPS request would fail. So Mozilla's roots
/// are merged in: system roots still work (e.g. corporate CAs), and the
/// static binary works without them. macOS and Windows use the OS verifier.
pub fn http_client() -> reqwest::ClientBuilder {
    let builder =
        reqwest::Client::builder().user_agent(concat!("tern/", env!("CARGO_PKG_VERSION")));
    #[cfg(target_os = "linux")]
    let builder = builder.tls_certs_merge(
        webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter_map(|c| reqwest::Certificate::from_der(c.as_ref()).ok()),
    );
    builder
}

#[cfg(test)]
mod tests {
    #[test]
    fn builds() {
        assert!(super::http_client().build().is_ok());
    }
}
