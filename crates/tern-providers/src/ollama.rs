//! What tern asks Ollama's own API (`/api/*`), beside the OpenAI-compatible
//! `/v1` it chats over: its version, what each model can do and how much
//! context it really has. `/v1/models` says none of that, and Ollama cuts a
//! prompt that is too long without an error, so tern has to know.
//! See docs/research/local-models.md §1.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::{Value, json};
use tern_core::models::ModelInfo;

/// The first Ollama with `capabilities` in `/api/show`.
pub const MIN_VERSION: (u64, u64, u64) = (0, 6, 4);
/// tern's agent loop needs at least this much context (Ollama's advice).
pub const MIN_CONTEXT: u64 = 65_536;
/// How long to keep a model loaded after tern used it.
pub const KEEP_ALIVE: &str = "30m";
/// Most models asked about in one listing.
const MAX_SHOWN: usize = 40;

/// Whether `base_url` is the Ollama everyone runs: this computer, port 11434.
pub fn is_ollama_url(base_url: &str) -> bool {
    let rest = base_url.split_once("://").map_or(base_url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or("");
    authority
        .rsplit_once(':')
        .is_some_and(|(_, p)| p == "11434")
        && tern_core::limits::is_local_url(base_url)
}

/// The server root for an OpenAI-compatible base: `http://host:11434/v1`
/// becomes `http://host:11434`.
pub fn native_base(base_url: &str) -> String {
    let b = base_url.trim_end_matches('/');
    b.strip_suffix("/v1").unwrap_or(b).to_owned()
}

/// `0.6.4`, `v0.6.4-rc1` and the like. `None` if it isn't a version.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>());
    let major = parts.next()?.ok()?;
    let minor = parts.next().unwrap_or(Ok(0)).ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

pub fn version_too_old(v: (u64, u64, u64)) -> bool {
    v < MIN_VERSION
}

/// The context Ollama gives a model when nothing sets one: 4k under 24 GiB
/// of memory, 32k up to 48 GiB, 256k above ([Ollama docs]).
///
/// [Ollama docs]: https://docs.ollama.com/context-length
pub fn default_context(ram_bytes: u64) -> u64 {
    const GIB: u64 = 1 << 30;
    match ram_bytes {
        r if r < 24 * GIB => 4_096,
        r if r < 48 * GIB => 32_768,
        _ => 262_144,
    }
}

/// What `/api/show` says about one model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shown {
    /// The model's own limit, from `model_info["<arch>.context_length"]`.
    pub max_context: Option<u64>,
    /// A `num_ctx` baked into the model's Modelfile.
    pub num_ctx: Option<u64>,
    /// `capabilities` listed `tools`. `None` when the server doesn't say
    /// (an old Ollama).
    pub tools: Option<bool>,
    pub thinking: Option<bool>,
}

pub fn parse_show(v: &Value) -> Shown {
    let max_context = v["model_info"].as_object().and_then(|m| {
        m.iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, n)| n.as_u64())
    });
    // `parameters` is the Modelfile's settings as text, one per line.
    let num_ctx = v["parameters"].as_str().and_then(|p| {
        p.lines().find_map(|l| {
            let mut it = l.split_whitespace();
            (it.next() == Some("num_ctx"))
                .then(|| it.next().and_then(|n| n.parse().ok()))
                .flatten()
        })
    });
    let caps = v["capabilities"]
        .as_array()
        .map(|a| a.iter().filter_map(|c| c.as_str()).collect::<BTreeSet<_>>());
    Shown {
        max_context,
        num_ctx,
        tools: caps.as_ref().map(|c| c.contains("tools")),
        thinking: caps.as_ref().map(|c| c.contains("thinking")),
    }
}

/// Loaded models and the context each was loaded with, from `/api/ps`.
pub fn parse_ps(v: &Value) -> BTreeMap<String, u64> {
    v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let name = m["name"].as_str().or_else(|| m["model"].as_str())?;
            Some((name.to_owned(), m["context_length"].as_u64()?))
        })
        .collect()
}

/// `model` matches `listed` with or without the implicit `:latest` tag.
fn same_model(listed: &str, other: &str) -> bool {
    let norm = |s: &str| s.strip_suffix(":latest").unwrap_or(s).to_owned();
    norm(listed) == norm(other)
}

/// The context a model gets: what it was loaded with, else its Modelfile,
/// else the server default, and never more than the model supports.
pub fn effective_context(
    loaded: Option<u64>,
    shown: &Shown,
    server_default: Option<u64>,
) -> Option<u64> {
    let ctx = loaded.or(shown.num_ctx).or(server_default)?;
    Some(shown.max_context.map_or(ctx, |m| ctx.min(m)))
}

/// Plain words for a context below [`MIN_CONTEXT`].
pub fn small_context_warning(ctx: u64) -> String {
    format!(
        "Ollama gives models {} tokens of context, and tern needs {}. \
         Longer prompts are cut without an error. Set OLLAMA_CONTEXT_LENGTH=65536 and restart Ollama.",
        group(ctx),
        group(MIN_CONTEXT)
    )
}

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn too_old_warning(v: (u64, u64, u64)) -> String {
    format!(
        "This Ollama ({}.{}.{}) is too old. Update to {}.{}.{} or newer.",
        v.0, v.1, v.2, MIN_VERSION.0, MIN_VERSION.1, MIN_VERSION.2
    )
}

async fn get(client: &reqwest::Client, url: String) -> Option<Value> {
    let r = client.get(url).send().await.ok()?;
    r.status().is_success().then_some(())?;
    r.json().await.ok()
}

/// The server's version, if `base` (an OpenAI-compatible base URL) is Ollama.
pub async fn version(client: &reqwest::Client, base: &str) -> Option<(u64, u64, u64)> {
    let v = get(client, format!("{}/api/version", native_base(base))).await?;
    parse_version(v["version"].as_str()?)
}

/// Fill in what only Ollama's own API knows: each model's context and tool
/// support. Returns advice for the person: a too-old server, a context below
/// what tern needs. Never fails; a model it can't ask about is left as is.
pub async fn enrich(
    client: &reqwest::Client,
    base: &str,
    models: &mut [ModelInfo],
    ram_bytes: Option<u64>,
) -> Vec<String> {
    let root = native_base(base);
    let mut warnings = Vec::new();
    if let Some(v) = version(client, base).await
        && version_too_old(v)
    {
        warnings.push(too_old_warning(v));
    }
    let loaded = get(client, format!("{root}/api/ps"))
        .await
        .map(|v| parse_ps(&v))
        .unwrap_or_default();
    // The server's own default only describes a server on this computer.
    let server_default = ram_bytes
        .filter(|_| tern_core::limits::is_local_url(base))
        .map(default_context);
    let shows = futures::future::join_all(models.iter().take(MAX_SHOWN).map(|m| {
        let client = client.clone();
        let url = format!("{root}/api/show");
        let name = m.id.clone();
        async move {
            let r = client
                .post(url)
                .json(&json!({ "model": name }))
                .send()
                .await
                .ok()?;
            r.status().is_success().then_some(())?;
            r.json::<Value>().await.ok()
        }
    }))
    .await;
    let mut smallest: Option<u64> = None;
    for (i, m) in models.iter_mut().enumerate() {
        let shown = shows
            .get(i)
            .and_then(Option::as_ref)
            .map(parse_show)
            .unwrap_or_default();
        let ctx = loaded
            .iter()
            .find(|(n, _)| same_model(n, &m.id))
            .map(|(_, c)| *c);
        m.tools = shown.tools;
        m.max_context = shown.max_context;
        // Only chat models count towards the warning: an embedding model's
        // small window is fine.
        if let Some(c) = effective_context(ctx, &shown, server_default) {
            m.context_window = Some(c);
            if shown.tools != Some(false) {
                smallest = Some(smallest.map_or(c, |s| s.min(c)));
            }
        }
    }
    if let Some(c) = smallest.filter(|c| *c < MIN_CONTEXT) {
        warnings.push(small_context_warning(c));
    }
    warnings
}

/// Keep `model` loaded for [`KEEP_ALIVE`]: an empty generate request with
/// `keep_alive`, which `/v1` can't carry. Best effort.
pub async fn keep_warm(client: &reqwest::Client, base: &str, model: &str) {
    let _ = client
        .post(format!("{}/api/generate", native_base(base)))
        .timeout(Duration::from_secs(10))
        .json(&json!({ "model": model, "keep_alive": KEEP_ALIVE }))
        .send()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const GIB: u64 = 1 << 30;

    #[test]
    fn recognises_the_usual_ollama_address_only() {
        assert!(is_ollama_url("http://localhost:11434/v1"));
        assert!(is_ollama_url("http://127.0.0.1:11434"));
        assert!(!is_ollama_url("http://localhost:1234/v1"));
        assert!(!is_ollama_url("https://example.com:11434/v1"));
        assert_eq!(
            native_base("http://localhost:11434/v1/"),
            "http://localhost:11434"
        );
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("0.6.4"), Some((0, 6, 4)));
        assert_eq!(parse_version("v0.12.0-rc1"), Some((0, 12, 0)));
        assert_eq!(parse_version("nope"), None);
        assert!(version_too_old((0, 5, 13)));
        assert!(!version_too_old((0, 6, 4)));
        assert!(!version_too_old((0, 40, 16)));
    }

    #[test]
    fn default_context_follows_memory() {
        assert_eq!(default_context(16 * GIB), 4_096);
        assert_eq!(default_context(32 * GIB), 32_768);
        assert_eq!(default_context(64 * GIB), 262_144);
    }

    #[test]
    fn reads_show_and_ps() {
        let s = parse_show(&json!({
            "capabilities": ["completion", "tools"],
            "model_info": { "general.architecture": "qwen3", "qwen3.context_length": 40960 },
            "parameters": "stop \"<|im_end|>\"\nnum_ctx 16384\n",
        }));
        assert_eq!(s.max_context, Some(40_960));
        assert_eq!(s.num_ctx, Some(16_384));
        assert_eq!(s.tools, Some(true));
        assert_eq!(s.thinking, Some(false));
        // An old server lists no capabilities: unknown, not "no".
        assert_eq!(parse_show(&json!({})).tools, None);
        let ps = parse_ps(&json!({ "models": [{ "name": "a:latest", "context_length": 8192 }] }));
        assert_eq!(ps["a:latest"], 8_192);
    }

    #[test]
    fn effective_context_prefers_what_is_loaded_and_never_exceeds_the_model() {
        let shown = Shown {
            max_context: Some(32_768),
            num_ctx: Some(16_384),
            ..Shown::default()
        };
        assert_eq!(
            effective_context(Some(8_192), &shown, Some(4_096)),
            Some(8_192)
        );
        assert_eq!(effective_context(None, &shown, Some(4_096)), Some(16_384));
        assert_eq!(
            effective_context(None, &Shown::default(), Some(4_096)),
            Some(4_096)
        );
        assert_eq!(effective_context(Some(65_536), &shown, None), Some(32_768));
        assert_eq!(effective_context(None, &Shown::default(), None), None);
    }

    #[test]
    fn warning_words() {
        assert!(small_context_warning(4_096).contains("4,096 tokens"));
        assert!(small_context_warning(4_096).contains("65,536"));
        assert!(too_old_warning((0, 5, 0)).contains("0.5.0"));
    }

    fn model(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            ..ModelInfo::default()
        }
    }

    async fn serve(show: Value, ps: Value, version: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": version })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ps))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/show"))
            .respond_with(ResponseTemplate::new(200).set_body_json(show))
            .mount(&server)
            .await;
        server
    }

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[tokio::test]
    async fn enrich_reads_context_and_tools_and_warns_below_64k() {
        let server = serve(
            json!({
                "capabilities": ["completion", "tools"],
                "model_info": { "llama.context_length": 131072 },
            }),
            json!({ "models": [] }),
            "0.9.0",
        )
        .await;
        let mut models = vec![model("llama3.1:8b")];
        let base = format!("{}/v1", server.uri());
        let w = enrich(&client(), &base, &mut models, Some(16 * GIB)).await;
        assert_eq!(models[0].context_window, Some(4_096));
        assert_eq!(models[0].max_context, Some(131_072));
        assert_eq!(models[0].tools, Some(true));
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("4,096"));
    }

    #[tokio::test]
    async fn enrich_uses_the_loaded_context_and_stays_quiet_when_it_is_enough() {
        let server = serve(
            json!({ "capabilities": ["tools"], "model_info": { "q.context_length": 262144 } }),
            json!({ "models": [{ "name": "q:latest", "context_length": 65536 }] }),
            "0.9.0",
        )
        .await;
        let mut models = vec![model("q")];
        let w = enrich(&client(), &server.uri(), &mut models, Some(16 * GIB)).await;
        assert_eq!(models[0].context_window, Some(65_536));
        assert!(w.is_empty(), "{w:?}");
    }

    #[tokio::test]
    async fn enrich_says_version_too_old() {
        let server = serve(json!({}), json!({ "models": [] }), "0.5.0").await;
        let mut models = vec![model("x")];
        let w = enrich(&client(), &server.uri(), &mut models, None).await;
        assert!(w.iter().any(|m| m.contains("too old")), "{w:?}");
        // No capabilities from an old server: tools stay unknown.
        assert_eq!(models[0].tools, None);
        assert_eq!(models[0].context_window, None);
    }

    #[tokio::test]
    async fn keep_warm_sends_keep_alive() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .and(body_partial_json(
                json!({ "model": "m", "keep_alive": KEEP_ALIVE }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        keep_warm(&client(), &format!("{}/v1", server.uri()), "m").await;
    }
}
