//! Built-in tools (spec §8.1).

pub mod fs;
pub mod http;
pub mod paths;
pub mod shell;

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tern_core::tool::{ToolRegistry, ToolResult};
use tern_core::types::ToolSpec;
use tern_sandbox::SandboxBackend;

pub struct BuiltinOptions {
    pub sandbox: Arc<dyn SandboxBackend>,
    pub shell_timeout: Duration,
}

/// Register the M1 built-ins: file, shell, and http tools.
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
