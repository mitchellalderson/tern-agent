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
            if let Some(v4) = embedded_v4(v6) {
                return is_public(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80 // link local
                || (seg[0] & 0xffc0) == 0xfec0 // site local (deprecated)
                || (seg[0] == 0x64 && seg[1] == 0xff9b && seg[2] == 1) // 64:ff9b:1::/48 local NAT64
                || (seg[0] == 0x2001 && seg[1] == 0) // 2001::/32 Teredo (embeds an obfuscated v4)
                || (seg[0] == 0x2001 && seg[1] == 0xdb8) // documentation
                || (seg[0] == 0x100 && seg[1..4] == [0, 0, 0])) // 100::/64 discard
        }
    }
}

/// The IPv4 address inside an IPv6 one that routes to it: mapped
/// (`::ffff:a.b.c.d`), compatible (`::a.b.c.d`), NAT64 (`64:ff9b::a.b.c.d`,
/// which DNS64 hands out for v4-only names) and 6to4 (`2002:aabb:ccdd::`).
/// Each is judged by the v4 rules, so `64:ff9b::a9fe:a9fe` is the metadata
/// service and `::1` is `0.0.0.1`.
fn embedded_v4(v6: std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let seg = v6.segments();
    let tail = || std::net::Ipv4Addr::from((u32::from(seg[6]) << 16) | u32::from(seg[7]));
    if let Some(v4) = v6.to_ipv4_mapped() {
        Some(v4)
    } else if seg[..6] == [0; 6] || (seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0]) {
        Some(tail())
    } else if seg[0] == 0x2002 {
        Some(std::net::Ipv4Addr::from(
            (u32::from(seg[1]) << 16) | u32::from(seg[2]),
        ))
    } else {
        None
    }
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
        // IPv6 forms that carry an IPv4 address are judged by it.
        for denied in [
            "64:ff9b::a9fe:a9fe", // NAT64 169.254.169.254
            "64:ff9b::7f00:1",
            "64:ff9b::10.0.0.1",
            "64:ff9b:1::1",     // local-use NAT64
            "2002:a9fe:a9fe::", // 6to4 169.254.169.254
            "2002:7f00:1::1",
            "2002:c0a8:101::1", // 6to4 192.168.1.1
            "::127.0.0.1",      // IPv4-compatible
            "::a9fe:a9fe",
            "::1",
            "::",
            "fec0::1", // site local
            "2001::1", // Teredo
            "2001:db8::1",
            "100::1",
        ] {
            assert!(!is_public(denied.parse().unwrap()), "{denied}");
        }
        assert!(
            is_public("64:ff9b::101:101".parse().unwrap()),
            "NAT64 1.1.1.1"
        );
        assert!(
            is_public("2002:101:101::1".parse().unwrap()),
            "6to4 1.1.1.1"
        );
        assert!(is_public("1.1.1.1".parse().unwrap()));
        assert!(is_public("2606:4700::1111".parse().unwrap()));
    }
}
