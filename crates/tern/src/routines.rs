//! The Luau side of routines (tooling spec §3.2, §3.3; T1b): checking a
//! watch when a routine is created, and polling it. The connector's
//! sandbox does the work (`tern_lua::watch`).

use std::path::PathBuf;

use async_trait::async_trait;
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

#[async_trait]
impl tern_tools::wait::WaitChecks for LuaChecks {
    async fn connector_tool(&self, connector: &str, tool: &str) -> Result<Vec<String>, String> {
        tern_lua::watch::read_tool_hosts(&self.host, &self.dir, connector, tool)
    }
}

/// A Luau host for routines: the same store and secrets as connectors.
pub fn lua_host(store: &Store, secrets: &tern_core::Secrets) -> anyhow::Result<tern_lua::Host> {
    Ok(tern_lua::Host::new(store.clone(), secrets.clone())?)
}

/// Ask the decision model whether a `notify` routine's result is worth a
/// push now (one yes/no through `decide()`). `None` when it couldn't answer:
/// the caller holds the result for the digest, never pushes on a guess.
pub async fn ask_gate(rt: &crate::runtime::Runtime, purpose: &str, result: &str) -> Option<bool> {
    use tern_core::decide::Question;
    use tern_core::initiative::{GATE_QUESTION, GATE_THRESHOLD, gate_state};
    let answers = rt
        .decide(
            &gate_state(purpose, result),
            vec![("interrupt".into(), Question::yes_no(GATE_QUESTION))],
        )
        .await;
    answers["interrupt"].is_yes(GATE_THRESHOLD)
}
