//! `capabilities.http` entries: a host (`api.example.com`, `*.example.com`),
//! optionally followed by a path pattern (`api.example.com/v1/*`).

/// The most `*` a path pattern may have.
const MAX_STARS: usize = 8;
const MAX_PATH: usize = 256;

/// An entry's host part.
pub fn entry_host(entry: &str) -> &str {
    entry.split_once('/').map_or(entry, |(h, _)| h)
}

/// An entry's path pattern, with its leading `/`, if it has one.
fn entry_path(entry: &str) -> Option<&str> {
    entry.find('/').map(|i| &entry[i..])
}

fn host_matches(pattern: &str, host: &str) -> bool {
    let (pattern, host) = (pattern.to_ascii_lowercase(), host.to_ascii_lowercase());
    match pattern.strip_prefix("*.") {
        Some(suffix) => host.ends_with(&format!(".{suffix}")),
        None => pattern == host,
    }
}

/// `true` if `host` is allowed by an entry like `api.example.com` or
/// `*.example.com` (with or without a path).
pub fn host_allowed(allowed: &[String], host: &str) -> bool {
    allowed.iter().any(|a| host_matches(entry_host(a), host))
}

/// Whether `path` (a URL's path, percent-encoded as sent) may be requested
/// on `host`. A host with an entry and no path allows every path; otherwise
/// one of its path patterns must match. On a path-restricted host, encoded
/// slashes, backslashes and dots are refused: a server that decodes them
/// could be steered outside the pattern.
pub fn path_allowed(allowed: &[String], host: &str, path: &str) -> bool {
    let mut patterns = Vec::new();
    for a in allowed.iter().filter(|a| host_matches(entry_host(a), host)) {
        match entry_path(a) {
            None => return true,
            Some(p) => patterns.push(p),
        }
    }
    if patterns.is_empty() {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    if ["%2f", "%5c", "%2e", "\\"]
        .iter()
        .any(|bad| lower.contains(bad))
        || path.split('/').any(|seg| seg == "..")
    {
        return false;
    }
    patterns.iter().any(|p| glob(p.as_bytes(), path.as_bytes()))
}

/// `*` matches within one path segment; a trailing `*` matches the rest,
/// slashes included (`/v1/*` covers `/v1/a/b`).
/// Dynamic programming over (pattern suffix, path suffix), so it stays
/// linear in each however the stars fall.
fn glob(p: &[u8], s: &[u8]) -> bool {
    // next[j]: does p[i + 1..] match s[j..]?
    let mut next: Vec<bool> = (0..=s.len()).map(|j| j == s.len()).collect();
    for i in (0..p.len()).rev() {
        let mut cur = vec![false; s.len() + 1];
        for j in (0..=s.len()).rev() {
            cur[j] = match p[i] {
                b'*' if i + 1 == p.len() => true,
                b'*' => next[j] || (j < s.len() && s[j] != b'/' && cur[j + 1]),
                c => j < s.len() && s[j] == c && next[j + 1],
            };
        }
        next = cur;
    }
    next[0]
}

/// Validate one entry; the error says what is wrong with it.
pub fn check_entry(entry: &str) -> Result<(), String> {
    let host = entry_host(entry);
    let bare = host.strip_prefix("*.").unwrap_or(host);
    let valid_host = !bare.is_empty()
        && bare.contains('.')
        && bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        || bare.parse::<std::net::IpAddr>().is_ok()
        || bare == "localhost";
    if !valid_host {
        return Err(format!(
            "capabilities.http entry {entry:?} must be a host name like \"api.example.com\" or \"*.example.com\", optionally with a path like \"api.example.com/v1/*\" (no \"*\" on its own, schemes or ports)"
        ));
    }
    let Some(path) = entry_path(entry) else {
        return Ok(());
    };
    let bad_char = path
        .chars()
        .find(|c| !c.is_ascii_graphic() || matches!(c, '?' | '#' | '%' | '\\'));
    if let Some(c) = bad_char {
        return Err(format!(
            "capabilities.http entry {entry:?}: the path can't contain {c:?} (query strings aren't matched; use \"*\" for any segment)"
        ));
    }
    if path.split('/').any(|seg| seg == "." || seg == "..") {
        return Err(format!(
            "capabilities.http entry {entry:?}: the path can't contain \".\" or \"..\" segments"
        ));
    }
    if path.len() > MAX_PATH || path.matches('*').count() > MAX_STARS {
        return Err(format!(
            "capabilities.http entry {entry:?}: the path is too long or has too many \"*\""
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn host_allowlist() {
        let allowed = list(&["api.linear.app", "*.example.com", "api.github.com/repos/*"]);
        assert!(host_allowed(&allowed, "api.linear.app"));
        assert!(host_allowed(&allowed, "API.Linear.app"));
        assert!(host_allowed(&allowed, "a.b.example.com"));
        assert!(host_allowed(&allowed, "api.github.com"));
        assert!(!host_allowed(&allowed, "example.com"));
        assert!(!host_allowed(&allowed, "api.linear.app.evil.com"));
        assert!(!host_allowed(&allowed, "evillinear.app"));
        assert!(!host_allowed(&[], "anything"));
    }

    #[test]
    fn path_allowlist() {
        let allowed = list(&[
            "api.github.com/repos/*/issues",
            "api.github.com/search/*",
            "api.linear.app",
        ]);
        let ok = |h: &str, p: &str| path_allowed(&allowed, h, p);
        assert!(ok("api.linear.app", "/anything/at/all"));
        assert!(ok("api.github.com", "/repos/tern/issues"));
        assert!(ok("api.github.com", "/search/code"));
        assert!(ok("api.github.com", "/search/a/b"));
        assert!(!ok("api.github.com", "/repos/tern/pulls"));
        assert!(
            !ok("api.github.com", "/repos/a/b/issues"),
            "a middle * is one segment"
        );
        assert!(!ok("api.github.com", "/user"));
        assert!(!ok("api.github.com", "/search"));
        assert!(!ok("api.github.com", "/search/%2e%2e/user"));
        assert!(!ok("api.github.com", "/search/a%2Fb"));
        assert!(!ok("api.github.com", "/search/../user"));
        assert!(!ok("evil.com", "/search/x"));
        // A host-only entry beside a path entry allows every path.
        let both = list(&["x.com/v1/*", "x.com"]);
        assert!(path_allowed(&both, "x.com", "/v2"));
    }

    #[test]
    fn entries_are_checked() {
        for good in [
            "api.linear.app",
            "*.ghost.io",
            "127.0.0.1",
            "localhost",
            "api.x.com/v1",
            "api.x.com/v1/*",
            "*.x.com/a/*/b",
        ] {
            assert!(check_entry(good).is_ok(), "{good:?} should be allowed");
        }
        for bad in [
            "*",
            "*.",
            "https://api.x.com",
            "x.com:443",
            "",
            "/v1",
            "x.com/v1?q=1",
            "x.com/a/../b",
            "x.com/a%2fb",
            "x.com/a b",
        ] {
            assert!(check_entry(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn many_stars_stay_fast() {
        let p = "/*a*a*a*a*a*a*a*b";
        let s = format!("/{}", "a".repeat(200));
        let started = std::time::Instant::now();
        assert!(!glob(p.as_bytes(), s.as_bytes()));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
