//! How big a model's context window and per-turn output are.
//!
//! Resolution, most specific first:
//!
//! 1. what the user set (`[providers.<name>].context_window`, then the
//!    top-level `context_window`);
//! 2. what the provider's own model list reported (Anthropic's
//!    `max_input_tokens`, OpenRouter's `context_length`);
//! 3. the catalogue below, from the providers' documented limits;
//! 4. a fallback: [`FALLBACK_CONTEXT_WINDOW`] for hosted providers, and the
//!    smaller [`LOCAL_CONTEXT_WINDOW`] for models on this computer, whose
//!    real window depends on how the server was started and is often far
//!    below what the model supports.
//!
//! Output tokens work the same way, with [`DEFAULT_MAX_OUTPUT`] as the
//! default: capped at the model's documented maximum, and at a fifth of the
//! window so that a small window still has room for the conversation.

/// Context window when nothing else knows one (hosted providers).
pub const FALLBACK_CONTEXT_WINDOW: u64 = 200_000;
/// Context window for a local model whose server doesn't report one.
pub const LOCAL_CONTEXT_WINDOW: u64 = 32_768;
/// Output tokens asked for per model call, unless the model allows fewer.
pub const DEFAULT_MAX_OUTPUT: u32 = 32_000;
/// Output never takes more than `1 / OUTPUT_SHARE` of the window.
pub const OUTPUT_SHARE: u64 = 5;

/// What a model is documented to accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelLimits {
    /// Context window in tokens.
    pub context: u64,
    /// Largest output per call.
    pub max_output: u32,
}

const fn lim(context: u64, max_output: u32) -> ModelLimits {
    ModelLimits {
        context,
        max_output,
    }
}

/// Documented limits for a model id, with any `vendor/` prefix (OpenRouter)
/// and dots in version numbers ignored. Newest families first; the first
/// prefix that matches wins.
pub fn known(model: &str) -> Option<ModelLimits> {
    let id = model.rsplit('/').next().unwrap_or(model).replace('.', "-");
    let id = id.to_ascii_lowercase();
    const TABLE: &[(&str, ModelLimits)] = &[
        // Anthropic. Claude 5 and 4.6+ have a 1M window and 128K output.
        ("claude-fable-5", lim(1_000_000, 128_000)),
        ("claude-mythos-5", lim(1_000_000, 128_000)),
        ("claude-opus-5", lim(1_000_000, 128_000)),
        ("claude-sonnet-5", lim(1_000_000, 128_000)),
        ("claude-haiku-5", lim(1_000_000, 128_000)),
        ("claude-opus-4-8", lim(1_000_000, 128_000)),
        ("claude-opus-4-7", lim(1_000_000, 128_000)),
        ("claude-opus-4-6", lim(1_000_000, 128_000)),
        ("claude-sonnet-4-6", lim(1_000_000, 128_000)),
        ("claude-opus-4-5", lim(200_000, 64_000)),
        ("claude-sonnet-4-5", lim(200_000, 64_000)),
        ("claude-haiku-4-5", lim(200_000, 64_000)),
        ("claude-sonnet-4", lim(200_000, 64_000)),
        ("claude-opus-4-1", lim(200_000, 32_000)),
        ("claude-opus-4", lim(200_000, 32_000)),
        ("claude-3-7-sonnet", lim(200_000, 64_000)),
        ("claude-3-5-haiku", lim(200_000, 8_192)),
        // OpenAI.
        ("gpt-5", lim(400_000, 128_000)),
        ("gpt-4-1", lim(1_047_576, 32_768)),
        ("gpt-4o", lim(128_000, 16_384)),
        ("o4-mini", lim(200_000, 100_000)),
        ("o3", lim(200_000, 100_000)),
        // Google.
        ("gemini-3", lim(1_048_576, 65_536)),
        ("gemini-2-5", lim(1_048_576, 65_536)),
        ("gemini-2-0", lim(1_048_576, 8_192)),
    ];
    TABLE
        .iter()
        .find(|(prefix, _)| id.starts_with(prefix))
        .map(|(_, l)| *l)
}

/// Whether `base_url` points at this computer or its network (Ollama,
/// LM Studio and similar): loopback, `.local` names and private ranges.
pub fn is_local_url(base_url: &str) -> bool {
    let rest = base_url.split_once("://").map_or(base_url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or("");
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    }
    .to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    if host == "::1" {
        return true;
    }
    let octets: Vec<u8> = host.split('.').filter_map(|p| p.parse().ok()).collect();
    match octets[..] {
        [a, b, _, _] if host.split('.').count() == 4 => {
            a == 127 || a == 10 || (a == 192 && b == 168) || (a == 172 && (16..32).contains(&b))
        }
        _ => false,
    }
}

/// What the agent knows about one provider when it sizes a run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Inputs {
    /// The user's `context_window` for this provider, else the top level.
    pub user_window: Option<u64>,
    /// The user's `max_tokens` for this provider, else the top level.
    pub user_max_tokens: Option<u32>,
    /// The window the provider's model list reported for this model.
    pub reported_window: Option<u64>,
    /// The provider runs on this computer or its network.
    pub local: bool,
}

/// The window and the per-call output budget for `model`.
pub fn resolve(model: &str, i: Inputs) -> (u64, u32) {
    let known = known(model);
    let window = i
        .user_window
        .or(i.reported_window)
        .or(known.map(|k| k.context))
        .unwrap_or(if i.local {
            LOCAL_CONTEXT_WINDOW
        } else {
            FALLBACK_CONTEXT_WINDOW
        });
    let max_tokens = i.user_max_tokens.unwrap_or_else(|| {
        let model_max = known.map_or(DEFAULT_MAX_OUTPUT, |k| k.max_output);
        let share = u32::try_from(window / OUTPUT_SHARE).unwrap_or(u32::MAX);
        DEFAULT_MAX_OUTPUT.min(model_max).min(share).max(1)
    });
    (window, max_tokens)
}

/// `requested`, cut to what `model` is documented to allow. Applied where a
/// request is sent, so a fallback model in a chain is never asked for more
/// than it takes. Unknown models pass through.
pub fn clamp_output(model: &str, requested: u32) -> u32 {
    known(model).map_or(requested, |k| requested.min(k.max_output))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(model: &str, i: Inputs) -> (u64, u32) {
        resolve(model, i)
    }

    #[test]
    fn user_window_beats_reported_beats_catalogue_beats_fallback() {
        let both = Inputs {
            user_window: Some(50_000),
            reported_window: Some(90_000),
            ..Inputs::default()
        };
        assert_eq!(r("claude-opus-5-5", both).0, 50_000);
        let reported = Inputs {
            reported_window: Some(90_000),
            ..Inputs::default()
        };
        assert_eq!(r("claude-opus-5-5", reported).0, 90_000);
        assert_eq!(r("claude-opus-5-5", Inputs::default()).0, 1_000_000);
        assert_eq!(r("some-new-model", Inputs::default()).0, 200_000);
    }

    #[test]
    fn catalogue_ignores_vendor_prefix_and_dots() {
        assert_eq!(
            known("anthropic/claude-sonnet-4.6"),
            known("claude-sonnet-4-6")
        );
        assert_eq!(known("openai/gpt-5.1").unwrap().context, 400_000);
        assert_eq!(known("google/gemini-2.5-flash").unwrap().max_output, 65_536);
        assert_eq!(known("claude-opus-4-1").unwrap().max_output, 32_000);
        assert_eq!(known("llama3.1:8b"), None);
    }

    #[test]
    fn output_defaults_to_32k_and_clamps_to_the_model() {
        assert_eq!(r("some-new-model", Inputs::default()).1, 32_000);
        // Documented max 8192 is below the default.
        assert_eq!(r("claude-3-5-haiku-latest", Inputs::default()).1, 8_192);
        // A 128K-output model still gets the 32K default.
        assert_eq!(r("claude-opus-5-5", Inputs::default()).1, 32_000);
    }

    #[test]
    fn output_leaves_room_in_a_small_window() {
        let small = Inputs {
            user_window: Some(8_000),
            ..Inputs::default()
        };
        assert_eq!(r("claude-opus-5-5", small).1, 1_600);
    }

    #[test]
    fn user_max_tokens_always_wins() {
        let i = Inputs {
            user_max_tokens: Some(100_000),
            ..Inputs::default()
        };
        assert_eq!(r("claude-3-5-haiku-latest", i).1, 100_000);
    }

    #[test]
    fn local_models_with_unknown_windows_stay_conservative() {
        let local = Inputs {
            local: true,
            ..Inputs::default()
        };
        assert_eq!(r("llama3.1:8b", local), (32_768, 6_553));
        // The endpoint's own number, or the user's, still wins.
        let reported = Inputs {
            reported_window: Some(131_072),
            ..local
        };
        assert_eq!(r("llama3.1:8b", reported).0, 131_072);
        let user = Inputs {
            user_window: Some(8_192),
            ..local
        };
        assert_eq!(r("llama3.1:8b", user).0, 8_192);
    }

    #[test]
    fn clamp_output_caps_known_models_only() {
        assert_eq!(clamp_output("claude-3-5-haiku-latest", 32_000), 8_192);
        assert_eq!(clamp_output("claude-opus-5-5", 32_000), 32_000);
        assert_eq!(clamp_output("mystery", 99_999), 99_999);
    }

    #[test]
    fn local_urls() {
        for u in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:1234/v1",
            "http://[::1]:8080",
            "http://192.168.1.20:8000/v1",
            "http://10.0.0.5/v1",
            "http://172.20.0.2/v1",
            "http://box.local:11434",
        ] {
            assert!(is_local_url(u), "{u}");
        }
        for u in [
            "https://api.openai.com/v1",
            "https://openrouter.ai/api/v1",
            "http://172.32.0.1",
            "https://localhost.evil.com",
        ] {
            assert!(!is_local_url(u), "{u}");
        }
    }
}
