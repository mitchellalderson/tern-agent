//! What the agent should do about a failed connector call: the host maps
//! the status to the next step, so the model doesn't guess (or send the
//! user to settings).

/// Where a rejected key lives: a plain secret, or one of a connection's.
pub struct KeyHome<'a> {
    pub name: &'a str,
    /// `(target, slug)` of a non-default connection.
    pub connection: Option<(&'a str, &'a str)>,
}

/// The hint for a rejected or rate-limited response from a service called
/// with API keys `keys`. `None` for other statuses.
pub fn call_hint(
    status: u16,
    keys: &[KeyHome<'_>],
    retry_after_secs: Option<u64>,
) -> Option<String> {
    match status {
        401 | 403 if !keys.is_empty() => Some(rejected_key(status, keys)),
        429 => Some(rate_limited(retry_after_secs)),
        _ => None,
    }
}

fn rejected_key(status: u16, keys: &[KeyHome<'_>]) -> String {
    let what = if status == 401 {
        "rejected the key"
    } else {
        "refused the key (wrong, expired or without access)"
    };
    let mut plain: Vec<String> = Vec::new();
    let mut conns: Vec<String> = Vec::new();
    for k in keys {
        match k.connection {
            Some((target, slug)) => conns.push(format!(
                "connection_add with target {target:?}, connection {slug:?} and method \"key\" so the user can enter a new {}",
                k.name
            )),
            None => plain.push(format!("{:?}", k.name)),
        }
    }
    let mut steps = Vec::new();
    if !plain.is_empty() {
        steps.push(format!(
            "secret_request with names [{}], replace true and a short reason, so the user can enter a new one in the private prompt",
            plain.join(", ")
        ));
    }
    steps.extend(conns);
    format!(
        "The service {what} (HTTP {status}). Call {}. Don't retry with the same key, and never ask for it in the chat.",
        steps.join("; or ")
    )
}

fn rate_limited(retry_after_secs: Option<u64>) -> String {
    match retry_after_secs {
        Some(s) => format!(
            "The service is rate limiting this connector (HTTP 429). Wait about {s}s before calling it again; don't retry in a loop."
        ),
        None => "The service is rate limiting this connector (HTTP 429). Wait a minute before calling it again; don't retry in a loop.".to_owned(),
    }
}

/// Hint for a failed fetch of an API description (an OpenAPI document).
pub fn fetch_hint(status: u16) -> &'static str {
    match status {
        401 | 403 => "It needs access: use a public URL for the API description.",
        404 | 410 => "Not found: check the URL of the API description.",
        429 => "Rate limited: wait a minute, then try again.",
        500..=599 => "The server had a problem: try again later.",
        _ => "Check the URL of the API description.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(name: &str) -> KeyHome<'_> {
        KeyHome {
            name,
            connection: None,
        }
    }

    #[test]
    fn a_rejected_key_says_to_replace_it() {
        for status in [401, 403] {
            let h = call_hint(status, &[plain("API_KEY")], None).unwrap();
            assert!(h.contains("secret_request"), "{h}");
            assert!(
                h.contains("replace true") && h.contains("\"API_KEY\""),
                "{h}"
            );
            assert!(h.contains(&format!("HTTP {status}")), "{h}");
        }
    }

    #[test]
    fn a_connections_key_goes_through_connection_add() {
        let k = KeyHome {
            name: "API_KEY",
            connection: Some(("hubspot", "work")),
        };
        let h = call_hint(401, &[k], None).unwrap();
        assert!(
            h.contains("connection_add") && h.contains("\"work\""),
            "{h}"
        );
        assert!(!h.contains("secret_request"), "{h}");
    }

    #[test]
    fn rate_limits_say_to_wait_and_other_statuses_say_nothing() {
        assert!(call_hint(429, &[], Some(30)).unwrap().contains("30s"));
        assert!(call_hint(429, &[], None).unwrap().contains("Wait a minute"));
        // No API key sent (open endpoint, or OAuth): nothing to replace.
        assert_eq!(call_hint(401, &[], None), None);
        assert_eq!(call_hint(404, &[plain("K")], None), None);
        assert_eq!(call_hint(500, &[plain("K")], None), None);
    }

    #[test]
    fn fetch_hints_cover_common_statuses() {
        assert!(fetch_hint(404).contains("Not found"));
        assert!(fetch_hint(503).contains("later"));
    }
}
