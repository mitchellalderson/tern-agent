//! Friendly provider presets: a short list of services people recognise,
//! each just a prefilled Anthropic or OpenAI-compatible provider
//! (docs/provider-setup-spec.md §12). A preset is not a new provider kind
//! and is never stored in `config.toml`; picking one fills in the name,
//! base URL and key hints, and the saved config is an ordinary
//! `[providers.<name>]` table.

use serde::Serialize;

use crate::config::ProviderKind;

/// One entry in the "Connect a provider" list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Preset {
    /// Stable id, e.g. `openrouter`.
    pub id: &'static str,
    /// What the list shows, e.g. `OpenRouter`.
    pub label: &'static str,
    /// One line under the label.
    pub blurb: &'static str,
    pub kind: ProviderKind,
    /// `None` for "Other": the user types the URL.
    pub base_url: Option<&'static str>,
    /// The provider name written to config (also the `name:` in model refs).
    pub name: &'static str,
    /// Where the key comes from; shown as a link.
    pub key_url: Option<&'static str>,
    /// Says what to do to get a key.
    pub key_hint: &'static str,
    /// A key is normally required. False for local servers.
    pub needs_key: bool,
    /// Runs on this computer (Ollama, LM Studio).
    pub local: bool,
    /// A few model ids to offer when the provider's own list can't be
    /// fetched. They are suggestions, not a promise they exist today.
    pub suggested_models: &'static [&'static str],
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "anthropic",
        label: "Anthropic",
        blurb: "Claude models",
        kind: ProviderKind::Anthropic,
        base_url: None,
        name: "anthropic",
        key_url: Some("https://console.anthropic.com/settings/keys"),
        key_hint: "Create a key in the Anthropic Console, then paste it here.",
        needs_key: true,
        local: false,
        suggested_models: &["claude-sonnet-4-5", "claude-haiku-4-5"],
    },
    Preset {
        id: "openai",
        label: "OpenAI",
        blurb: "GPT models",
        kind: ProviderKind::OpenaiCompat,
        base_url: Some("https://api.openai.com/v1"),
        name: "openai",
        key_url: Some("https://platform.openai.com/api-keys"),
        key_hint: "Create a secret key on the OpenAI platform, then paste it here.",
        needs_key: true,
        local: false,
        suggested_models: &["gpt-4o", "gpt-4o-mini"],
    },
    Preset {
        id: "openrouter",
        label: "OpenRouter",
        blurb: "One key for hundreds of models",
        kind: ProviderKind::OpenaiCompat,
        base_url: Some("https://openrouter.ai/api/v1"),
        name: "openrouter",
        key_url: Some("https://openrouter.ai/keys"),
        key_hint: "Create a key on openrouter.ai/keys, then paste it here.",
        needs_key: true,
        local: false,
        suggested_models: &[
            "anthropic/claude-sonnet-4.5",
            "openai/gpt-4o-mini",
            "google/gemini-2.5-flash",
        ],
    },
    Preset {
        id: "gemini",
        label: "Google Gemini",
        blurb: "Gemini models, through Google's OpenAI-compatible endpoint",
        kind: ProviderKind::OpenaiCompat,
        base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        name: "gemini",
        key_url: Some("https://aistudio.google.com/apikey"),
        key_hint: "Create a key in Google AI Studio, then paste it here.",
        needs_key: true,
        local: false,
        suggested_models: &["gemini-2.5-flash", "gemini-2.5-pro"],
    },
    Preset {
        id: "groq",
        label: "Groq",
        blurb: "Fast open models",
        kind: ProviderKind::OpenaiCompat,
        base_url: Some("https://api.groq.com/openai/v1"),
        name: "groq",
        key_url: Some("https://console.groq.com/keys"),
        key_hint: "Create an API key in the Groq console, then paste it here.",
        needs_key: true,
        local: false,
        suggested_models: &["llama-3.3-70b-versatile"],
    },
    Preset {
        id: "together",
        label: "Together AI",
        blurb: "Open models, hosted",
        kind: ProviderKind::OpenaiCompat,
        base_url: Some("https://api.together.xyz/v1"),
        name: "together",
        key_url: Some("https://api.together.ai/settings/api-keys"),
        key_hint: "Create an API key in your Together settings, then paste it here.",
        needs_key: true,
        local: false,
        suggested_models: &["meta-llama/Llama-3.3-70B-Instruct-Turbo"],
    },
    Preset {
        id: "ollama",
        label: "Ollama (on this computer)",
        blurb: "Local models; no key needed",
        kind: ProviderKind::OpenaiCompat,
        base_url: Some("http://localhost:11434/v1"),
        name: "ollama",
        key_url: Some("https://ollama.com/download"),
        key_hint: "Start Ollama, and pull a model first (for example `ollama pull llama3.2`). No key needed.",
        needs_key: false,
        local: true,
        suggested_models: &["llama3.2"],
    },
    Preset {
        id: "lmstudio",
        label: "LM Studio (on this computer)",
        blurb: "Local models; no key needed",
        kind: ProviderKind::OpenaiCompat,
        base_url: Some("http://localhost:1234/v1"),
        name: "lmstudio",
        key_url: Some("https://lmstudio.ai"),
        key_hint: "Open LM Studio, load a model and start its local server. No key needed.",
        needs_key: false,
        local: true,
        suggested_models: &[],
    },
    Preset {
        id: "other",
        label: "Other (OpenAI-compatible URL)",
        blurb: "Any server that speaks the OpenAI API",
        kind: ProviderKind::OpenaiCompat,
        base_url: None,
        name: "custom",
        key_url: None,
        key_hint: "Enter the server's base URL (it usually ends in /v1). Leave the key blank if it doesn't need one.",
        needs_key: false,
        local: false,
        suggested_models: &[],
    },
];

/// The preset with this id.
pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// The preset a base URL belongs to, by host. Anthropic's default URL
/// counts as the Anthropic preset (pass `None` for it).
pub fn for_url(kind: ProviderKind, base_url: Option<&str>) -> Option<&'static Preset> {
    match (kind, base_url) {
        (ProviderKind::Anthropic, None) => find("anthropic"),
        (ProviderKind::Anthropic, Some(u)) => {
            (host(u) == "api.anthropic.com").then(|| find("anthropic"))?
        }
        (ProviderKind::OpenaiCompat, Some(u)) => {
            let (h, port) = (host(u), port(u));
            PRESETS
                .iter()
                .filter(|p| p.kind == ProviderKind::OpenaiCompat)
                .find(|p| {
                    p.base_url
                        .is_some_and(|b| host(b) == h && (!p.local || self::port(b) == port))
                })
        }
        (ProviderKind::OpenaiCompat, None) => None,
    }
}

fn authority(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let a = rest.split(['/', '?', '#']).next().unwrap_or_default();
    a.rsplit('@').next().unwrap_or_default()
}

fn host(url: &str) -> String {
    authority(url)
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn port(url: &str) -> Option<&str> {
    authority(url).rsplit_once(':').map(|(_, p)| p)
}

fn path(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    match rest.find('/') {
        Some(i) => rest[i..].split(['?', '#']).next().unwrap_or_default(),
        None => "",
    }
}

/// Endpoint paths people paste instead of the base address. tern adds
/// `/chat/completions` (the Chat Completions API, not the newer Responses
/// API) or `/models` itself.
const ENDPOINT_SUFFIXES: [&str; 6] = [
    "/chat/completions",
    "/completions",
    "/responses",
    "/messages",
    "/embeddings",
    "/models",
];

/// Whether `url` ends in an endpoint path rather than a base address.
pub fn is_pasted_endpoint(url: &str) -> bool {
    let u = url.trim().trim_end_matches('/');
    ENDPOINT_SUFFIXES.iter().any(|s| u.ends_with(s))
}

/// What to tell the user when `typed` was changed to `fixed`.
pub fn repair_note(typed: &str, fixed: &str) -> String {
    if is_pasted_endpoint(typed) {
        format!(
            "That looks like a full endpoint, not the base address; using {fixed}. \
             (tern calls the chat completions endpoint itself.)"
        )
    } else {
        format!(
            "Using {fixed}: that is the address {} expects.",
            fixed_host(fixed)
        )
    }
}

fn fixed_host(url: &str) -> String {
    host(url)
}

/// Tidy a base URL so a chat call goes where the server expects it:
///
/// - drop whitespace and trailing slashes;
/// - drop an endpoint path pasted by mistake (`…/chat/completions`,
///   `…/completions`, `…/models`), because tern adds it;
/// - for a service in the preset list, replace the bare host or a guessed
///   version path (`openrouter.ai`, `openrouter.ai/v1`, `openrouter.ai/api`)
///   with the preset's real base URL.
///
/// Anything else is returned as typed.
pub fn normalize_base_url(url: &str) -> String {
    let mut u = url.trim().trim_end_matches('/').to_owned();
    loop {
        let before = u.len();
        for suffix in ENDPOINT_SUFFIXES {
            if let Some(stripped) = u.strip_suffix(suffix) {
                u = stripped.trim_end_matches('/').to_owned();
            }
        }
        if u.len() == before {
            break;
        }
    }
    if let Some(p) = for_url(ProviderKind::OpenaiCompat, Some(&u))
        && !p.local
        && let Some(real) = p.base_url
    {
        let given = path(&u).trim_end_matches('/');
        let guessed = matches!(given, "" | "/v1" | "/api" | "/api/v1" | "/v1beta");
        if guessed && given != path(real) {
            return real.to_owned();
        }
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_names_are_valid() {
        for (i, p) in PRESETS.iter().enumerate() {
            assert!(PRESETS[..i].iter().all(|q| q.id != p.id), "{}", p.id);
            assert!(
                p.name.starts_with(|c: char| c.is_ascii_lowercase())
                    && p.name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{}",
                p.name
            );
            if p.kind == ProviderKind::OpenaiCompat && p.id != "other" {
                assert!(p.base_url.is_some(), "{} needs a base URL", p.id);
            }
        }
        assert_eq!(find("openrouter").unwrap().label, "OpenRouter");
        assert!(find("nope").is_none());
    }

    #[test]
    fn finds_a_preset_by_url() {
        let id = |k, u| for_url(k, u).map(|p| p.id);
        let oc = ProviderKind::OpenaiCompat;
        assert_eq!(
            id(oc, Some("https://openrouter.ai/api/v1")),
            Some("openrouter")
        );
        assert_eq!(id(oc, Some("https://openrouter.ai/v1")), Some("openrouter"));
        assert_eq!(id(oc, Some("http://localhost:11434/v1")), Some("ollama"));
        assert_eq!(id(oc, Some("http://localhost:1234/v1")), Some("lmstudio"));
        assert_eq!(id(oc, Some("http://localhost:8000/v1")), None);
        assert_eq!(id(ProviderKind::Anthropic, None), Some("anthropic"));
        assert_eq!(id(oc, Some("https://example.com/v1")), None);
    }

    #[test]
    fn strips_a_pasted_responses_endpoint() {
        let n = normalize_base_url;
        for bad in [
            "https://openrouter.ai/api/v1/responses",
            "https://openrouter.ai/api/v1/responses/",
            "https://openrouter.ai/api/v1/embeddings",
            "https://openrouter.ai/api/v1/messages",
        ] {
            assert_eq!(n(bad), "https://openrouter.ai/api/v1", "{bad}");
            assert!(is_pasted_endpoint(bad));
        }
        assert_eq!(
            n("https://example.com/v1/responses"),
            "https://example.com/v1"
        );
        assert!(!is_pasted_endpoint("https://openrouter.ai/api/v1"));
        let note = repair_note(
            "https://openrouter.ai/api/v1/responses",
            "https://openrouter.ai/api/v1",
        );
        assert!(note.starts_with("That looks like a full endpoint, not the base address; using https://openrouter.ai/api/v1."));
    }

    #[test]
    fn normalizes_urls() {
        let n = normalize_base_url;
        assert_eq!(
            n("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            n("https://openrouter.ai/api/v1/"),
            "https://openrouter.ai/api/v1"
        );
        for bad in [
            "https://openrouter.ai",
            "https://openrouter.ai/",
            "https://openrouter.ai/v1",
            "https://openrouter.ai/api",
            "https://openrouter.ai/v1/chat/completions",
            "https://openrouter.ai/api/v1/chat/completions",
            "  https://openrouter.ai/api/v1/models  ",
        ] {
            assert_eq!(n(bad), "https://openrouter.ai/api/v1", "{bad}");
        }
        assert_eq!(
            n("https://api.groq.com/v1"),
            "https://api.groq.com/openai/v1"
        );
        assert_eq!(n("https://api.openai.com"), "https://api.openai.com/v1");
        assert_eq!(
            n("https://generativelanguage.googleapis.com"),
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
        // Not in the list, or deliberately different: left alone, apart
        // from a pasted endpoint path.
        assert_eq!(n("https://example.com"), "https://example.com");
        assert_eq!(
            n("https://example.com/v1/chat/completions"),
            "https://example.com/v1"
        );
        assert_eq!(n("http://localhost:11434/v1"), "http://localhost:11434/v1");
        assert_eq!(
            n("https://api.openai.com/v1/proxy"),
            "https://api.openai.com/v1/proxy"
        );
    }
}
