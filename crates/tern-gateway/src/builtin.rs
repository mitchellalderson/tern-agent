//! Gateway connectors shipped with tern (`tern gateway add`): ordinary Luau
//! connectors the user can read and edit once installed.

pub const NTFY: &str = include_str!("../connectors/ntfy.luau");
pub const SLACK: &str = include_str!("../connectors/slack.luau");

/// The built-in gateways' names.
pub const KINDS: &[&str] = &["ntfy", "slack"];

/// Built-in `kind`'s source, pointed at `server` (an ntfy server, or a
/// stand-in for Slack's API in tests) when given: the base URL and the
/// declared host change together, so the allowlist still holds.
pub fn source(kind: &str, server: Option<&str>) -> Result<String, String> {
    let (src, base, host) = match kind {
        "ntfy" => (NTFY, "https://ntfy.sh", "\"ntfy.sh\""),
        "slack" => (SLACK, "https://slack.com/api", "\"slack.com/api/*\""),
        _ => {
            return Err(format!(
                "no built-in gateway {kind:?}; there are {}",
                KINDS.join(" and ")
            ));
        }
    };
    let Some(server) = server else {
        return Ok(src.to_owned());
    };
    let url = reqwest::Url::parse(server).map_err(|e| format!("{server:?}: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{server:?} must be an http(s) URL"));
    }
    let h = url
        .host_str()
        .ok_or_else(|| format!("{server:?} has no host"))?;
    let server = server.trim_end_matches('/');
    let src = src
        .replacen(&format!("\"{base}"), &format!("\"{server}"), 1)
        .replacen(host, &format!("\"{h}\""), 1);
    Ok(src)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_moves_the_base_url_and_the_allowed_host_together() {
        let s = source("ntfy", Some("https://ntfy.example.com/")).unwrap();
        assert!(s.contains("local SERVER = \"https://ntfy.example.com\""));
        assert!(s.contains("http = { \"ntfy.example.com\" }"));
        assert!(!s.contains("ntfy.sh\""));
        assert_eq!(source("ntfy", None).unwrap(), NTFY);
        assert!(source("ntfy", Some("ftp://x")).is_err());
        assert!(source("discord", None).is_err());
        let s = source("slack", Some("http://127.0.0.1:9")).unwrap();
        assert!(s.contains("local API = \"http://127.0.0.1:9/\""));
        assert!(s.contains("http = { \"127.0.0.1\" }"));
    }
}
