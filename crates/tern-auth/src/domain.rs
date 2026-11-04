//! Validating OAuth endpoints that came from research (tooling spec
//! §6.7.2, wargame W10): a web page must not be able to point the user at
//! a lookalike sign-in page.

use reqwest::Url;

/// Identity providers that sign in for other companies' APIs. Exact hosts
/// are the providers' own; `*.` entries are tenant patterns, which anyone
/// can register, so a plan using one must also be confirmed by the
/// issuer's metadata.
const KNOWN_IDPS: &[&str] = &[
    "accounts.google.com",
    "oauth2.googleapis.com",
    "login.microsoftonline.com",
    "login.live.com",
    "*.okta.com",
    "*.oktapreview.com",
    "*.auth0.com",
    "*.onelogin.com",
    "*.amazoncognito.com",
    "*.b2clogin.com",
    "*.pingone.com",
];

/// The registrable domain (eTLD+1, with the public suffix list's private
/// section, so `a.github.io` and `b.github.io` are different sites).
/// `None` for IP addresses and bare suffixes.
pub fn registrable(host: &str) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return None;
    }
    let d = psl::domain(host.as_bytes())?;
    std::str::from_utf8(d.as_bytes()).ok().map(str::to_owned)
}

/// What a known identity provider match was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idp {
    /// The provider's own host.
    Exact,
    /// A tenant subdomain anyone could register.
    Tenant,
}

pub fn known_idp(host: &str) -> Option<Idp> {
    let host = host.to_ascii_lowercase();
    KNOWN_IDPS.iter().find_map(|p| match p.strip_prefix("*.") {
        Some(base) if host.ends_with(&format!(".{base}")) && host.len() > base.len() + 1 => {
            Some(Idp::Tenant)
        }
        Some(_) => None,
        None => (*p == host).then_some(Idp::Exact),
    })
}

/// How an endpoint relates to the API it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relation {
    /// Same registrable domain as one of the API hosts.
    SameSite,
    KnownIdp(Idp),
}

/// Check one researched endpoint against the API hosts the connector
/// declares. `Err` says why it is refused.
pub fn check_endpoint(what: &str, url: &str, api_hosts: &[String]) -> Result<Relation, String> {
    let u = Url::parse(url).map_err(|_| format!("the {what} URL {url:?} isn't a URL"))?;
    if u.scheme() != "https" {
        return Err(format!(
            "the {what} URL must be https, not {:?}",
            u.scheme()
        ));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(format!("the {what} URL carries a user name"));
    }
    let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
    let site = registrable(&host).ok_or_else(|| {
        format!(
            "the {what} host {host:?} has no registrable domain (an IP address or a bare suffix)"
        )
    })?;
    let api_sites: Vec<String> = api_hosts
        .iter()
        .filter_map(|h| registrable(h.trim_start_matches("*.")))
        .collect();
    if api_sites.contains(&site) {
        return Ok(Relation::SameSite);
    }
    if let Some(idp) = known_idp(&host) {
        return Ok(Relation::KnownIdp(idp));
    }
    Err(format!(
        "the {what} host {host} ({site}) is not on the API's domain ({}) and is not a known identity provider",
        if api_sites.is_empty() {
            "none declared".to_owned()
        } else {
            api_sites.join(", ")
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(h: &[&str]) -> Vec<String> {
        h.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn registrable_domains_follow_the_public_suffix_list() {
        assert_eq!(registrable("api.notion.com").as_deref(), Some("notion.com"));
        assert_eq!(
            registrable("auth.notion.com.").as_deref(),
            Some("notion.com")
        );
        assert_eq!(
            registrable("a.b.example.co.uk").as_deref(),
            Some("example.co.uk")
        );
        // Private suffixes: different users' sites.
        assert_eq!(
            registrable("victim.github.io").as_deref(),
            Some("victim.github.io")
        );
        assert_ne!(
            registrable("victim.github.io"),
            registrable("attacker.github.io")
        );
        assert_eq!(registrable("1.2.3.4"), None);
    }

    #[test]
    fn lookalikes_are_refused() {
        let api = hosts(&["api.notion.com"]);
        assert_eq!(
            check_endpoint("token", "https://auth.notion.com/token", &api),
            Ok(Relation::SameSite)
        );
        for bad in [
            "https://notion-auth.com/token",
            "https://auth.notion.com.evil.test/token",
            "https://auth.n0tion.com/token",
            // Cyrillic о: the URL parser turns it into punycode, another site.
            "https://auth.nоtion.com/token",
            "http://auth.notion.com/token",
            "https://user@auth.notion.com/token",
            "https://93.184.216.34/token",
        ] {
            assert!(check_endpoint("token", bad, &api).is_err(), "{bad}");
        }
        assert_eq!(
            check_endpoint(
                "authorize",
                "https://accounts.google.com/o/oauth2/auth",
                &api
            ),
            Ok(Relation::KnownIdp(Idp::Exact))
        );
        assert_eq!(
            check_endpoint("authorize", "https://notion.okta.com/authorize", &api),
            Ok(Relation::KnownIdp(Idp::Tenant))
        );
        assert!(check_endpoint("authorize", "https://okta.com.evil.test/a", &api).is_err());
    }
}
