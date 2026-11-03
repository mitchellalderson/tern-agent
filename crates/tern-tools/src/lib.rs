//! Built-in tools (spec §8.1).

pub mod fs;
pub mod http;
pub mod memory;
pub mod paths;
pub mod schedule;
pub mod shell;
pub mod skills;

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
    registry.register(Arc::new(fs::ReadFile));
    registry.register(Arc::new(fs::ListDir));
    registry.register(Arc::new(fs::SearchFiles));
    registry.register(Arc::new(fs::WriteFile));
    registry.register(Arc::new(fs::EditFile));
    registry.register(Arc::new(shell::Shell::new(
        opts.sandbox,
        opts.shell_timeout,
    )));
    registry.register(Arc::new(http::HttpFetch::new().map_err(|e| e.to_string())?));
    registry.register(Arc::new(memory::MemorySave::new(opts.store.clone())));
    registry.register(Arc::new(memory::MemorySearch::new(opts.store.clone())));
    registry.register(Arc::new(schedule::ScheduleCreate {
        store: opts.store.clone(),
    }));
    registry.register(Arc::new(schedule::ScheduleList {
        store: opts.store.clone(),
    }));
    registry.register(Arc::new(schedule::ScheduleDelete {
        store: opts.store.clone(),
    }));
    registry.register(Arc::new(memory::SessionSearch::new(opts.store)));
    registry.register(Arc::new(skills::SkillRead::new(opts.skills_dir)));
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
