//! The Luau side of routines (tooling spec §3.2, §3.3; T1b): checking a
//! watch when a routine is created, and polling it. The connector's
//! sandbox does the work (`tern_lua::watch`).

use std::path::PathBuf;

use async_trait::async_trait;
use futures::StreamExt;
use tern_core::config::ModelRef;
use tern_core::initiative::{GATE_SYSTEM, GateAnswer, gate_prompt, parse_gate_answer};
use tern_core::types::{ChatRequest, Message, StreamEvent};
use tern_store::Store;
use tern_tools::schedule::{RoutineChecks, WatchSummary};

/// Checks against the active connectors in `dir`.
pub struct LuaChecks {
    host: tern_lua::Host,
    dir: PathBuf,
}

impl LuaChecks {
    pub fn new(host: tern_lua::Host, dir: PathBuf) -> Self {
        Self { host, dir }
    }
}

#[async_trait]
impl RoutineChecks for LuaChecks {
    async fn watch(&self, connector: &str, watch: &str) -> Result<WatchSummary, String> {
        let list = tern_lua::watch::watches(&self.host, &self.dir, connector).await?;
        let Some(info) = list.iter().find(|w| w.name == watch) else {
            let names: Vec<&str> = list.iter().map(|w| w.name.as_str()).collect();
            return Err(if names.is_empty() {
                format!("connector {connector} declares no watches")
            } else {
                format!(
                    "connector {connector} has no watch {watch:?} (it has {})",
                    names.join(", ")
                )
            });
        };
        let path = self
            .dir
            .join(format!("{connector}.{}", tern_lua::EXTENSION));
        let hosts = tern_lua::load_file(&self.host, &path)
            .map(|vm| vm.spec.capabilities.http)
            .unwrap_or_default();
        Ok(WatchSummary {
            default_every: info.default_every.clone(),
            hosts,
        })
    }

    async fn gate(&self, connector: Option<&str>, code: &str) -> Result<(), String> {
        tern_lua::watch::check_gate(&self.host, &self.dir, connector, code)
    }
}

/// A Luau host for routines: the same store and secrets as connectors.
pub fn lua_host(store: &Store, secrets: &tern_core::Secrets) -> anyhow::Result<tern_lua::Host> {
    Ok(tern_lua::Host::new(store.clone(), secrets.clone())?)
}

/// The model for the notification gate (tooling spec §4.1, §8.3):
/// `[schedules] gate_model` if set; else the cheapest configured model with
/// a known price (input plus output per million tokens), among every model
/// in a model setting or chain; else the routine run's own model.
pub fn gate_model(rt: &crate::runtime::Runtime, run_model: &str) -> anyhow::Result<ModelRef> {
    let config = rt.live();
    if let Some(m) = &config.schedules.gate_model {
        return Ok(m.primary().parse()?);
    }
    let mut candidates: Vec<String> = config
        .model_chains()
        .into_iter()
        .flat_map(|(_, c)| c.entries().to_vec())
        .collect();
    candidates.push(run_model.to_owned());
    let cheapest = candidates
        .iter()
        .filter(|m| {
            m.parse::<ModelRef>()
                .is_ok_and(|r| rt.mock || config.providers.contains_key(&r.provider))
        })
        .filter_map(|m| rt.price_of(m).map(|p| (p.input + p.output, m)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, m)| m.clone());
    Ok(cheapest.as_deref().unwrap_or(run_model).parse()?)
}

/// One short call on the gate model, no tools: `system` and `user` in, its
/// text out. Used for the notification gate and for routing a channel
/// post to agents. Gives up after `timeout`.
pub async fn gate_text(
    rt: &crate::runtime::Runtime,
    run_model: &str,
    system: &str,
    user: String,
    max_tokens: u32,
    timeout: std::time::Duration,
) -> anyhow::Result<String> {
    let model = gate_model(rt, run_model)?;
    let provider = rt.agent.provider(&model)?;
    let req = ChatRequest {
        model: model.model.clone(),
        system: system.into(),
        messages: vec![Message::user_text(user)],
        tools: Vec::new(),
        max_tokens,
        forbid_tool_calls: false,
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
    let text = tokio::time::timeout(timeout, call)
        .await
        .map_err(|_| anyhow::anyhow!("the gate model took over {}s", timeout.as_secs()))??;
    tracing::debug!(model = %model, answer = %text, "gate model answered");
    Ok(text)
}

/// Ask the gate model whether a `notify` routine's result is worth a push
/// now: one short call, no tools, answered as `{"interrupt", "headline"}`.
pub async fn ask_gate(
    rt: &crate::runtime::Runtime,
    purpose: &str,
    run_model: &str,
    result: &str,
) -> anyhow::Result<GateAnswer> {
    let text = gate_text(
        rt,
        run_model,
        GATE_SYSTEM,
        gate_prompt(purpose, result),
        200,
        std::time::Duration::from_secs(60),
    )
    .await?;
    parse_gate_answer(&text)
        .ok_or_else(|| anyhow::anyhow!("the gate model answered {text:?}, not the JSON asked for"))
}
