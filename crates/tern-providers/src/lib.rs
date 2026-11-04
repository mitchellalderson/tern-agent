//! Model providers: Anthropic, OpenAI-compatible endpoints, and a scripted mock.

pub mod anthropic;
pub mod chain;
mod http;
pub mod mock;
pub mod models;
pub mod openai_compat;
mod sse;

use std::sync::Arc;

use tern_core::Secrets;
use tern_core::config::{Config, ModelRef, ProviderConfig, ProviderKind};
use tern_core::types::{Provider, ProviderError};

pub use anthropic::Anthropic;
pub use chain::{ChainProvider, Member};
pub use mock::MockProvider;
pub use models::{ListError, Listing, ModelInfo, list_models};
pub use openai_compat::OpenAiCompat;

/// Env var selecting the mock provider: `TERN_PROVIDER=mock:<script.json>`.
pub const PROVIDER_ENV: &str = "TERN_PROVIDER";

/// Construct the provider named `name` from its config.
pub fn build(
    name: &str,
    cfg: &ProviderConfig,
    secrets: &Secrets,
) -> Result<Arc<dyn Provider>, ProviderError> {
    let key = match &cfg.api_key_secret {
        Some(secret) => Some(secrets.get(secret).ok_or_else(|| {
            ProviderError::Config(format!(
                "provider {name}: secret {secret} is not set (environment or secrets.toml)"
            ))
        })?),
        None => None,
    };
    let base = cfg.effective_base_url().ok_or_else(|| {
        ProviderError::Config(format!(
            "provider {name}: base_url is required for openai_compat"
        ))
    })?;
    Ok(match cfg.kind {
        ProviderKind::Anthropic => {
            let key = key.ok_or_else(|| {
                ProviderError::Config(format!("provider {name}: anthropic needs api_key_secret"))
            })?;
            Arc::new(Anthropic::new(name, base, key)?)
        }
        ProviderKind::OpenaiCompat => Arc::new(OpenAiCompat::new(name, base, key)?),
    })
}

/// The provider for `model`: a [`ChainProvider`] when config lists
/// fallbacks for it (T6a), else the model's own provider. A fallback that
/// can't be built (its provider is gone, its key unset) is left out with a
/// warning; `tern doctor` says why.
pub fn for_model(
    model: &ModelRef,
    config: &Config,
    secrets: &Secrets,
) -> Result<Arc<dyn Provider>, ProviderError> {
    with_fallbacks(model, config, |m| {
        let cfg = config.providers.get(&m.provider).ok_or_else(|| {
            ProviderError::Config(format!(
                "provider {:?} is not configured in config.toml",
                m.provider
            ))
        })?;
        build(&m.provider, cfg, secrets)
    })
}

/// `model`'s chain from `config`, each member's provider from `one`.
pub fn with_fallbacks(
    model: &ModelRef,
    config: &Config,
    one: impl Fn(&ModelRef) -> Result<Arc<dyn Provider>, ProviderError>,
) -> Result<Arc<dyn Provider>, ProviderError> {
    let primary = one(model)?;
    let fallbacks = config.fallbacks_for(model);
    if fallbacks.is_empty() {
        return Ok(primary);
    }
    let mut members = vec![Member {
        model: model.clone(),
        provider: primary,
    }];
    for m in fallbacks {
        match one(&m) {
            Ok(provider) => members.push(Member { model: m, provider }),
            Err(e) => tracing::warn!(fallback = %m, error = %e, "skipping a fallback model"),
        }
    }
    Ok(match members.len() {
        1 => members.remove(0).provider,
        _ => Arc::new(ChainProvider::new(members)),
    })
}
