//! Built-in tools (spec §8.1).

pub mod agents;
pub mod channels;
pub mod documents;
pub mod fs;
pub mod history;
pub mod http;
pub mod memory;
pub mod schedule;
pub mod secret_request;
pub mod shell;
pub mod shell_class;
pub mod shell_read;
pub mod shell_script;
pub mod skills;
pub mod wait;

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tern_core::tool::{ToolRegistry, ToolResult};
use tern_core::types::ToolSpec;
use tern_sandbox::SandboxBackend;

pub struct BuiltinOptions {
    pub store: tern_store::Store,
    pub skills_dir: std::path::PathBuf,
    pub sandbox: Arc<dyn SandboxBackend>,
    pub shell_timeout: Duration,
}

/// Register the built-ins: file, shell, http, memory, and session search.
pub fn register_builtins(registry: &mut ToolRegistry, opts: BuiltinOptions) -> Result<(), String> {
    for tool in fs::all() {
        registry.register(tool);
    }
    registry.register(Arc::new(shell::Shell::new(
        opts.sandbox,
        opts.shell_timeout,
    )));
    registry.register(Arc::new(http::HttpFetch::new().map_err(|e| e.to_string())?));
    registry.register(Arc::new(memory::MemorySave::new(opts.store.clone())));
    registry.register(Arc::new(memory::MemorySearch::new(opts.store.clone())));
    registry.register(Arc::new(schedule::ScheduleCreate {
        store: opts.store.clone(),
        notify: None,
        checks: None,
    }));
    registry.register(Arc::new(schedule::ScheduleUpdate {
        store: opts.store.clone(),
        notify: None,
        checks: None,
    }));
    registry.register(Arc::new(schedule::ScheduleList {
        store: opts.store.clone(),
    }));
    registry.register(Arc::new(schedule::ScheduleDelete {
        store: opts.store.clone(),
        notify: None,
    }));
    wait::register(registry, opts.store.clone(), wait::DEFAULT_MAX_ACTIVE, None);
    documents::register(registry, opts.store.clone());
    registry.register(Arc::new(history::HistorySearch::new(opts.store.clone())));
    registry.register(Arc::new(memory::SessionSearch::new(opts.store.clone())));
    registry.register(Arc::new(
        skills::SkillRead::new(opts.skills_dir).with_store(opts.store),
    ));
    Ok(())
}

pub(crate) fn spec(name: &str, description: &str, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

pub(crate) fn args<T: DeserializeOwned>(raw: Value) -> Result<T, ToolResult> {
    serde_json::from_value(raw).map_err(|e| ToolResult::error(format!("invalid arguments: {e}")))
}

#[cfg(test)]
mod ingestion_tests {
    use super::*;
    use tern_core::tool::Ingestion;

    /// Every built-in that reaches the network or returns outside content
    /// declares an ingestion kind (quarantined-fetch design F0): a new path
    /// must choose a trust level.
    #[test]
    fn network_and_outside_content_tools_declare_an_ingestion_kind() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::new();
        register_builtins(
            &mut registry,
            BuiltinOptions {
                store: tern_store::Store::open_in_memory().unwrap(),
                skills_dir: dir.path().join("skills"),
                sandbox: Arc::new(tern_sandbox::HostBackend::default()),
                shell_timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        assert_eq!(registry.ingestion("http_fetch"), Ingestion::Web);
        // The default host backend has no network sandbox guarantee here, so
        // the shell counts as network-reachable and says so.
        assert_eq!(registry.ingestion("shell"), Ingestion::Network);
        for quiet in ["read_file", "list_dir", "write_file", "memory_save"] {
            assert_eq!(registry.ingestion(quiet), Ingestion::None, "{quiet}");
        }
    }
}
