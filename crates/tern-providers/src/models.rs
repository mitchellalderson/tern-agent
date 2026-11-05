//! Listing a provider's models: the picker's source, and `tern doctor`'s
//! reachability probe.

use std::time::Duration;

use serde_json::Value;
use tern_core::config::{ProviderConfig, ProviderKind};
pub use tern_core::models::{ListError, ModelInfo};

const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Stop following Anthropic's pagination after this many pages.
const MAX_PAGES: usize = 20;
/// Ids containing any of these are not chat models (OpenAI's `/models`
/// lists embedding, audio and image models alongside chat ones).
const NOT_CHAT: &[&str] = &[
    "embed",
    "tts",
    "whisper",
    "dall-e",
    "moderation",
    "transcribe",
    "image",
];

/// A successful listing.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub models: Vec<ModelInfo>,
    /// Set when `base_url` had no version path and adding `/v1` worked.
    pub suggested_base_url: Option<String>,
}

/// List the chat models `cfg` offers, newest first where the provider says.
pub async fn list_models(cfg: &ProviderConfig, key: Option<&str>) -> Result<Listing, ListError> {
    let base = cfg
        .effective_base_url()
        .ok_or_else(|| ListError::BadUrl("base_url is required".into()))?;
    let tidy;
    let base = match cfg.kind {
        ProviderKind::OpenaiCompat => {
            tidy = tern_core::presets::normalize_base_url(base);
            tidy.as_str()
        }
        ProviderKind::Anthropic => base,
    };
    check_url(base)?;
    let client = tern_core::net::http_client()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| ListError::Unreachable(format!("http client: {e}")))?;
    match cfg.kind {
        ProviderKind::Anthropic => {
            let key = key.ok_or(ListError::MissingKey)?;
            let models = anthropic(&client, base, key).await?;
            Ok(Listing {
                models,
                suggested_base_url: None,
            })
        }
        ProviderKind::OpenaiCompat => match openai_compat(&client, base, key).await {
            Err(ListError::NotFound) if !has_version_path(base) => {
                let retry = format!("{base}/v1");
                // `/v1` answering at all (even 401) means it is the API; its
                // error is the one that matters.
                let models = match openai_compat(&client, &retry, key).await {
                    Err(ListError::Unreachable(_) | ListError::Malformed(_)) => {
                        return Err(ListError::NotFound);
                    }
                    other => other?,
                };
                Ok(Listing {
                    models,
                    suggested_base_url: Some(retry),
                })
            }
            other => other.map(|models| Listing {
                models,
                suggested_base_url: None,
            }),
        },
    }
}

/// `http` only for loopback and private networks, so a key is never sent
/// in cleartext over the internet by mistake.
pub fn check_url(url: &str) -> Result<(), ListError> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| ListError::BadUrl(format!("{url:?} has no http:// or https://")))?;
    let host = host_of(rest);
    if host.is_empty() {
        return Err(ListError::BadUrl(format!("{url:?} has no host")));
    }
    match scheme {
        "https" => Ok(()),
        "http" if is_private_host(host) => Ok(()),
        "http" => Err(ListError::BadUrl(format!(
            "use https:// for {host}; http:// is only allowed for local and private-network servers"
        ))),
        _ => Err(ListError::BadUrl(format!(
            "{url:?} must start with http:// or https://"
        ))),
    }
}

/// The host part of `host[:port][/path]`, without IPv6 brackets.
fn host_of(rest: &str) -> &str {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default();
    if let Some(v6) = authority.strip_prefix('[') {
        return v6.split(']').next().unwrap_or_default();
    }
    authority.split(':').next().unwrap_or_default()
}

fn is_private_host(host: &str) -> bool {
    use std::net::IpAddr;
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
        Err(_) => false,
    }
}

/// Whether the URL's last path segment looks like `v1`, `v1beta`, …
fn has_version_path(base: &str) -> bool {
    let path = base.split_once("://").map_or(base, |(_, r)| r);
    let Some((_, path)) = path.split_once('/') else {
        return false;
    };
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|seg| seg.strip_prefix('v'))
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
}

async fn anthropic(
    client: &reqwest::Client,
    base: &str,
    key: &str,
) -> Result<Vec<ModelInfo>, ListError> {
    let mut models = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        // Model ids are `[A-Za-z0-9._-]`, so they need no escaping.
        let url = match &after {
            Some(id) => format!("{base}/v1/models?limit=1000&after_id={id}"),
            None => format!("{base}/v1/models?limit=1000"),
        };
        let req = client
            .get(url)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("x-api-key", key);
        let page = get_json(req).await?;
        let data = page["data"]
            .as_array()
            .ok_or_else(|| ListError::Malformed("no `data` array".into()))?;
        models.extend(data.iter().filter_map(|m| {
            Some(ModelInfo {
                id: m["id"].as_str()?.to_owned(),
                name: m["display_name"].as_str().map(str::to_owned),
                context_window: m["max_input_tokens"].as_u64(),
                created: None,
                pricing: None,
            })
        }));
        match (page["has_more"].as_bool(), page["last_id"].as_str()) {
            (Some(true), Some(last)) if last.bytes().all(is_id_byte) => {
                after = Some(last.to_owned());
            }
            _ => break,
        }
    }
    // Anthropic already returns newest first.
    Ok(models)
}

async fn openai_compat(
    client: &reqwest::Client,
    base: &str,
    key: Option<&str>,
) -> Result<Vec<ModelInfo>, ListError> {
    let mut req = client.get(format!("{base}/models"));
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    let body = get_json(req).await?;
    let data = body["data"]
        .as_array()
        .ok_or_else(|| ListError::Malformed("no `data` array".into()))?;
    let mut models: Vec<ModelInfo> = data
        .iter()
        .filter_map(|m| {
            Some(ModelInfo {
                id: m["id"].as_str()?.to_owned(),
                name: m["name"].as_str().map(str::to_owned),
                context_window: m["context_length"].as_u64(),
                created: m["created"].as_i64(),
                pricing: pricing(&m["pricing"]),
            })
        })
        .filter(|m| is_chat_model(&m.id))
        .collect();
    models.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)));
    Ok(models)
}

/// OpenRouter's `pricing`: USD per token, as strings (`"0.000003"`).
fn pricing(p: &Value) -> Option<tern_core::config::Pricing> {
    let per_mtok = |key: &str| -> Option<f64> {
        let v = &p[key];
        let n = v
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| v.as_f64())?;
        (n >= 0.0 && n.is_finite()).then_some(n * 1_000_000.0)
    };
    Some(tern_core::config::Pricing {
        input: per_mtok("prompt")?,
        output: per_mtok("completion")?,
        cache_read: per_mtok("input_cache_read"),
        cache_write: per_mtok("input_cache_write"),
    })
}

fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

/// False for ids that are clearly embedding, audio, image or moderation
/// models.
pub fn is_chat_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    !NOT_CHAT.iter().any(|s| id.contains(s))
}

async fn get_json(req: reqwest::RequestBuilder) -> Result<Value, ListError> {
    let resp = req
        .send()
        .await
        .map_err(|e| ListError::Unreachable(root_cause(&e)))?;
    match resp.status().as_u16() {
        200..=299 => {}
        401 => return Err(ListError::Unauthorized),
        403 => return Err(ListError::Forbidden),
        404 => return Err(ListError::NotFound),
        429 => return Err(ListError::RateLimited),
        other => return Err(ListError::Unexpected(other)),
    }
    resp.json()
        .await
        .map_err(|e| ListError::Malformed(root_cause(&e)))
}

fn root_cause(e: &(dyn std::error::Error + 'static)) -> String {
    let mut cur = e;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{bearer_token, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cfg(kind: ProviderKind, base: &str) -> ProviderConfig {
        ProviderConfig {
            kind,
            base_url: Some(base.into()),
            api_key_secret: None,
            context_window: None,
            prices: Default::default(),
        }
    }

    #[tokio::test]
    async fn anthropic_follows_pages() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "k"))
            .and(query_param("after_id", "b"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id": "c"}], "has_more": false, "last_id": "c"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "a", "display_name": "Model A", "max_input_tokens": 200000},
                    {"id": "b"}
                ],
                "has_more": true, "last_id": "b"
            })))
            .mount(&server)
            .await;
        let l = list_models(&cfg(ProviderKind::Anthropic, &server.uri()), Some("k"))
            .await
            .unwrap();
        let ids: Vec<_> = l.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(l.models[0].name.as_deref(), Some("Model A"));
        assert_eq!(l.models[0].context_window, Some(200_000));
    }

    #[tokio::test]
    async fn anthropic_needs_a_key() {
        let err = list_models(&cfg(ProviderKind::Anthropic, "https://example.com"), None)
            .await
            .unwrap_err();
        assert_eq!(err, ListError::MissingKey);
    }

    #[tokio::test]
    async fn openai_compat_filters_and_sorts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(bearer_token("k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "old", "created": 1},
                    {"id": "text-embedding-3-small", "created": 9},
                    {"id": "new", "created": 5, "name": "New", "context_length": 8000},
                    {"id": "whisper-1", "created": 3},
                    {"id": "b-undated"},
                    {"id": "a-undated"}
                ]
            })))
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        let l = list_models(&cfg(ProviderKind::OpenaiCompat, &base), Some("k"))
            .await
            .unwrap();
        let ids: Vec<_> = l.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["new", "old", "a-undated", "b-undated"]);
        assert_eq!(l.models[0].context_window, Some(8000));
        assert_eq!(l.suggested_base_url, None);
    }

    #[tokio::test]
    async fn openai_compat_retries_with_v1() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "m"}]})))
            .mount(&server)
            .await;
        let l = list_models(&cfg(ProviderKind::OpenaiCompat, &server.uri()), None)
            .await
            .unwrap();
        assert_eq!(l.models.len(), 1);
        assert_eq!(l.suggested_base_url, Some(format!("{}/v1", server.uri())));
    }

    #[tokio::test]
    async fn retry_with_v1_reports_its_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let err = list_models(&cfg(ProviderKind::OpenaiCompat, &server.uri()), Some("bad"))
            .await
            .unwrap_err();
        assert_eq!(err, ListError::Unauthorized);
    }

    #[tokio::test]
    async fn maps_statuses() {
        for (status, want) in [
            (401, ListError::Unauthorized),
            (403, ListError::Forbidden),
            (429, ListError::RateLimited),
            (500, ListError::Unexpected(500)),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let base = format!("{}/v1", server.uri());
            let err = list_models(&cfg(ProviderKind::OpenaiCompat, &base), Some("k"))
                .await
                .unwrap_err();
            assert_eq!(err, want, "HTTP {status}");
        }
    }

    #[tokio::test]
    async fn missing_endpoint_is_not_found() {
        let server = MockServer::start().await;
        let base = format!("{}/v1", server.uri());
        let err = list_models(&cfg(ProviderKind::OpenaiCompat, &base), None)
            .await
            .unwrap_err();
        assert_eq!(err, ListError::NotFound);
        // A 404 means the address is wrong; it must not save as "working".
        assert!(!err.key_accepted());
    }

    #[test]
    fn urls() {
        assert!(check_url("https://api.openai.com/v1").is_ok());
        assert!(check_url("http://localhost:11434/v1").is_ok());
        assert!(check_url("http://127.0.0.1:1234/v1").is_ok());
        assert!(check_url("http://192.168.1.5/v1").is_ok());
        assert!(check_url("http://[::1]:8080").is_ok());
        assert!(check_url("http://api.openai.com/v1").is_err());
        assert!(check_url("api.openai.com").is_err());
        assert!(check_url("ftp://localhost").is_err());
        assert!(has_version_path("https://x.com/api/v1"));
        assert!(has_version_path("https://x.com/v1beta/"));
        assert!(!has_version_path("https://x.com"));
        assert!(!has_version_path("https://x.com/api"));
    }
}
