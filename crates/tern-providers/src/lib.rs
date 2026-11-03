//! Model providers: Anthropic, OpenAI-compatible endpoints, and a scripted mock.

pub mod anthropic;
mod http;
pub mod mock;
pub mod openai_compat;
mod sse;

use std::sync::Arc;

use tern_core::Secrets;
use tern_core::config::{ProviderConfig, ProviderKind};
use tern_core::types::{Provider, ProviderError};

pub use anthropic::Anthropic;
pub use mock::MockProvider;
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
