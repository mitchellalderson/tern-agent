//! The three model tiers and `decide()` (the host decides, not the model).
//!
//! - **agent model**: `default_model`, what runs use.
//! - **fast model** (`fast_model`): background writing (role lines, carry-over,
//!   summaries), and the fallback for decisions.
//! - **decision model** (`decision_model`): quick choices.
//!
//! [`Runtime::decide`] asks typed questions (choice, yes/no, score) about one
//! text. Primary path: one call to OpenRouter's Decisions API when the decision
//! model is an OpenRouter one. Fallback, on no key, an error or a 3 s timeout:
//! the fast model, one structured call with the schema in the prompt and one
//! corrective retry. Every call is bounded. A refusal or missing answer is
//! [`Answer::Unanswered`], never "no": callers choose their own safe default.
//! Quick calls turn reasoning off.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures::StreamExt;
use serde_json::Value;
use tern_core::config::{ModelRef, ProviderKind};
use tern_core::decide::{self, Answer, Answers, Questions};
use tern_core::types::{CacheHint, ChatRequest, ContentBlock, Message, Role, StreamEvent};

use crate::runtime::Runtime;

/// The Decisions API gets this long before the fast model takes over.
const DECISIONS_TIMEOUT: Duration = Duration::from_secs(3);
/// One structured call on the fast model.
const FALLBACK_TIMEOUT: Duration = Duration::from_secs(12);
/// Characters of `state` sent.
const STATE_CHARS: usize = 8000;
const DEFAULT_FAST: &str = "anthropic:claude-haiku-4-5";

#[derive(Default)]
struct Learned {
    /// OpenRouter models that aren't decision models (the API refused them).
    not_decision: HashSet<String>,
    /// The decision model found on OpenRouter: `Some(None)` = looked, none.
    discovered: Option<Option<String>>,
}

fn learned() -> &'static Mutex<Learned> {
    static L: OnceLock<Mutex<Learned>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// A decisions-capable endpoint: where, with which key, for which model.
struct Endpoint {
    model: String,
    url: String,
    key: String,
}

impl Runtime {
    /// The fast model: `fast_model`; else Haiku when Anthropic is set up;
    /// else the cheapest model with a known price; else the agent model.
    pub fn fast_model(&self) -> anyhow::Result<ModelRef> {
        let config = self.live();
        if let Some(m) = &config.fast_model {
            return Ok(m.primary().parse()?);
        }
        if self.mock {
            return self.pick_model(None, None);
        }
        if config.providers.contains_key("anthropic") {
            return Ok(DEFAULT_FAST.parse()?);
        }
        let mut candidates: Vec<String> = config
            .model_chains()
            .into_iter()
            .flat_map(|(_, c)| c.entries().to_vec())
            .collect();
        if let Some(Ok(m)) = config.default_model_ref() {
            candidates.push(m.to_string());
        }
        let cheapest = candidates
            .iter()
            .filter(|m| {
                m.parse::<ModelRef>()
                    .is_ok_and(|r| config.providers.contains_key(&r.provider))
            })
            .filter_map(|m| self.price_of(m).map(|p| (p.input + p.output, m)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, m)| m.clone());
        match cheapest {
            Some(m) => Ok(m.parse()?),
            None => self.pick_model(None, None),
        }
    }

    /// One short text call on `model`: no tools, reasoning off, bounded.
    pub async fn quick_text(
        &self,
        model: &ModelRef,
        system: &str,
        messages: Vec<Message>,
        max_tokens: u32,
        timeout: Duration,
    ) -> anyhow::Result<String> {
        let provider = self.agent.provider(model)?;
        let req = ChatRequest {
            model: model.model.clone(),
            system: system.into(),
            messages,
            tools: Vec::new(),
            max_tokens,
            forbid_tool_calls: false,
            cache: CacheHint {
                quick: true,
                ..Default::default()
            },
        };
        let call = async {
            let mut stream = provider
                .stream(req, tokio_util::sync::CancellationToken::new())
                .await?;
            let mut text = String::new();
            while let Some(ev) = stream.next().await {
                if let StreamEvent::TextDelta(t) = ev? {
                    text.push_str(&t);
                }
            }
            anyhow::Ok(text)
        };
        tokio::time::timeout(timeout, call)
            .await
            .map_err(|_| anyhow::anyhow!("{model} took over {}s", timeout.as_secs()))?
    }

    /// [`Runtime::quick_text`] on the fast model.
    pub async fn fast_text(
        &self,
        system: &str,
        user: String,
        max_tokens: u32,
        timeout: Duration,
    ) -> anyhow::Result<String> {
        let model = self.fast_model()?;
        self.quick_text(
            &model,
            system,
            vec![Message::user_text(user)],
            max_tokens,
            timeout,
        )
        .await
    }

    /// The Decisions API endpoint for the decision model, if there is one:
    /// an OpenRouter model, with its key set, not known to be a plain chat
    /// model.
    async fn decisions_endpoint(&self) -> Option<Endpoint> {
        if self.mock {
            return None;
        }
        let config = self.live();
        let configured = config
            .decision_model
            .as_ref()
            .map(|c| c.primary().to_owned());
        let provider_name = match &configured {
            Some(m) => m.parse::<ModelRef>().ok()?.provider,
            None => config
                .providers
                .iter()
                .find(|(_, p)| is_openrouter(p))
                .map(|(n, _)| n.clone())?,
        };
        let provider = config.providers.get(&provider_name)?;
        if !is_openrouter(provider) {
            return None;
        }
        let key = self.secrets.get(provider.api_key_secret.as_deref()?)?;
        let base = provider.effective_base_url()?;
        let url = reqwest::Url::parse(base).ok()?;
        let at = |path: &str| {
            let mut u = url.clone();
            u.set_path(path);
            u.to_string()
        };
        let (url, models_url) = (at("/api/alpha/decisions"), at("/api/v1/models"));
        let model = match configured {
            Some(m) => m.parse::<ModelRef>().ok()?.model,
            None => self.discover_decision_model(&models_url, &key).await?,
        };
        if learned().lock().ok()?.not_decision.contains(&model) {
            return None;
        }
        Some(Endpoint { model, url, key })
    }

    /// An OpenRouter decision model, found once per process: a Liquid one if
    /// listed, else the first.
    async fn discover_decision_model(&self, models_url: &str, key: &str) -> Option<String> {
        if let Some(found) = learned().lock().ok()?.discovered.clone() {
            return found;
        }
        let client = tern_core::net::http_client().build().ok()?;
        let fetch = async {
            let v: Value = client
                .get(format!("{models_url}?output_modalities=decisions"))
                .bearer_auth(key)
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?
                .json()
                .await
                .ok()?;
            let ids: Vec<String> = v
                .get("data")?
                .as_array()?
                .iter()
                .filter_map(|m| m.get("id")?.as_str().map(str::to_owned))
                .collect();
            Some(
                ids.iter()
                    .find(|i| i.to_lowercase().contains("liquid"))
                    .or(ids.first())
                    .cloned(),
            )
        };
        let found = tokio::time::timeout(DECISIONS_TIMEOUT, fetch)
            .await
            .ok()
            .flatten();
        // A failed lookup isn't remembered: the next call tries again.
        let found = found?;
        learned().lock().ok()?.discovered = Some(found.clone());
        found
    }

    async fn decisions_call(
        &self,
        ep: &Endpoint,
        state: &str,
        questions: &Questions,
    ) -> Result<Answers, String> {
        let client = tern_core::net::http_client()
            .build()
            .map_err(|e| e.to_string())?;
        let body = decide::decisions_body(&ep.model, state, questions);
        let call = async {
            let resp = client
                .post(&ep.url)
                .bearer_auth(&ep.key)
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            if !status.is_success() {
                if status.as_u16() == 400
                    && let Ok(mut l) = learned().lock()
                {
                    // Most likely a chat model, not a decision model.
                    l.not_decision.insert(ep.model.clone());
                    l.discovered = None;
                }
                return Err(format!("decisions API answered {status}"));
            }
            resp.json::<Value>().await.map_err(|e| e.to_string())
        };
        let v = tokio::time::timeout(DECISIONS_TIMEOUT, call)
            .await
            .map_err(|_| "decisions API took over 3s".to_owned())??;
        Ok(decide::parse_decisions(&v, questions))
    }

    /// Ask `questions` about `state`. Always returns an answer per question;
    /// one that couldn't be answered is [`Answer::Unanswered`].
    pub async fn decide(&self, state: &str, questions: Questions) -> Answers {
        if questions.is_empty() {
            return Answers::new();
        }
        let state: String = state.chars().take(STATE_CHARS).collect();
        if let Some(ep) = self.decisions_endpoint().await {
            match self.decisions_call(&ep, &state, &questions).await {
                Ok(a) => return a,
                Err(e) => tracing::debug!(error = %e, "decisions API failed; using the fast model"),
            }
        }
        self.decide_with_fast(&state, &questions).await
    }

    /// The fallback: one structured call, one corrective retry.
    async fn decide_with_fast(&self, state: &str, questions: &Questions) -> Answers {
        let unanswered = || {
            questions
                .iter()
                .map(|(id, _)| (id.clone(), Answer::Unanswered))
                .collect()
        };
        // A decision model that isn't a Decisions API model is asked as a
        // chat model (the old `gate_model`); else the fast model.
        let model = match self.live().decision_model.as_ref() {
            Some(c) => c.primary().parse::<ModelRef>().ok(),
            None => None,
        }
        .or_else(|| self.fast_model().ok());
        let Some(model) = model else {
            return unanswered();
        };
        let first = Message::user_text(decide::fallback_prompt(state, questions));
        let mut messages = vec![first];
        let mut best: Option<Answers> = None;
        for attempt in 0..2 {
            let text = match self
                .quick_text(
                    &model,
                    decide::FALLBACK_SYSTEM,
                    messages.clone(),
                    300,
                    FALLBACK_TIMEOUT,
                )
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %e, "decision call failed");
                    break;
                }
            };
            let (answers, all) = decide::parse_fallback(&text, questions);
            if all || attempt == 1 {
                return answers;
            }
            best = Some(answers);
            messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text }],
            });
            messages.push(Message::user_text(decide::RETRY_NOTE));
        }
        best.unwrap_or_else(unanswered)
    }
}

fn is_openrouter(p: &tern_core::config::ProviderConfig) -> bool {
    p.kind == ProviderKind::OpenaiCompat
        && p.effective_base_url()
            .is_some_and(|u| tern_core::models::host_of(u) == "openrouter.ai")
}
