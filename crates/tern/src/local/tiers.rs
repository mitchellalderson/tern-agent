//! Choosing a local model per tier (agent, fast, decision) and the
//! recommended set for this Mac (docs/local-models.md).

use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use tern_core::models::{Change, ProviderDraft};

use super::Error;
use super::catalog::{self, Tier};
use crate::runtime::Runtime;

/// One tier: what is recommended, and what is set.
#[derive(Debug, Clone, Serialize)]
pub struct TierRow {
    pub tier: Tier,
    pub model: &'static str,
    pub size_gb: f32,
    pub blurb: &'static str,
    pub note: Option<&'static str>,
    /// The recommended model is downloaded.
    pub installed: bool,
    /// The model this tier uses now, when it is a local one.
    pub current: Option<String>,
}

/// `GET /v1/local/catalog`.
#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub ram_bytes: Option<u64>,
    pub tiers: Vec<TierRow>,
    /// "Set up all three" is running.
    pub setting_up: bool,
    /// Why the last "set up all three" stopped.
    pub error: Option<String>,
}

/// The configured Ollama provider's name.
fn ollama_provider(rt: &Runtime) -> Option<String> {
    rt.live()
        .providers
        .iter()
        .find(|(name, p)| {
            // By address, or the provider the ollama preset names.
            *name == "ollama"
                || p.effective_base_url()
                    .is_some_and(tern_providers::ollama::is_ollama_url)
        })
        .map(|(n, _)| n.clone())
}

/// What `tier` uses now as a local model name (without the provider).
fn current(rt: &Runtime, tier: Tier) -> Option<String> {
    let provider = ollama_provider(rt)?;
    let cfg = rt.live();
    let chain = match tier {
        Tier::Agent => cfg.default_model.as_ref(),
        Tier::Fast => cfg.fast_model.as_ref(),
        Tier::Decision => cfg.decision_model.as_ref(),
    }?;
    chain
        .primary()
        .strip_prefix(&format!("{provider}:"))
        .map(str::to_owned)
}

pub async fn catalog(rt: &Runtime) -> Catalog {
    let ram = tern_core::limits::total_memory_bytes();
    let installed: Vec<String> = rt
        .local
        .models()
        .await
        .map(|m| m.into_iter().map(|m| m.name).collect())
        .unwrap_or_default();
    let has = |name: &str| {
        installed
            .iter()
            .any(|n| n == name || n.strip_suffix(":latest") == Some(name))
    };
    let (setting_up, error) = rt.local.preset_state();
    Catalog {
        ram_bytes: ram,
        tiers: catalog::picks(ram)
            .into_iter()
            .map(|p| TierRow {
                tier: p.tier,
                model: p.model,
                size_gb: p.size_gb,
                blurb: p.blurb,
                note: p.note,
                installed: has(p.model),
                current: current(rt, p.tier),
            })
            .collect(),
        setting_up,
        error,
    }
}

/// Use the installed `model` for `tier`. Refuses a model that can't use
/// tools where the tier needs them.
pub async fn use_for(rt: &Runtime, tier: Tier, model: &str) -> Result<(), Error> {
    let models = rt.local.models().await?;
    let m = models
        .iter()
        .find(|m| m.name == model)
        .ok_or_else(|| Error::Failed(format!("{model} isn't installed. Download it first.")))?;
    if tier.needs_tools() && m.tools == Some(false) {
        return Err(Error::Failed(format!(
            "{model} can't use tools, so it can't be used for the {} tier.",
            tier.as_str()
        )));
    }
    let provider = match ollama_provider(rt) {
        Some(p) => p,
        None => {
            let draft = ProviderDraft {
                preset: Some("ollama".into()),
                ..ProviderDraft::default()
            };
            let (status, _) = rt
                .save_provider(&draft, None, None)
                .await
                .map_err(|e| Error::Failed(e.to_string()))?;
            status.name
        }
    };
    let reference = format!("{provider}:{model}");
    match tier {
        Tier::Agent => {
            rt.set_default_model(&reference)
                .map_err(|e| Error::Failed(e.to_string()))?;
        }
        Tier::Fast | Tier::Decision => {
            let key = match tier {
                Tier::Fast => "fast_model",
                _ => "decision_model",
            };
            rt.change_setting(key, Some(&json!(reference)))
                .map_err(|e| Error::Failed(e.to_string()))?;
        }
    }
    let _ = rt.changes.send(Change::LocalChanged {});
    Ok(())
}

/// Download what is missing from the recommended set, one model at a time,
/// then use each for its tier. Stops at the first failure and says why.
pub async fn set_up_all(rt: std::sync::Arc<Runtime>) {
    rt.local.set_preset(true, None);
    let result = run_all(&rt).await;
    rt.local
        .set_preset(false, result.err().map(|e| e.to_string()));
    let _ = rt.changes.send(Change::LocalChanged {});
}

async fn run_all(rt: &Runtime) -> Result<(), Error> {
    rt.local.ensure_running().await?;
    let ram = tern_core::limits::total_memory_bytes();
    let picks = catalog::picks(ram);
    // The same model can serve two tiers: download each once.
    let mut wanted: Vec<&str> = Vec::new();
    for p in &picks {
        if !wanted.contains(&p.model) {
            wanted.push(p.model);
        }
    }
    for model in wanted {
        let have = rt
            .local
            .models()
            .await?
            .iter()
            .any(|m| m.name == model || m.name.strip_suffix(":latest") == Some(model));
        if have {
            continue;
        }
        let mut changes = rt.subscribe_changes();
        let job = rt.local.pull(model).await?;
        loop {
            // A missed event is covered by looking at the list.
            let _ = tokio::time::timeout(Duration::from_secs(2), changes.recv()).await;
            match rt.local.pulls().into_iter().find(|p| p.id == job.id) {
                Some(p) if p.phase == super::pulls::Phase::Done => break,
                Some(p) if p.phase.finished() => {
                    return Err(Error::Failed(
                        p.error
                            .unwrap_or_else(|| format!("downloading {model} didn't finish")),
                    ));
                }
                Some(_) => {}
                None => return Err(Error::Failed("the download disappeared".into())),
            }
        }
    }
    for p in &picks {
        use_for(rt, p.tier, p.model).await?;
    }
    Ok(())
}
