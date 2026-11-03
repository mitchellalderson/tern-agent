//! Sandboxed Luau connectors (spec §10): user- or agent-written tools in
//! `~/.tern/connectors/*.luau`, each in its own VM with declared capabilities.

mod host;
mod spec;
pub mod tools;

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use async_trait::async_trait;
use serde_json::Value;
use tern_core::tool::{Tool, ToolCtx, ToolPolicy, ToolResult, ToolSource};
use tern_core::types::ToolSpec;

pub use host::{DEFAULT_MEMORY, DEFAULT_TIMEOUT, Host, LoadError, Vm, host_allowed};
pub use spec::{Capabilities, ConnectorSpec, ToolDef, check_schema, valid_ident};

pub const EXTENSION: &str = "luau";

/// Load one connector file.
pub fn load_file(host: &Host, path: &Path) -> Result<Vm, LoadError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| LoadError::Lua(format!("reading {}: {e}", path.display())))?;
    let chunk = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let vm = Vm::load(host, &chunk, &source)?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if vm.spec.name != stem {
        return Err(LoadError::Invalid(format!(
            "file {chunk} declares name {:?}; the name must match the file name",
            vm.spec.name
        )));
    }
    Ok(vm)
}

/// A Luau connector tool exposed to the agent as `lua__<connector>__<tool>`.
pub struct LuaTool {
    vm: Arc<Vm>,
    def: ToolDef,
}

pub fn tool_name(connector: &str, tool: &str) -> String {
    format!("lua__{connector}__{tool}")
}

#[async_trait]
impl Tool for LuaTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: tool_name(&self.vm.spec.name, &self.def.name),
            description: format!(
                "{} (Luau connector {})",
                self.def.description, self.vm.spec.name
            ),
            input_schema: self.def.params.clone(),
        }
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy {
            idempotent: self.def.idempotent,
            requires_approval: self.def.requires_approval,
        }
    }

    async fn call(&self, _: &ToolCtx, args: Value) -> ToolResult {
        match self.vm.call(&self.def.name, &args).await {
            Ok(out) => ToolResult::ok(out),
            Err(e) => ToolResult::error(format!("{}.{}: {e}", self.vm.spec.name, self.def.name)),
        }
    }
}

#[derive(Default)]
struct Loaded {
    fingerprint: Vec<(PathBuf, Option<SystemTime>)>,
    vms: Vec<Arc<Vm>>,
    errors: Vec<(PathBuf, String)>,
}

/// The active connectors in a directory. Reloads (on `prepare`) when files
/// are added, removed, or modified, so activated drafts show up on the
/// next run.
pub struct Connectors {
    host: Host,
    dir: PathBuf,
    state: RwLock<Loaded>,
}

impl Connectors {
    pub fn open(host: Host, dir: PathBuf) -> Self {
        let c = Self {
            host,
            dir,
            state: RwLock::new(Loaded::default()),
        };
        c.reload_if_changed();
        c
    }

    fn fingerprint(&self) -> Vec<(PathBuf, Option<SystemTime>)> {
        let mut files: Vec<(PathBuf, Option<SystemTime>)> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == EXTENSION))
            .map(|p| {
                let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                (p, mtime)
            })
            .collect();
        files.sort();
        files
    }

    /// Reload if the directory changed. Returns true if it reloaded.
    pub fn reload_if_changed(&self) -> bool {
        let fp = self.fingerprint();
        if self.state.read().is_ok_and(|s| s.fingerprint == fp) && !fp.is_empty() {
            return false;
        }
        let mut loaded = Loaded {
            fingerprint: fp.clone(),
            ..Loaded::default()
        };
        for (path, _) in &fp {
            match load_file(&self.host, path) {
                Ok(vm) => loaded.vms.push(Arc::new(vm)),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to load connector");
                    loaded.errors.push((path.clone(), e.to_string()));
                }
            }
        }
        if let Ok(mut s) = self.state.write() {
            *s = loaded;
        }
        true
    }

    pub fn specs(&self) -> Vec<ConnectorSpec> {
        self.state
            .read()
            .map(|s| s.vms.iter().map(|v| v.spec.clone()).collect())
            .unwrap_or_default()
    }

    pub fn errors(&self) -> Vec<(PathBuf, String)> {
        self.state
            .read()
            .map(|s| s.errors.clone())
            .unwrap_or_default()
    }

    /// Every secret name any loaded connector may read (for redaction).
    pub fn secret_names(&self) -> Vec<String> {
        self.specs()
            .into_iter()
            .flat_map(|s| s.capabilities.secrets)
            .collect()
    }
}

#[async_trait]
impl ToolSource for Connectors {
    async fn prepare(&self) {
        self.reload_if_changed();
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let Ok(s) = self.state.read() else {
            return Vec::new();
        };
        s.vms
            .iter()
            .flat_map(|vm| {
                vm.spec.tools.iter().map(|def| {
                    Arc::new(LuaTool {
                        vm: vm.clone(),
                        def: def.clone(),
                    }) as Arc<dyn Tool>
                })
            })
            .collect()
    }

    fn describe(&self) -> Vec<String> {
        self.specs()
            .into_iter()
            .map(|s| {
                format!(
                    "Luau connector {}: {} (tools lua__{}__*)",
                    s.name, s.description, s.name
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
