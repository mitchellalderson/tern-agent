//! Sandboxed Luau connectors (spec §10): user- or agent-written tools in
//! `~/.tern/connectors/*.luau`, each in its own VM with declared capabilities.

mod allow;
mod host;
mod limit;
mod spec;
pub mod tools;
pub mod watch;

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use async_trait::async_trait;
use serde_json::Value;
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult, ToolSource};
use tern_core::types::ToolSpec;

pub use allow::{host_allowed, path_allowed};
pub use host::{DEFAULT_MEMORY, DEFAULT_RATE, DEFAULT_TIMEOUT, Host, LoadError, Vm};
pub use spec::{Auth, Capabilities, ConnectorSpec, Inject, ToolDef, check_schema, valid_ident};

pub const EXTENSION: &str = "luau";

/// kv namespace recording what the user approved at activation.
pub const APPROVALS_NS: &str = "lua:approved";

/// kv namespace of connectors that keep M4's approval default: name → the
/// source hash they had when effects arrived (T0).
pub const GRANDFATHERED_NS: &str = "lua:grandfathered";
const META_NS: &str = "lua:meta";
const SNAPSHOT_KEY: &str = "effects_snapshot";

/// What a connector is, for approval purposes: its exact source and the
/// capabilities it declared when loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub hash: String,
    pub capabilities: Capabilities,
}

impl Fingerprint {
    fn to_json(&self) -> String {
        let c = &self.capabilities;
        let mut v = serde_json::json!({
            "hash": self.hash,
            "http": c.http,
            "secrets": c.secrets,
        });
        // Only when declared, so records written before T4b still match.
        if !c.auth.is_empty() {
            v["auth"] = serde_json::to_value(&c.auth).unwrap_or_default();
        }
        if let Some(r) = c.rate {
            v["rate"] = r.to_string().into();
        }
        if let Some(o) = &c.oauth {
            v["oauth"] = serde_json::to_value(o).unwrap_or_default();
        }
        v.to_string()
    }

    fn from_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        let list = |k: &str| -> Option<Vec<String>> {
            v[k].as_array()?
                .iter()
                .map(|x| x.as_str().map(str::to_owned))
                .collect()
        };
        let auth = match v.get("auth") {
            None => Vec::new(),
            Some(a) => serde_json::from_value(a.clone()).ok()?,
        };
        let rate = match v.get("rate") {
            None => None,
            Some(r) => Some(r.as_str()?.parse().ok()?),
        };
        let oauth = match v.get("oauth") {
            None => None,
            Some(o) => Some(serde_json::from_value(o.clone()).ok()?),
        };
        Some(Self {
            hash: v["hash"].as_str()?.to_owned(),
            capabilities: Capabilities {
                http: list("http")?,
                secrets: list("secrets")?,
                auth,
                rate,
                oauth,
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
    let mut loaded = load_fingerprinted(host, path)?;
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
    loaded.vm.grandfathered = is_grandfathered(&host.store, name, &loaded.fingerprint.hash);
    Ok(loaded.vm)
}

/// Remember what the user approved for `name`. Activating again also ends
/// its grandfathering: from now on, tools without an `effect` are `write`.
pub fn record_approval(host: &Host, name: &str, fp: &Fingerprint) -> Result<(), String> {
    host.store
        .kv_set(APPROVALS_NS, name, &fp.to_json())
        .and_then(|()| host.store.kv_delete(GRANDFATHERED_NS, name))
        .map_err(|e| e.to_string())
}

/// Whether connector `name` with source `hash` keeps M4's default.
pub fn is_grandfathered(store: &tern_store::Store, name: &str, hash: &str) -> bool {
    store
        .kv_get(GRANDFATHERED_NS, name)
        .ok()
        .flatten()
        .is_some_and(|h| h == hash)
}

/// T0 changed the default for a tool without `effect` from "no approval" to
/// `write`. Connectors that were already active keep the old default: the
/// first time this runs against a data directory it records each active
/// connector's source hash. Editing the file or activating it again ends
/// that. Later runs do nothing, so a connector added afterwards gets the
/// new default.
pub fn grandfather_existing(host: &Host, dir: &Path) {
    if host
        .store
        .kv_get(META_NS, SNAPSHOT_KEY)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    for path in luau_files(dir) {
        if let Ok(l) = load_fingerprinted(host, &path) {
            let _ = host
                .store
                .kv_set(GRANDFATHERED_NS, &l.vm.spec.name, &l.fingerprint.hash);
        }
    }
    let _ = host.store.kv_set(META_NS, SNAPSHOT_KEY, "1");
}

fn luau_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == EXTENSION))
        .collect();
    files.sort();
    files
}

/// A Luau connector tool exposed to the agent as `lua__<connector>__<tool>`.
pub struct LuaTool {
    vm: Arc<Vm>,
    def: ToolDef,
}

pub fn tool_name(connector: &str, tool: &str) -> String {
    format!("lua__{connector}__{tool}")
}

impl LuaTool {
    /// This connector's accounts, when its tools take a `connection`.
    fn routed(&self) -> Option<Vec<tern_auth::Conn>> {
        self.vm
            .host
            .connections
            .as_ref()?
            .routed(tern_auth::Kind::Connector, &self.vm.spec.name)
    }

    /// The account a call runs under, with the account parameter taken out
    /// of `args`. `Err` refuses the call before anything is sent.
    fn route(&self, args: &mut Value) -> Result<Option<tern_auth::Resolved>, String> {
        let Some(conns) = &self.vm.host.connections else {
            return Ok(None);
        };
        let asked = tern_auth::connections::take_param(&self.def.params, args);
        conns.resolve(
            tern_auth::Kind::Connector,
            &self.vm.spec.name,
            asked.as_deref(),
            self.def.effect(),
        )
    }
}

#[async_trait]
impl Tool for LuaTool {
    fn spec(&self) -> ToolSpec {
        let input_schema = match self.routed() {
            Some(conns) => tern_auth::connections::with_param(&self.def.params, &conns),
            None => self.def.params.clone(),
        };
        ToolSpec {
            name: tool_name(&self.vm.spec.name, &self.def.name),
            description: format!(
                "{} (Luau connector {})",
                self.def.description, self.vm.spec.name
            ),
            input_schema,
        }
    }

    fn effect(&self) -> Effect {
        self.def.effect()
    }

    fn untrusted(&self) -> bool {
        self.def.untrusted
    }

    fn policy(&self, _: &ToolCtx, args: &Value) -> ToolPolicy {
        // A call the host will refuse for want of an account sends nothing,
        // so nobody is asked to approve it.
        if self.route(&mut args.clone()).is_err() {
            return ToolPolicy::new(Effect::Read, true);
        }
        ToolPolicy {
            // M4: no approval unless the tool asked for it.
            granted: self.vm.grandfathered && self.def.declared_effect.is_none(),
            forbidden: self.def.forbidden,
            ..ToolPolicy::new(self.def.effect(), self.def.idempotent)
                .forced(self.def.requires_approval)
        }
    }

    /// Names the account, so a wrong-account send is visible on the
    /// approval card (§6.8.2): `lua__gmail__send as gmail · personal
    /// (mitch@gmail.com)`.
    fn summary(&self, args: &Value) -> Option<String> {
        self.routed()?;
        let r = self.route(&mut args.clone()).ok()??;
        Some(format!(
            "{} as {}",
            tool_name(&self.vm.spec.name, &self.def.name),
            r.conn.title()
        ))
    }

    async fn call(&self, _: &ToolCtx, mut args: Value) -> ToolResult {
        let conn = match self.route(&mut args) {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::error(format!("{}.{}: {e}", self.vm.spec.name, self.def.name));
            }
        };
        match self.vm.call_as(&self.def.name, &args, conn).await {
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
        grandfather_existing(&host, &dir);
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

    /// Every secret name any loaded connector uses (for redaction).
    pub fn secret_names(&self) -> Vec<String> {
        self.specs()
            .into_iter()
            .flat_map(|s| s.capabilities.all_secrets())
            .collect()
    }

    /// `(secret, what uses it)` for every secret an active connector
    /// declares, e.g. `connector notion · sent by tern to api.notion.com`.
    pub fn secret_uses(&self) -> Vec<(String, String)> {
        self.specs()
            .iter()
            .flat_map(|s| secret_users(s, false, false))
            .collect()
    }
}

/// How `spec` uses each of its secrets, for the Secrets view and the
/// `secret_request` prompt. A raw read (`ctx.secret`) is marked legacy.
fn secret_users(spec: &ConnectorSpec, draft: bool, reach: bool) -> Vec<(String, String)> {
    let caps = &spec.capabilities;
    let mut who = format!("connector {}", spec.name);
    if draft {
        who.push_str(" (draft)");
    }
    let mut uses = Vec::new();
    for a in &caps.auth {
        let hosts = if a.hosts.is_empty() {
            caps.http
                .iter()
                .map(|h| allow::entry_host(h).to_owned())
                .collect::<Vec<_>>()
        } else {
            a.hosts.clone()
        };
        let user = format!("{who} · sent by tern to {}", hosts.join(", "));
        uses.extend(
            a.secrets()
                .into_iter()
                .filter(|s| !spec::is_oauth_secret(s))
                .map(|s| (s.to_owned(), user.clone())),
        );
    }
    let mut raw = who;
    if reach && !caps.http.is_empty() {
        raw.push_str(&format!(" · reaches {}", caps.http.join(", ")));
    }
    raw.push_str(" · reads the value itself (legacy ctx.secret)");
    uses.extend(caps.secrets.iter().map(|s| (s.clone(), raw.clone())));
    uses
}

/// `(secret, what uses it)` for every secret a connector in `active` or
/// `drafts` declares, naming the hosts it can send it to, e.g.
/// `connector weather (draft) · reaches api.weather.com`. Shown when the
/// agent asks the user for a secret, so they can see where it would go.
pub fn describe_secret_uses(host: &Host, active: &Path, drafts: &Path) -> Vec<(String, String)> {
    let mut uses = Vec::new();
    for (dir, draft) in [(active, false), (drafts, true)] {
        for path in luau_files(dir) {
            let Ok(vm) = load_file(host, &path) else {
                continue;
            };
            uses.extend(secret_users(&vm.spec, draft, true));
        }
    }
    uses
}

#[async_trait]
impl ToolSource for Connectors {
    async fn prepare(&self) {
        self.reload_if_changed();
        // Accounts can change from another process (`tern connections`).
        if let Some(c) = &self.host.connections {
            c.reload();
        }
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
                let mut line = format!(
                    "Luau connector {}: {} (tools lua__{}__*)",
                    s.name, s.description, s.name
                );
                if let Some(accounts) = self
                    .host
                    .connections
                    .as_ref()
                    .and_then(|c| c.describe(tern_auth::Kind::Connector, &s.name))
                {
                    line.push_str(&format!("; accounts: {accounts}"));
                }
                line
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
