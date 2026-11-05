//! Provider setup and model lists (docs/provider-setup-spec.md): verifying
//! a key, adding, changing and removing providers without a restart,
//! cached model lists, and resolving model references.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tern_core::Config;
use tern_core::config::edit::ConfigFile;
use tern_core::config::{ModelChain, ModelRef, Pricing, ProviderConfig, ProviderKind};
use tern_core::models::{
    Change, ListError, ModelInfo, ProviderDraft, ProviderModels, ProviderStatus, Resolved,
    Verified, describe, host_of,
};
use tern_core::secrets::keys::{mask_key, normalize_key, secret_name_for};
use tern_providers::{Listing, list_models};
use tokio::sync::broadcast;

use crate::runtime::Runtime;

/// Model lists older than this are refreshed in the background.
const FRESH_SECS: i64 = 600;
/// Env var overriding [`FRESH_SECS`], for tests.
pub const FRESH_ENV: &str = "TERN_MODELS_FRESH_SECS";

fn fresh_secs() -> i64 {
    static SECS: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *SECS.get_or_init(|| {
        std::env::var(FRESH_ENV)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(FRESH_SECS)
    })
}
const ANTHROPIC_PREFIX: &str = "sk-ant-";

// --- Model cache ----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// Changes when the provider's kind, URL or key changes.
    fingerprint: String,
    fetched_at: i64,
    models: Vec<ModelInfo>,
    #[serde(skip)]
    error: Option<ListError>,
}

/// Model lists per provider, in memory and in `cache/models.json`, so
/// pickers open at once and work offline with the last known list.
pub struct ModelCache {
    path: PathBuf,
    entries: Mutex<BTreeMap<String, Entry>>,
    changes: broadcast::Sender<Change>,
}

impl ModelCache {
    pub fn load(path: PathBuf, changes: broadcast::Sender<Change>) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self {
            path,
            entries: Mutex::new(entries),
            changes,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn get(&self, provider: &str, fingerprint: &str) -> Option<Entry> {
        self.lock()
            .get(provider)
            .filter(|e| e.fingerprint == fingerprint)
            .cloned()
    }

    /// Ids the cache lists for `provider`, whatever its fingerprint.
    fn ids(&self, provider: &str) -> Option<Vec<String>> {
        let entries = self.lock();
        let e = entries.get(provider).filter(|e| !e.models.is_empty())?;
        Some(e.models.iter().map(|m| m.id.clone()).collect())
    }

    /// The price the cached list gives for `provider:model`, if any.
    pub fn pricing(&self, provider: &str, model: &str) -> Option<Pricing> {
        let entries = self.lock();
        entries
            .get(provider)?
            .models
            .iter()
            .find(|m| m.id == model)?
            .pricing
    }

    fn put(&self, provider: &str, fingerprint: &str, result: &Result<Listing, ListError>) {
        let now = chrono::Utc::now().timestamp();
        let mut entries = self.lock();
        let entry = match result {
            Ok(l) => Entry {
                fingerprint: fingerprint.to_owned(),
                fetched_at: now,
                models: l.models.clone(),
                error: None,
            },
            Err(e) => {
                // Keep the last good list next to the error.
                let models = entries
                    .get(provider)
                    .filter(|old| old.fingerprint == fingerprint)
                    .map(|old| old.models.clone())
                    .unwrap_or_default();
                Entry {
                    fingerprint: fingerprint.to_owned(),
                    fetched_at: now,
                    models,
                    error: Some(e.clone()),
                }
            }
        };
        entries.insert(provider.to_owned(), entry);
        let body = serde_json::to_string(&*entries).unwrap_or_default();
        drop(entries);
        if let Err(e) = write_atomic(&self.path, &body) {
            tracing::warn!(error = %e, path = %self.path.display(), "writing model cache");
        }
        let _ = self.changes.send(Change::ModelsChanged {
            provider: provider.to_owned(),
        });
    }

    fn forget(&self, provider: &str) {
        self.lock().remove(provider);
    }
}

fn write_atomic(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// Identifies what a cached list was fetched with, without storing the key.
fn fingerprint(cfg: &ProviderConfig, key: Option<&str>) -> String {
    let mut h = blake3::Hasher::new();
    h.update(format!("{:?}|{:?}|", cfg.kind, cfg.effective_base_url()).as_bytes());
    if let Some(k) = key {
        h.update(k.as_bytes());
    }
    h.finalize().to_hex()[..16].to_owned()
}

// --- Plain-words errors ---------------------------------------------------

/// A provider name from a base URL: `api.openai.com` → `openai`,
/// `localhost:11434` → `ollama`, `localhost:1234` → `lmstudio`.
pub fn suggest_name(base_url: &str) -> String {
    let rest = base_url.split_once("://").map_or(base_url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    match authority.rsplit_once(':').map(|(_, p)| p) {
        Some("11434") => return "ollama".into(),
        Some("1234") => return "lmstudio".into(),
        _ => {}
    }
    let host = host_of(base_url);
    if host.parse::<std::net::IpAddr>().is_ok() || host == "localhost" {
        return "local".into();
    }
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    let pick = match labels.as_slice() {
        [.., name, _tld] => *name,
        [only] => *only,
        [] => "custom",
    };
    let name = clean_name(pick);
    if name.is_empty() {
        "custom".into()
    } else {
        name
    }
}

fn clean_name(s: &str) -> String {
    s.chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(32)
        .collect()
}

/// Provider names go before the `:` in model references and into secret
/// names, so keep them simple.
pub fn check_provider_name(name: &str) -> anyhow::Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
    if !ok {
        bail!(
            "invalid provider name {name:?}: use lowercase letters, digits, - and _, starting with a letter"
        );
    }
    Ok(())
}

// --- Runtime operations ---------------------------------------------------

/// A draft resolved against the live config: what would be saved.
struct Plan {
    name: String,
    cfg: ProviderConfig,
    /// A pasted key to store under `cfg.api_key_secret`.
    new_key: Option<String>,
    /// The key to verify with: pasted, from the environment, or stored.
    key: Option<String>,
    warnings: Vec<String>,
}

/// `provider:model`'s price: `[providers.<name>.prices]` first, then the
/// provider's model list. `None` when neither knows it.
pub fn price_of(config: &Config, cache: &ModelCache, model: &str) -> Option<Pricing> {
    let m: ModelRef = model.parse().ok()?;
    if let Some(p) = config
        .providers
        .get(&m.provider)
        .and_then(|p| p.prices.get(&m.model))
    {
        return Some(*p);
    }
    cache.pricing(&m.provider, &m.model)
}

impl Runtime {
    /// `model`'s price (`provider:model`), if known.
    pub fn price_of(&self, model: &str) -> Option<Pricing> {
        price_of(&self.live(), &self.models, model)
    }

    /// Whether there is no usable model: no default, its provider isn't
    /// configured, or that provider's key is missing.
    pub fn setup_needed(&self) -> bool {
        if self.mock {
            return false;
        }
        let config = self.live();
        let Some(Ok(m)) = config.default_model_ref() else {
            return true;
        };
        match config.providers.get(&m.provider) {
            None => true,
            Some(p) => p
                .api_key_secret
                .as_ref()
                .is_some_and(|s| self.secrets.get(s).is_none()),
        }
    }

    /// Resolve `text` to a configured `provider:model`. The provider may be
    /// left off when exactly one provider's cached list has that id. A
    /// model missing from its provider's list is allowed, with a warning.
    pub fn resolve_model(&self, text: &str) -> anyhow::Result<Resolved> {
        let text = text.trim();
        let config = self.live();
        if self.mock {
            let m: ModelRef = text.parse()?;
            return Ok(Resolved {
                model: m.to_string(),
                warning: None,
            });
        }
        let explicit = text
            .split_once(':')
            .filter(|(p, m)| !m.is_empty() && config.providers.contains_key(*p));
        if let Some((provider, model)) = explicit {
            let warning = self
                .models
                .ids(provider)
                .filter(|ids| !ids.iter().any(|id| id == model))
                .map(|_| format!("{model} is not in {provider}'s model list; using it anyway"));
            return Ok(Resolved {
                model: format!("{provider}:{model}"),
                warning,
            });
        }
        let offering: Vec<&String> = config
            .providers
            .keys()
            .filter(|p| {
                self.models
                    .ids(p)
                    .is_some_and(|ids| ids.iter().any(|id| id == text))
            })
            .collect();
        match offering.as_slice() {
            [one] => Ok(Resolved {
                model: format!("{one}:{text}"),
                warning: None,
            }),
            [] => match text.split_once(':') {
                Some((p, m)) if !p.is_empty() && !m.is_empty() => {
                    bail!("provider {p:?} is not configured; run /setup to add it")
                }
                _ if config.providers.is_empty() => {
                    bail!("no providers are configured; run /setup to add one")
                }
                _ => bail!(
                    "no configured provider lists {text:?}; write it as provider:model (providers: {})",
                    config
                        .providers
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
            many => {
                let names: Vec<String> = many.iter().map(|p| format!("{p}:{text}")).collect();
                let providers: Vec<&str> = many.iter().map(|p| p.as_str()).collect();
                bail!(
                    "{text:?} is offered by {}; use {}",
                    providers.join(" and "),
                    names.join(" or ")
                )
            }
        }
    }

    /// Re-read `config.toml` and swap in its providers and default model.
    /// Runs already going keep the provider they started with.
    pub fn reload_providers(&self) -> anyhow::Result<()> {
        let (config, _) = Config::load(&self.home.config_path())?;
        self.set_live(config);
        let _ = self.changes.send(Change::ProvidersChanged {});
        Ok(())
    }

    pub fn provider_statuses(&self) -> Vec<ProviderStatus> {
        let config = self.live();
        let default = config
            .default_model_ref()
            .and_then(Result::ok)
            .map(|m| m.provider);
        config
            .providers
            .iter()
            .map(|(name, p)| {
                let key = p.api_key_secret.as_ref().and_then(|s| self.secrets.get(s));
                let key_source = match &p.api_key_secret {
                    None => "none",
                    Some(s) => match self.secrets.source(s) {
                        Some(tern_core::secrets::SecretSource::Env) => "env",
                        Some(tern_core::secrets::SecretSource::File) => "file",
                        None => "missing",
                    },
                };
                let error = self
                    .models
                    .get(name, &fingerprint(p, key.as_deref()))
                    .and_then(|e| e.error);
                ProviderStatus {
                    name: name.clone(),
                    kind: p.kind,
                    base_url: p.effective_base_url().map(str::to_owned),
                    secret: p.api_key_secret.clone(),
                    key_source: key_source.into(),
                    key_hint: key.as_deref().map(mask_key),
                    preset: preset_of(p).map(|x| x.id.to_owned()),
                    label: label_of(name, p),
                    message: error
                        .as_ref()
                        .map(|e| describe(e, name, p.effective_base_url())),
                    error,
                    is_default: default.as_deref() == Some(name),
                }
            })
            .collect()
    }

    /// Model lists for every configured provider, or just `provider`.
    /// Fresh cached lists are returned as is; stale ones are returned and
    /// refreshed in the background (clients get `models_changed`).
    pub async fn models(
        &self,
        provider: Option<&str>,
        refresh: bool,
    ) -> anyhow::Result<Vec<ProviderModels>> {
        let config = self.live();
        if let Some(p) = provider
            && !config.providers.contains_key(p)
        {
            bail!("provider {p:?} is not configured");
        }
        let now = chrono::Utc::now().timestamp();
        let mut out = Vec::new();
        for (name, cfg) in &config.providers {
            if provider.is_some_and(|p| p != name) {
                continue;
            }
            let key = cfg
                .api_key_secret
                .as_ref()
                .and_then(|s| self.secrets.get(s));
            let fp = fingerprint(cfg, key.as_deref());
            let cached = self.models.get(name, &fp);
            let entry = match cached {
                Some(e) if !refresh && now - e.fetched_at < fresh_secs() => e,
                Some(e) if !refresh => {
                    let (cache, name, cfg) = (self.models.clone(), name.clone(), cfg.clone());
                    tokio::spawn(async move {
                        let result = list_models(&cfg, key.as_deref()).await;
                        cache.put(&name, &fp, &result);
                    });
                    e
                }
                _ => {
                    let result = list_models(cfg, key.as_deref()).await;
                    self.models.put(name, &fp, &result);
                    self.models.get(name, &fp).context("model cache entry")?
                }
            };
            out.push(ProviderModels {
                provider: name.clone(),
                message: entry
                    .error
                    .as_ref()
                    .map(|e| describe(e, name, cfg.effective_base_url())),
                error: entry.error,
                fetched_at: Some(entry.fetched_at),
                models: entry.models,
                suggested: preset_of(cfg)
                    .map(|p| {
                        p.suggested_models
                            .iter()
                            .map(|m| (*m).to_owned())
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Work out what `draft` would save. `existing` names a provider being
    /// changed rather than added.
    fn plan(&self, draft: &ProviderDraft, existing: Option<&str>) -> anyhow::Result<Plan> {
        let config = self.live();
        let mut warnings = Vec::new();
        // A preset fills in what the draft leaves out.
        let preset = match draft.preset.as_deref() {
            Some(id) => Some(
                tern_core::presets::find(id)
                    .with_context(|| format!("unknown provider preset {id:?}"))?,
            ),
            None => None,
        };
        let mut draft = draft.clone();
        if let Some(p) = preset
            && existing.is_none()
        {
            draft.kind.get_or_insert(p.kind);
            if draft.name.as_deref().is_none_or(|n| n.trim().is_empty()) {
                draft.name = Some(unused_name(&config, p.name));
            }
            if draft.base_url.as_deref().is_none_or(|u| u.trim().is_empty()) {
                draft.base_url = p.base_url.map(str::to_owned);
            }
        }
        let draft = &draft;
        let (name, mut cfg) = match existing {
            Some(name) => {
                let cfg = config
                    .providers
                    .get(name)
                    .with_context(|| format!("provider {name:?} is not configured"))?
                    .clone();
                (name.to_owned(), cfg)
            }
            None => {
                let kind = draft.kind.context("kind is required")?;
                let name = match (&draft.name, kind) {
                    (Some(n), _) => n.trim().to_owned(),
                    (None, ProviderKind::Anthropic) => "anthropic".into(),
                    (None, ProviderKind::OpenaiCompat) => {
                        let url = draft.base_url.as_deref().unwrap_or_default();
                        unused_name(&config, &suggest_name(url))
                    }
                };
                check_provider_name(&name)?;
                if config.providers.contains_key(&name) {
                    bail!("a provider named {name:?} already exists; choose another name");
                }
                let cfg = ProviderConfig {
                    kind,
                    base_url: None,
                    api_key_secret: None,
                    context_window: None,
                    prices: Default::default(),
                };
                (name, cfg)
            }
        };
        if let Some(url) = draft
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            let url = url.trim_end_matches('/');
            let tidy;
            let url = if cfg.kind == ProviderKind::OpenaiCompat {
                tidy = tern_core::presets::normalize_base_url(url);
                if tidy != url {
                    warnings.push(format!("Using {tidy}: that is the address {} expects.", host_of(&tidy)));
                }
                tidy.as_str()
            } else {
                url
            };
            let default =
                cfg.kind == ProviderKind::Anthropic && url == ProviderConfig::ANTHROPIC_BASE_URL;
            cfg.base_url = (!default).then(|| url.to_owned());
        }
        if cfg.kind == ProviderKind::OpenaiCompat && cfg.base_url.is_none() {
            bail!("a base URL is required, e.g. https://api.openai.com/v1");
        }
        if let Some(base) = cfg.effective_base_url() {
            tern_providers::models::check_url(base).map_err(|e| anyhow::anyhow!("{e}"))?;
        }

        let pasted = match draft.key.as_deref().filter(|k| !k.trim().is_empty()) {
            Some(raw) => Some(normalize_key(raw)?),
            None => None,
        };
        let secret = cfg
            .api_key_secret
            .clone()
            .unwrap_or_else(|| secret_name_for(&name));
        let env_key = std::env::var(&secret).ok().filter(|v| !v.is_empty());
        let key = match (&pasted, draft.use_env_key) {
            (Some(k), _) => {
                if env_key.as_deref().is_some_and(|e| e != k) {
                    warnings.push(format!(
                        "{secret} in your environment overrides keys saved here. \
                         Unset it, or the saved key won't be used."
                    ));
                }
                Some(k.clone())
            }
            (None, true) => {
                Some(env_key.with_context(|| format!("{secret} is not set in the environment"))?)
            }
            (None, false) => cfg
                .api_key_secret
                .as_ref()
                .and_then(|s| self.secrets.get(s)),
        };
        if key.is_some() {
            cfg.api_key_secret = Some(secret);
        }
        if cfg.kind == ProviderKind::Anthropic
            && let Some(k) = &key
            && !k.starts_with(ANTHROPIC_PREFIX)
        {
            warnings.push(format!(
                "Anthropic keys usually start with {ANTHROPIC_PREFIX}."
            ));
        }
        Ok(Plan {
            name,
            cfg,
            new_key: pasted,
            key,
            warnings,
        })
    }

    /// Check a draft against the provider without saving anything.
    pub async fn verify_provider(
        &self,
        draft: &ProviderDraft,
        existing: Option<&str>,
    ) -> anyhow::Result<Verified> {
        let plan = self.plan(draft, existing)?;
        Ok(verify(&plan).await)
    }

    /// Verify, store the key, write `config.toml` and reload. Sets the
    /// default model to `default_model` (resolved against the fresh list),
    /// or to the first listed model when there is no usable default.
    pub async fn save_provider(
        &self,
        draft: &ProviderDraft,
        existing: Option<&str>,
        default_model: Option<&str>,
    ) -> anyhow::Result<(ProviderStatus, Verified)> {
        let mut plan = self.plan(draft, existing)?;
        let verified = verify(&plan).await;
        if !verified.ok {
            bail!(
                "{}",
                verified
                    .message
                    .clone()
                    .unwrap_or_else(|| "verification failed".into())
            );
        }
        if let Some(url) = &verified.suggested_base_url {
            plan.cfg.base_url = Some(url.clone());
        }
        if let (Some(key), Some(secret)) = (&plan.new_key, &plan.cfg.api_key_secret) {
            self.secrets.set(secret, key)?;
        }
        let set_default = match default_model {
            Some(m) => Some(full_model(&plan.name, m)),
            None if self.setup_needed() => verified
                .models
                .first()
                .map(|m| format!("{}:{}", plan.name, m.id)),
            None => None,
        };
        let mut file = ConfigFile::open(&self.home.config_path())?;
        file.upsert_provider(&plan.name, &plan.cfg)?;
        if let Some(m) = &set_default {
            file.set_value("default_model", self.default_chain(m))?;
        }
        file.save()?;
        self.models.forget(&plan.name);
        let fp = fingerprint(&plan.cfg, plan.key.as_deref());
        let listing = Ok(Listing {
            models: verified.models.clone(),
            suggested_base_url: None,
        });
        self.models.put(&plan.name, &fp, &listing);
        self.reload_providers()?;
        tracing::info!(provider = %plan.name, "provider saved");
        let status = self
            .provider_statuses()
            .into_iter()
            .find(|s| s.name == plan.name)
            .context("provider vanished after saving it")?;
        Ok((status, verified))
    }

    /// Remove a provider. Drops it from `default_model` (a chain keeps its
    /// other models; nothing left clears the key), and with `remove_secret` deletes its key unless something else uses it.
    pub fn remove_provider(&self, name: &str, remove_secret: bool) -> anyhow::Result<()> {
        let config = self.live();
        let p = config
            .providers
            .get(name)
            .with_context(|| format!("provider {name:?} is not configured"))?;
        let mut file = ConfigFile::open(&self.home.config_path())?;
        file.remove_provider(name);
        if let Some(chain) = &config.default_model {
            let uses = |m: &String| {
                m.parse::<ModelRef>()
                    .is_ok_and(|m: ModelRef| m.provider == name)
            };
            if chain.entries().iter().any(uses) {
                let kept = chain.entries().iter().filter(|m| !uses(m)).cloned();
                match ModelChain::new(kept.collect()) {
                    Some(rest) => file.set_value("default_model", rest.to_toml())?,
                    None => {
                        file.remove("default_model");
                    }
                }
            }
        }
        let saved = file.save()?;
        if remove_secret && let Some(secret) = &p.api_key_secret {
            let still_used = saved.secret_uses().iter().any(|(s, _)| s == secret)
                || self
                    .connector_secret_uses()
                    .iter()
                    .any(|(s, _)| s == secret);
            if !still_used {
                self.secrets.remove(secret)?;
            }
        }
        self.models.forget(name);
        self.reload_providers()?;
        tracing::info!(provider = %name, "provider removed");
        Ok(())
    }

    pub fn set_default_model(&self, model: &str) -> anyhow::Result<Resolved> {
        let resolved = self.resolve_model(model)?;
        let mut file = ConfigFile::open(&self.home.config_path())?;
        file.set_value("default_model", self.default_chain(&resolved.model))?;
        file.save()?;
        self.reload_providers()?;
        Ok(resolved)
    }

    /// `default_model` with `model` first: a configured chain keeps its
    /// fallbacks (T6a), so picking a model doesn't drop them.
    fn default_chain(&self, model: &str) -> toml_edit::Value {
        match &self.live().default_model {
            Some(chain) => chain.with_primary(model).to_toml(),
            None => model.into(),
        }
    }
}

/// The friendly preset a configured provider matches, by URL.
pub fn preset_of(cfg: &ProviderConfig) -> Option<&'static tern_core::presets::Preset> {
    tern_core::presets::for_url(cfg.kind, cfg.base_url.as_deref())
}

/// A provider's display name: its preset's label, else its config name.
pub fn label_of(name: &str, cfg: &ProviderConfig) -> String {
    match preset_of(cfg) {
        Some(p) if p.id != "other" => p.label.to_owned(),
        _ => name.to_owned(),
    }
}

/// A failed model call for `model` (`provider:id`) in plain words, with the
/// raw error kept after it: `OpenRouter couldn't find that endpoint or
/// model. Check the model name. (details: HTTP 404: ...)`. The raw text
/// alone when it is not an error we know how to explain.
pub fn explain_failure(config: &Config, model: &str, raw: &str) -> String {
    let provider = model.split_once(':').map_or(model, |(p, _)| p);
    let label = config
        .providers
        .get(provider)
        .map_or_else(|| provider.to_owned(), |p| label_of(provider, p));
    match tern_core::models::friendly_call_error(raw, &label) {
        Some(f) => format!("{} (details: {raw})", f.message),
        None => raw.to_owned(),
    }
}

/// `model` as `provider:model`, unless it already names `provider`.
fn full_model(provider: &str, model: &str) -> String {
    match model.split_once(':') {
        Some((p, _)) if p == provider => model.to_owned(),
        _ => format!("{provider}:{model}"),
    }
}

fn unused_name(config: &Config, base: &str) -> String {
    if !config.providers.contains_key(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|n| !config.providers.contains_key(n))
        .unwrap_or_else(|| base.to_owned())
}

async fn verify(plan: &Plan) -> Verified {
    let result = list_models(&plan.cfg, plan.key.as_deref()).await;
    let base = plan.cfg.effective_base_url();
    let mut v = Verified {
        ok: true,
        name: plan.name.clone(),
        error: None,
        message: None,
        warnings: plan.warnings.clone(),
        suggested_base_url: None,
        key_hint: plan.key.as_deref().map(mask_key),
        models: Vec::new(),
    };
    match result {
        Ok(l) => {
            v.models = l.models;
            v.suggested_base_url = l.suggested_base_url;
        }
        Err(e) => {
            v.ok = e.key_accepted();
            v.message = Some(describe(&e, &plan.name, base));
            v.error = Some(e);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_names() {
        assert_eq!(suggest_name("https://api.openai.com/v1"), "openai");
        assert_eq!(suggest_name("https://openrouter.ai/api/v1"), "openrouter");
        assert_eq!(suggest_name("http://localhost:11434/v1"), "ollama");
        assert_eq!(suggest_name("http://127.0.0.1:1234/v1"), "lmstudio");
        assert_eq!(suggest_name("http://192.168.1.4:8000/v1"), "local");
        assert_eq!(suggest_name(""), "custom");
    }

    #[test]
    fn provider_names() {
        check_provider_name("openai").unwrap();
        check_provider_name("my-proxy_2").unwrap();
        assert!(check_provider_name("").is_err());
        assert!(check_provider_name("Open AI").is_err());
        assert!(check_provider_name("a:b").is_err());
        assert!(check_provider_name("2fast").is_err());
    }

    #[test]
    fn describes_errors() {
        let base = Some("http://localhost:11434/v1");
        assert_eq!(
            describe(
                &ListError::Unreachable("connection refused".into()),
                "ollama",
                base
            ),
            "Couldn't reach localhost: connection refused. Is the server running?"
        );
        assert_eq!(
            describe(&ListError::Unauthorized, "anthropic", None),
            "anthropic rejected that key. Check you copied all of it."
        );
    }

    #[test]
    fn explains_a_failed_call_with_the_raw_text_kept() {
        let config = Config::parse(
            "[providers.openrouter]\nkind = \"openai_compat\"\nbase_url = \"https://openrouter.ai/api/v1\"\n",
            std::path::Path::new("config.toml"),
        )
        .unwrap();
        let raw = r#"HTTP 404: {"error":{"message":"Not Found","code":404}}"#;
        let text = explain_failure(&config, "openrouter:minimax/m", raw);
        assert!(text.starts_with("OpenRouter couldn't find that endpoint or model."));
        assert!(text.ends_with(&format!("(details: {raw})")));
        // Unknown text and unknown providers fall back sensibly.
        assert_eq!(explain_failure(&config, "x:y", "tool failed"), "tool failed");
        assert!(explain_failure(&config, "mine:y", "HTTP 401: no").starts_with("mine rejected"));
    }

    #[test]
    fn full_model_names() {
        assert_eq!(full_model("ollama", "llama3:8b"), "ollama:llama3:8b");
        assert_eq!(full_model("ollama", "ollama:llama3"), "ollama:llama3");
    }
}
