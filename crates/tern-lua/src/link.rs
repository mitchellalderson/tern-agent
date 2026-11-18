//! Sign-in links returned by connector tools. A tool may return
//! `sign_in = { url, label, expires_at }` (say, a link that lets the user
//! finish connecting an app). The host checks it and shows it as a card;
//! the model never sees the URL, only that a card was shown.
//!
//! A link is refused unless it is https, carries no embedded credentials,
//! and is on the same registrable domain as a host the connector declares in
//! `capabilities.http`. The card shows the URL's real domain.

use serde_json::Value;

use crate::spec::Capabilities;

/// Longest a link may say it lives (a day), and the longest the card waits.
const MAX_LIFE_MS: i64 = 24 * 60 * 60 * 1000;

/// A checked sign-in link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInLink {
    pub url: String,
    pub label: String,
    /// The URL's host, as the card shows it.
    pub domain: String,
    /// Unix milliseconds.
    pub expires_at: Option<i64>,
}

/// What a tool's result held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extracted {
    /// No `sign_in` key.
    None,
    Link(SignInLink),
    /// A `sign_in` the host refused, and why (safe to tell the model).
    Refused(String),
}

/// Take `sign_in` out of a tool's output. The returned text is what the model
/// may see: the rest of the result, without the link.
pub fn take_sign_in(caps: &Capabilities, out: &str, now_ms: i64) -> (String, Extracted) {
    let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(out) else {
        return (out.to_owned(), Extracted::None);
    };
    let Some(raw) = map.remove("sign_in") else {
        return (out.to_owned(), Extracted::None);
    };
    let rest = if map.is_empty() {
        String::new()
    } else {
        serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_default()
    };
    match check(caps, &raw, now_ms) {
        Ok(link) => (rest, Extracted::Link(link)),
        Err(why) => (rest, Extracted::Refused(why)),
    }
}

fn check(caps: &Capabilities, v: &Value, now_ms: i64) -> Result<SignInLink, String> {
    let Value::Object(o) = v else {
        return Err("sign_in must be a table with url, label and expires_at".into());
    };
    let url = o
        .get("url")
        .and_then(Value::as_str)
        .ok_or("sign_in.url must be a string")?;
    let label = o
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.chars().count() <= 80 && !l.chars().any(char::is_control))
        .ok_or("sign_in.label must be 1-80 characters of plain text")?;
    let expires_at = match o.get("expires_at") {
        None | Some(Value::Null) => None,
        Some(n) => {
            let n = n.as_f64().ok_or("sign_in.expires_at must be a unix time")?;
            // Seconds or milliseconds.
            let ms = if n < 1e11 { n * 1000.0 } else { n } as i64;
            if ms <= now_ms {
                return Err("sign_in.expires_at is already past".into());
            }
            if ms - now_ms > MAX_LIFE_MS {
                return Err("sign_in.expires_at is more than a day away".into());
            }
            Some(ms)
        }
    };
    let hosts: Vec<String> = caps
        .http
        .iter()
        .map(|e| crate::allow::entry_host(e).to_owned())
        .collect();
    // https, no embedded credentials, same site as a declared host. A known
    // identity provider is not enough: the connector must declare the site.
    match tern_auth::domain::check_endpoint("sign-in link", url, &hosts) {
        Ok(tern_auth::domain::Relation::SameSite) => {}
        Ok(_) => {
            return Err(
                "the sign-in link must be on the domain of a host in capabilities.http".into(),
            );
        }
        Err(e) => return Err(e),
    }
    let domain = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .ok_or("the sign-in link has no host")?;
    Ok(SignInLink {
        url: url.to_owned(),
        label: label.to_owned(),
        domain,
        expires_at,
    })
}

/// Show `link` as a card and wait for the answer, until it expires. The
/// text returned is what the model learns: never the URL.
pub async fn present(
    host: &crate::Host,
    ctx: &tern_core::tool::ToolCtx,
    connector: &str,
    link: &SignInLink,
) -> String {
    use tern_core::approval::{AuthCard, AuthPrompt};
    let Some(asker) = host.sign_in.clone() else {
        return "[a sign-in link was returned, but there is nobody to show it to]".into();
    };
    let card = AuthCard::Link {
        title: link.label.clone(),
        url: link.url.clone(),
        domain: link.domain.clone(),
        expires_at: link.expires_at,
        notes: vec![format!(
            "From connector {connector}. The page is on {}.",
            link.domain
        )],
    };
    let req = AuthPrompt {
        id: format!("auth_{}", tern_auth::pkce::random(9)),
        run_id: ctx.run_id.clone(),
        session_id: ctx.session_id.clone(),
        call_id: tern_core::tool::current_call_id(),
        parent_run_id: None,
        agent_label: None,
        card,
    };
    let wait = link
        .expires_at
        .map(|at| (at - tern_store::now_ms()).max(0) as u64)
        .map_or(MAX_WAIT, |ms| {
            std::time::Duration::from_millis(ms).min(MAX_WAIT)
        });
    let answer = tokio::select! {
        a = tokio::time::timeout(wait, asker.ask_auth(&req)) => a,
        () = ctx.cancel.cancelled() => return "[cancelled]".into(),
    };
    match answer {
        Err(_) => "[the sign-in link expired before the user finished]".into(),
        Ok(Some(a)) if a == "done" => {
            "[the user says they finished signing in; check with a read before relying on it]"
                .into()
        }
        Ok(Some(_)) => "[the user cancelled the sign-in]".into(),
        Ok(None) => "[a sign-in link was returned, but nobody could be asked]".into(),
    }
}

/// The most the card waits for an answer.
const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Capabilities {
        Capabilities {
            http: vec!["api.acme.com/v1/*".into()],
            ..Capabilities::default()
        }
    }

    fn take(sign_in: &str) -> (String, Extracted) {
        let out = format!(r#"{{"ok": true, "sign_in": {sign_in}}}"#);
        take_sign_in(&caps(), &out, 1_000_000_000_000)
    }

    #[test]
    fn a_good_link_is_taken_out_of_the_result() {
        let (rest, got) = take(
            r#"{"url": "https://login.acme.com/connect?t=1", "label": "Connect Acme", "expires_at": 1000000600}"#,
        );
        assert!(!rest.contains("login.acme.com"), "{rest}");
        assert!(rest.contains("\"ok\""));
        let Extracted::Link(l) = got else {
            panic!("{got:?}")
        };
        assert_eq!(l.domain, "login.acme.com");
        assert_eq!(l.expires_at, Some(1_000_000_600_000));
    }

    #[test]
    fn bad_links_are_refused_and_never_reach_the_model() {
        for (bad, why) in [
            (
                r#"{"url": "http://login.acme.com/x", "label": "L"}"#,
                "https",
            ),
            (
                r#"{"url": "https://user:pw@login.acme.com/x", "label": "L"}"#,
                "user name",
            ),
            (
                r#"{"url": "https://acme.com.evil.com/x", "label": "L"}"#,
                "not on the API's domain",
            ),
            (
                r#"{"url": "https://accounts.google.com/x", "label": "L"}"#,
                "domain of a host",
            ),
            (
                r#"{"url": "https://login.acme.com/x", "label": ""}"#,
                "label",
            ),
            (
                r#"{"url": "https://login.acme.com/x", "label": "L", "expires_at": 5}"#,
                "past",
            ),
            (r#""https://login.acme.com/x""#, "table"),
        ] {
            let (rest, got) = take(bad);
            let Extracted::Refused(e) = got else {
                panic!("{bad} => {got:?}")
            };
            assert!(e.contains(why), "{bad}: {e}");
            assert!(!rest.contains("acme") && !e.contains("pw@"), "{rest} {e}");
        }
    }

    #[test]
    fn results_without_a_link_pass_through() {
        let (rest, got) = take_sign_in(&caps(), "plain text", 0);
        assert_eq!((rest.as_str(), got), ("plain text", Extracted::None));
        let (rest, got) = take_sign_in(&caps(), r#"{"a": 1}"#, 0);
        assert_eq!((rest.as_str(), got), (r#"{"a": 1}"#, Extracted::None));
    }
}
