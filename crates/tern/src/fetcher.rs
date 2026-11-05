//! Where the quarantined fetcher's model comes from (docs/quarantined-fetch-design.md
//! §2.5): the configured `[fetch] model`, or the cheapest model the user
//! already has configured. Only the user's own providers; never a service
//! of ours.

use std::sync::{Arc, RwLock, Weak};

use tern_core::agent::Agent;
use tern_core::config::{Config, ModelChain, ModelRef};
use tern_core::fetch::{FetcherModel, FetcherModels};

use crate::providers::{ModelCache, price_of};

pub struct RuntimeFetcherModels {
    pub live: Arc<RwLock<Config>>,
    pub agent: Weak<Agent>,
    pub models: Arc<ModelCache>,
    /// `TERN_PROVIDER=mock:...`: every provider name is usable.
    pub mock: bool,
}

/// The model to read with: `configured` if set, else the cheapest among the
/// models in every model setting that has a provider and a known price, else
/// the default model.
pub fn pick_model(
    config: &Config,
    models: &ModelCache,
    mock: bool,
    configured: Option<&ModelChain>,
) -> Result<ModelRef, String> {
    if let Some(c) = configured {
        return c
            .primary()
            .parse()
            .map_err(|e| format!("[fetch] model: {e}"));
    }
    let usable = |m: &&String| {
        m.parse::<ModelRef>()
            .is_ok_and(|r| mock || config.providers.contains_key(&r.provider))
    };
    let candidates: Vec<String> = config
        .model_chains()
        .into_iter()
        .flat_map(|(_, c)| c.entries().to_vec())
        .collect();
    let cheapest = candidates
        .iter()
        .filter(usable)
        .filter_map(|m| price_of(config, models, m).map(|p| (p.input + p.output, m)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, m)| m.clone());
    let chosen = cheapest
        .or_else(|| candidates.iter().find(usable).cloned())
        .ok_or_else(|| "no model is configured".to_owned())?;
    chosen.parse().map_err(|e| format!("{e}"))
}

impl FetcherModels for RuntimeFetcherModels {
    fn pick(&self, configured: Option<&ModelChain>) -> Result<FetcherModel, String> {
        let config = self.live.read().unwrap_or_else(|p| p.into_inner()).clone();
        let model = pick_model(&config, &self.models, self.mock, configured)?;
        let agent = self.agent.upgrade().ok_or("tern is shutting down")?;
        let provider = agent.provider(&model).map_err(|e| e.to_string())?;
        let label = model.to_string();
        let price = price_of(&config, &self.models, &label).map(|p| (p.input, p.output));
        Ok(FetcherModel {
            provider,
            model: model.model,
            label,
            price,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_models_is_an_error_and_a_configured_one_wins() {
        let cfg = Config::default();
        let cache = ModelCache::load(
            std::env::temp_dir().join("tern-fetcher-test-models.json"),
            tokio::sync::broadcast::channel(4).0,
        );
        assert!(pick_model(&cfg, &cache, false, None).is_err());
        let chain = ModelChain::new(vec!["a:b".into()]).unwrap();
        assert_eq!(
            pick_model(&cfg, &cache, false, Some(&chain)).unwrap().model,
            "b"
        );
    }
}
