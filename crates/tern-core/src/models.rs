//! Model lists and provider setup as clients see them: the shapes the API
//! returns and the TUI and desktop app show (docs/provider-setup-spec.md).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::config::ProviderKind;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// What goes after `provider:`.
    pub id: String,
    /// Display name, when the provider gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Unix seconds, when the provider gives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    /// When the provider lists prices (OpenRouter does), for budgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<crate::config::Pricing>,
}

/// Why a listing failed, in a shape clients can turn into wording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum ListError {
    /// No API key, for a provider that needs one.
    MissingKey,
    /// The base URL is unusable (missing, or not http/https).
    BadUrl(String),
    /// 401: the key was rejected.
    Unauthorized,
    /// 403: the key works but may not list models.
    Forbidden,
    /// 404: the server answered but has no models endpoint.
    NotFound,
    /// 429: the key works but is rate limited.
    RateLimited,
    /// Connection, DNS or TLS failure.
    Unreachable(String),
    /// Any other HTTP status.
    Unexpected(u16),
    /// A 2xx answer we could not parse.
    Malformed(String),
}

impl ListError {
    /// The server answered in a way that shows the key is accepted, even
    /// though there is no list to show.
    ///
    /// A 404 is not one of these: it means the URL is wrong, and saving
    /// it would fail every later model call the same way.
    pub fn key_accepted(&self) -> bool {
        matches!(self, Self::RateLimited)
    }
}

impl fmt::Display for ListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKey => write!(f, "no API key is set"),
            Self::BadUrl(why) => write!(f, "invalid base URL: {why}"),
            Self::Unauthorized => write!(f, "the API key was rejected (HTTP 401)"),
            Self::Forbidden => write!(f, "the key may not list models (HTTP 403)"),
            Self::NotFound => write!(f, "the server has no models endpoint (HTTP 404)"),
            Self::RateLimited => write!(f, "rate limited (HTTP 429)"),
            Self::Unreachable(why) => write!(f, "unreachable: {why}"),
            Self::Unexpected(status) => write!(f, "unexpected HTTP {status}"),
            Self::Malformed(why) => write!(f, "unexpected response: {why}"),
        }
    }
}

impl std::error::Error for ListError {}

/// A change clients should re-read, sent on `/v1/events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    ProvidersChanged {},
    ModelsChanged {
        provider: String,
    },
    /// An MCP server changed state or config; `state` is `None` once it's
    /// removed.
    McpChanged {
        name: String,
        state: Option<crate::mcp::McpState>,
    },
    /// A post was written to a channel's timeline.
    PostCreated {
        /// Boxed: a post is much bigger than the other changes.
        post: Box<crate::channels::Post>,
    },
    /// A post the daemon keeps current was rewritten (a delegation's
    /// pointer as its work goes on).
    PostUpdated {
        /// Boxed, as for `PostCreated`.
        post: Box<crate::channels::Post>,
    },
    /// A channel was created, renamed, archived, or got a new main thread.
    ChannelsChanged {},
    /// An agent profile was created, edited or archived.
    AgentsChanged {},
    /// An outbox item was queued or changed state; re-read `/v1/outbox`.
    OutboxChanged {
        id: i64,
        state: String,
    },
    /// A connection (account) was added, removed, renamed, signed in again
    /// or changed status; re-read `/v1/connections`.
    ConnectionsChanged {},
    /// A routine was created, changed or deleted, by a tool or the API;
    /// re-read `/v1/schedules`.
    SchedulesChanged {
        id: String,
    },
}

/// What a client fills in to add or change a provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProviderDraft {
    /// Required when adding; ignored when changing an existing provider.
    pub kind: Option<ProviderKind>,
    /// Defaults to `anthropic`, or a name suggested from the base URL.
    pub name: Option<String>,
    pub base_url: Option<String>,
    /// A pasted key. Cleaned up with `normalize_key` before use.
    pub key: Option<String>,
    /// Use the key already set in the environment under the secret name.
    #[serde(default)]
    pub use_env_key: bool,
    /// A preset id (`openrouter`, `groq`, ...; see `tern_core::presets`).
    /// Fills in the kind, name and base URL when they are left out.
    #[serde(default)]
    pub preset: Option<String>,
}

/// The result of checking a draft against the provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verified {
    /// The key is accepted (the list may still be empty, e.g. on a 429).
    pub ok: bool,
    pub name: String,
    pub error: Option<ListError>,
    /// The error in plain words, for showing as is.
    pub message: Option<String>,
    /// Advice that doesn't block saving, e.g. an environment override.
    pub warnings: Vec<String>,
    /// Set when `/v1` had to be added to the base URL.
    pub suggested_base_url: Option<String>,
    pub key_hint: Option<String>,
    pub models: Vec<ModelInfo>,
}

/// A configured provider, for `GET /v1/providers`. Never includes a key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    pub secret: Option<String>,
    /// `env`, `file` or `missing`; `none` when no key is configured.
    pub key_source: String,
    pub key_hint: Option<String>,
    /// The friendly preset this provider matches by URL (`openrouter`, ...).
    #[serde(default)]
    pub preset: Option<String>,
    /// What to call it in the UI: the preset's label, else the name.
    #[serde(default)]
    pub label: String,
    /// From the last model listing, if any.
    pub error: Option<ListError>,
    pub message: Option<String>,
    pub is_default: bool,
}

/// One provider's models, for `GET /v1/models`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModels {
    pub provider: String,
    pub error: Option<ListError>,
    pub message: Option<String>,
    /// Unix seconds; `None` when never fetched.
    pub fetched_at: Option<i64>,
    pub models: Vec<ModelInfo>,
    /// Model ids the provider's preset suggests. Clients show them only
    /// when `models` is empty (the list couldn't be fetched).
    #[serde(default)]
    pub suggested: Vec<String>,
    /// What to call the provider in the UI (its preset's label, else its name).
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolved {
    pub model: String,
    pub warning: Option<String>,
}

/// A listing error as a sentence a person can act on (spec §5.3).
pub fn describe(e: &ListError, provider: &str, base: Option<&str>) -> String {
    let host = base.map_or_else(|| provider.to_owned(), host_of);
    match e {
        ListError::MissingKey => format!("{provider} needs an API key."),
        ListError::BadUrl(why) => format!("{why}."),
        ListError::Unauthorized => {
            format!("{provider} rejected that key. Check you copied all of it.")
        }
        ListError::Forbidden => "The key works, but this account can't list models. \
             Check its permissions or billing."
            .into(),
        ListError::RateLimited => "Key works (rate limited right now).".into(),
        ListError::NotFound => format!(
            "{host} didn't recognise that address (HTTP 404). Check the URL; it usually ends in /v1."
        ),
        ListError::Unreachable(why) => {
            let local = host == "localhost" || host.starts_with("127.") || host == "::1";
            let hint = if local { " Is the server running?" } else { "" };
            format!("Couldn't reach {host}: {why}.{hint}")
        }
        ListError::Unexpected(status) => format!("{host} answered HTTP {status}."),
        ListError::Malformed(why) => format!("{host} sent a model list tern can't read: {why}."),
    }
}

/// The host of `url`, without port or IPv6 brackets.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default().to_owned(),
        None => authority.split(':').next().unwrap_or_default().to_owned(),
    }
}

/// A failed model call in plain words (spec section 12.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friendly {
    /// `not_found`, `unauthorized`, `forbidden`, `payment`, `rate_limited`,
    /// `server` or `unreachable`.
    pub code: String,
    /// A sentence a person can act on.
    pub message: String,
    /// The fix is in the provider's settings (key, URL or model name), so
    /// clients can offer "Fix in Models".
    pub fix_in_models: bool,
}

/// Explain a model-call error such as `HTTP 404: {"error":...}` or
/// `network error: ...` for the provider called `label` (a display name,
/// e.g. "OpenRouter"). `None` when the text is not one of those, so the
/// caller shows it as is. The raw text stays available as the details.
pub fn friendly_call_error(raw: &str, label: &str) -> Option<Friendly> {
    let raw = raw.trim();
    let make = |code: &str, message: String, fix: bool| {
        Some(Friendly {
            code: code.into(),
            message,
            fix_in_models: fix,
        })
    };
    if let Some(rest) = raw.strip_prefix("HTTP ") {
        let status: u16 = rest.split(':').next()?.trim().parse().ok()?;
        let body = rest
            .split_once(':')
            .map_or("", |(_, b)| b)
            .to_ascii_lowercase();
        return match status {
            401 => make(
                "unauthorized",
                format!("{label} rejected the API key. Check the key in Models."),
                true,
            ),
            402 => make(
                "payment",
                format!(
                    "{label} says the account is out of credit. Add credit there, then try again."
                ),
                false,
            ),
            403 => make(
                "forbidden",
                format!(
                    "{label} won't let this key use that. Check the key's permissions and the model name."
                ),
                true,
            ),
            404 if body.contains("model") || body.contains("no endpoints found") => make(
                "not_found",
                format!("{label} couldn't find that model. Check the model name."),
                true,
            ),
            404 => make(
                "not_found",
                format!("{label} couldn't find that endpoint or model. Check the model name."),
                true,
            ),
            429 => make(
                "rate_limited",
                format!("{label} is limiting requests right now. Wait a moment and try again."),
                false,
            ),
            500..=599 => make(
                "server",
                format!("{label} had a problem on its side (HTTP {status}). Try again shortly."),
                false,
            ),
            _ => None,
        };
    }
    if raw.starts_with("network error:") {
        return make(
            "unreachable",
            format!(
                "Couldn't reach {label}. Check your connection, or the server address in Models."
            ),
            true,
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_call_errors() {
        let f = friendly_call_error(
            r#"HTTP 404: {"error":{"message":"Not Found","code":404}}"#,
            "OpenRouter",
        )
        .unwrap();
        assert_eq!(f.code, "not_found");
        assert_eq!(
            f.message,
            "OpenRouter couldn't find that endpoint or model. Check the model name."
        );
        assert!(f.fix_in_models);
        let m = friendly_call_error(
            r#"HTTP 404: {"error":{"message":"No endpoints found for x/y"}}"#,
            "OpenRouter",
        );
        assert!(m.unwrap().message.contains("find that model"));
        let code = |t: &str| friendly_call_error(t, "X").map(|f| (f.code, f.fix_in_models));
        assert_eq!(code("HTTP 401: nope"), Some(("unauthorized".into(), true)));
        assert_eq!(code("HTTP 429: slow"), Some(("rate_limited".into(), false)));
        assert_eq!(code("HTTP 503: down"), Some(("server".into(), false)));
        assert_eq!(code("HTTP 402: pay"), Some(("payment".into(), false)));
        assert_eq!(
            code("network error: dns error"),
            Some(("unreachable".into(), true))
        );
        assert_eq!(code("HTTP 400: bad request"), None);
        assert_eq!(code("tool failed"), None);
    }

    #[test]
    fn a_404_does_not_count_as_a_working_key() {
        assert!(!ListError::NotFound.key_accepted());
        assert!(ListError::RateLimited.key_accepted());
    }
}
