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

/// kv namespace recording what the user approved at activation.
pub const APPROVALS_NS: &str = "lua:approved";

/// What a connector is, for approval purposes: its exact source and the
/// capabilities it declared when loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub hash: String,
    pub capabilities: Capabilities,
}

impl Fingerprint {
    fn to_json(&self) -> String {
        serde_json::json!({
            "hash": self.hash,
            "http": self.capabilities.http,
            "secrets": self.capabilities.secrets,
        })
        .to_string()
    }

    fn from_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        let list = |k: &str| -> Option<Vec<String>> {
            v[k].as_array()?
                .iter()
                .map(|x| x.as_str().map(str::to_owned))
                .collect()
        };
        Some(Self {
            hash: v["hash"].as_str()?.to_owned(),
            capabilities: Capabilities {
                http: list("http")?,
                secrets: list("secrets")?,
            },
        })
    }
}

/// A loaded connector together with the exact bytes it was loaded from.
pub struct Loaded {
    pub vm: Vm,
    pub fingerprint: Fingerprint,
    pub source: String,
}

/// Load one connector file, reading it once so the fingerprint describes
/// exactly what was loaded.
pub fn load_fingerprinted(host: &Host, path: &Path) -> Result<Loaded, LoadError> {
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
    let fingerprint = Fingerprint {
        hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
        capabilities: vm.spec.capabilities.clone(),
    };
    Ok(Loaded {
        vm,
        fingerprint,
        source,
    })
}

/// Load one connector file.
pub fn load_file(host: &Host, path: &Path) -> Result<Vm, LoadError> {
    load_fingerprinted(host, path).map(|l| l.vm)
}

/// Load an active connector. One activated by `connector_activate` must
/// still be exactly what the user approved: same file, same capabilities
/// (capabilities computed by code, e.g. from the time, would differ).
/// Connectors the user installed by hand have no record and load as-is.
pub fn load_active(host: &Host, path: &Path) -> Result<Vm, LoadError> {
    let loaded = load_fingerprinted(host, path)?;
    let name = &loaded.vm.spec.name;
    let record = host
        .store
        .kv_get(APPROVALS_NS, name)
        .ok()
        .flatten()
        .and_then(|r| Fingerprint::from_json(&r));
    if let Some(approved) = record
        && approved != loaded.fingerprint
    {
        return Err(LoadError::Invalid(format!(
            "{name} changed since you approved it (its file or declared capabilities differ); activate it again or remove it"
        )));
    }
    Ok(loaded.vm)
}

/// Remember what the user approved for `name`.
pub fn record_approval(host: &Host, name: &str, fp: &Fingerprint) -> Result<(), String> {
    host.store
        .kv_set(APPROVALS_NS, name, &fp.to_json())
        .map_err(|e| e.to_string())
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
struct LoadedSet {
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
    state: RwLock<LoadedSet>,
}

impl Connectors {
    pub fn open(host: Host, dir: PathBuf) -> Self {
        let c = Self {
            host,
            dir,
            state: RwLock::new(LoadedSet::default()),
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
        let mut loaded = LoadedSet {
            fingerprint: fp.clone(),
            ..LoadedSet::default()
        };
        for (path, _) in &fp {
            match load_active(&self.host, path) {
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

    /// `(secret, "connector <name>")` for every secret an active connector declares.
    pub fn secret_uses(&self) -> Vec<(String, String)> {
        self.specs()
            .into_iter()
            .flat_map(|s| {
                let user = format!("connector {}", s.name);
                s.capabilities
                    .secrets
                    .into_iter()
                    .map(move |secret| (secret, user.clone()))
            })
            .collect()
    }
}

/// `(secret, what reads it)` for every secret a connector in `active` or
/// `drafts` declares, naming the hosts it can send it to, e.g.
/// `connector weather (draft) · reaches api.weather.com`. Shown when the
/// agent asks the user for a secret, so they can see where it would go.
pub fn describe_secret_uses(host: &Host, active: &Path, drafts: &Path) -> Vec<(String, String)> {
    let mut uses = Vec::new();
    for (dir, draft) in [(active, false), (drafts, true)] {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == EXTENSION))
            .collect();
        files.sort();
        for path in files {
            let Ok(vm) = load_file(host, &path) else {
                continue;
            };
            let caps = &vm.spec.capabilities;
            let mut user = format!("connector {}", vm.spec.name);
            if draft {
                user.push_str(" (draft)");
            }
            if !caps.http.is_empty() {
                user.push_str(&format!(" · reaches {}", caps.http.join(", ")));
            }
            uses.extend(caps.secrets.iter().map(|s| (s.clone(), user.clone())));
        }
    }
    uses
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
