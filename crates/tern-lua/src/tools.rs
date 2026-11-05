//! Agent-authored connectors (spec §10.4): `connector_write` drafts and
//! validates, `connector_test` runs a draft's tool, `connector_activate`
//! promotes a draft after the user approves its capabilities.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_core::tool::{Effect, GrantKey, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;

use crate::{
    EXTENSION, Fingerprint, Host, Loaded, Vm, load_fingerprinted, record_approval, tool_name,
    valid_ident,
};

#[derive(Clone)]
pub struct Dirs {
    pub active: PathBuf,
    pub drafts: PathBuf,
}

impl Dirs {
    pub fn draft_path(&self, name: &str) -> PathBuf {
        self.draft(name)
    }
    fn draft(&self, name: &str) -> PathBuf {
        self.drafts.join(format!("{name}.{EXTENSION}"))
    }
    fn active(&self, name: &str) -> PathBuf {
        self.active.join(format!("{name}.{EXTENSION}"))
    }
    /// A draft's recorded fixtures, beside it.
    pub fn draft_fixtures(&self, name: &str) -> PathBuf {
        crate::fixtures::path_for(&self.draft(name))
    }
    /// An active connector's fixtures, beside it.
    pub fn active_fixtures(&self, name: &str) -> PathBuf {
        crate::fixtures::path_for(&self.active(name))
    }
}

fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolResult> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolResult::error(format!("invalid arguments: `{key}` must be a string")))
}

fn name_arg(args: &Value) -> Result<&str, ToolResult> {
    let name = arg(args, "name")?;
    if !valid_ident(name) {
        return Err(ToolResult::error(format!(
            "name {name:?} must match [a-z][a-z0-9_]* (max 32 chars)"
        )));
    }
    Ok(name)
}

fn load_draft_full(host: &Host, dirs: &Dirs, name: &str) -> Result<Loaded, String> {
    let path = dirs.draft(name);
    if !path.exists() {
        return Err(format!(
            "no draft named {name:?}; write one with connector_write"
        ));
    }
    load_fingerprinted(host, &path).map_err(|e| e.to_string())
}

fn load_draft(host: &Host, dirs: &Dirs, name: &str) -> Result<Vm, String> {
    load_draft_full(host, dirs, name).map(|l| l.vm)
}

/// What a tool's policy and summary saw, by draft name. `call` runs only if
/// the draft it loads has the same source and capabilities, so the user
/// approves exactly what runs (a draft can't change between the check and
/// the call, or compute different capabilities on each load).
#[derive(Default)]
pub struct Inspected {
    seen: Mutex<HashMap<String, Fingerprint>>,
    /// What each test call's approval was decided on, by (run, call id):
    /// every call has its own plan and `call` consumes exactly its own, so
    /// two concurrent identical calls can't share (or steal) one.
    plans: Mutex<HashMap<String, Plan>>,
}

/// What `policy` judged for one test call, which `grant_key` describes and
/// `call` must run exactly: replay (offline) or live, on this source, under
/// this account.
#[derive(Clone)]
struct Plan {
    replay: bool,
    source: String,
    key: Option<GrantKey>,
    /// The account it resolved to: `Ok(None)` the implicit default,
    /// `Ok(Some(connection id))`, or `Err` when it couldn't be resolved.
    account: Result<Option<String>, ()>,
}

/// The id a call's plan is kept under: while the registry asks the tool for
/// a policy or grant key ([`tern_core::tool::planning_scope`]), or, when
/// the call runs, its own run and call id.
fn plan_id(running: Option<&ToolCtx>) -> String {
    // Outside the registry (no call id, no planning scope: tests, direct
    // use) everything shares the one empty id, which is one call at a time.
    let (run, call) = match running {
        Some(ctx) => tern_core::tool::current_call_id()
            .map(|c| (ctx.run_id.clone(), c))
            .unwrap_or_default(),
        None => tern_core::tool::planning_scope().unwrap_or_default(),
    };
    format!("{run}\0{call}")
}

impl Inspected {
    pub fn new() -> Self {
        Self::default()
    }

    fn inspect(&self, host: &Host, dirs: &Dirs, name: &str) -> Result<Loaded, String> {
        let loaded = load_draft_full(host, dirs, name)?;
        if let Ok(mut m) = self.seen.lock() {
            m.insert(name.to_owned(), loaded.fingerprint.clone());
        }
        Ok(loaded)
    }

    fn set_plan(&self, id: String, plan: Plan) {
        if let Ok(mut m) = self.plans.lock() {
            m.insert(id, plan);
        }
    }

    fn plan(&self, id: &str) -> Option<Plan> {
        self.plans.lock().ok().and_then(|m| m.get(id).cloned())
    }

    /// Drop one call's plan, or every plan of a run (`None`).
    fn forget(&self, run_id: &str, call_id: Option<&str>) {
        if let Ok(mut m) = self.plans.lock() {
            match call_id {
                Some(c) => {
                    m.remove(&format!("{run_id}\0{c}"));
                }
                None => m.retain(|k, _| !k.starts_with(&format!("{run_id}\0"))),
            }
        }
    }

    /// How many plans are held (tests).
    #[cfg(test)]
    pub(crate) fn plans_held(&self) -> usize {
        self.plans.lock().map_or(0, |m| m.len())
    }

    /// The call's own plan, which it uses up.
    fn take_plan(&self, id: &str) -> Option<Plan> {
        self.plans.lock().ok().and_then(|mut m| m.remove(id))
    }

    /// Load for running: refuse unless it matches what was inspected.
    fn verify(&self, host: &Host, dirs: &Dirs, name: &str) -> Result<Loaded, String> {
        let loaded = load_draft_full(host, dirs, name)?;
        let seen = self.seen.lock().ok().and_then(|m| m.get(name).cloned());
        match seen {
            Some(fp) if fp == loaded.fingerprint => Ok(loaded),
            Some(_) => Err(format!(
                "draft {name} changed since it was checked (its source or declared capabilities differ); nothing was run"
            )),
            None => Err(format!(
                "draft {name} wasn't checked before running; nothing was run"
            )),
        }
    }
}

fn describe(vm: &Vm) -> String {
    let s = &vm.spec;
    let mut out = format!(
        "{} — {}\n{}",
        s.name,
        s.description,
        vm.capability_summary()
    );
    for t in &s.tools {
        out.push_str(&format!(
            "\n- {} [{}]: {}",
            tool_name(&s.name, &t.name),
            t.effect(),
            t.description
        ));
    }
    out
}

/// Writing a draft only touches the drafts directory: a granted write.
const NOT_IDEMPOTENT: ToolPolicy = ToolPolicy::granted_write(false);

// ---- connector_write ----

pub struct ConnectorWrite {
    pub host: Host,
    pub dirs: Dirs,
}

#[async_trait]
impl Tool for ConnectorWrite {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "connector_write".into(),
            description: CONNECTOR_DOCS.into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "lowercase identifier; must equal the table's `name`" },
                    "source": { "type": "string", "description": "complete Luau source of the connector file" }
                },
                "required": ["name", "source"]
            }),
        }
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        NOT_IDEMPOTENT
    }

    async fn call(&self, _: &ToolCtx, args: Value) -> ToolResult {
        let (name, source) = match (name_arg(&args), arg(&args, "source")) {
            (Ok(n), Ok(s)) => (n, s),
            (Err(e), _) | (_, Err(e)) => return e,
        };
        if let Err(e) = std::fs::create_dir_all(&self.dirs.drafts) {
            return ToolResult::error(format!("creating drafts directory: {e}"));
        }
        if let Err(e) = std::fs::write(self.dirs.draft(name), source) {
            return ToolResult::error(format!("writing draft: {e}"));
        }
        match load_draft(&self.host, &self.dirs, name) {
            Ok(vm) => ToolResult::ok(format!(
                "draft saved and valid:\n{}\n\nTry it with connector_test, then ask to activate it with connector_activate.",
                describe(&vm)
            )),
            Err(e) => ToolResult::error(format!("draft saved but invalid: {e}")),
        }
    }
}

// ---- connector_test ----

pub struct ConnectorTest {
    pub host: Host,
    pub dirs: Dirs,
    pub inspected: Inspected,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Live,
    Replay,
}

fn mode_arg(args: &Value) -> Result<Mode, String> {
    match args.get("mode").and_then(Value::as_str) {
        None | Some("auto") => Ok(Mode::Auto),
        Some("live") => Ok(Mode::Live),
        Some("replay") => Ok(Mode::Replay),
        Some(other) => Err(format!(
            "mode {other:?} must be \"auto\", \"live\" or \"replay\""
        )),
    }
}

fn test_args(args: &Value) -> Value {
    args.get("args").cloned().unwrap_or_else(|| json!({}))
}

impl ConnectorTest {
    /// The grant key for `tool` of a loaded draft, judged `replay` (see
    /// [`Tool::grant_key`]). `None` if the account can't be resolved.
    fn build_key(
        &self,
        loaded: &Loaded,
        name: &str,
        tool: &str,
        def: &crate::spec::ToolDef,
        replay: bool,
        account: &Result<Option<String>, ()>,
    ) -> Option<GrantKey> {
        let c = &loaded.fingerprint.capabilities;
        // The account the call would run under, by identity: connecting
        // another account or changing the default changes the key.
        let account = match account {
            Ok(Some(id)) => json!(id),
            Ok(None) => json!("default"),
            Err(()) => return None,
        };
        let material = json!({
            "source": loaded.fingerprint.hash,
            "http": c.http,
            "secrets": c.secrets,
            "auth": c.auth.iter().map(|a| a.describe()).collect::<Vec<_>>(),
            "rate": c.rate.map(|r| r.to_string()),
            "effect": def.effect().as_str(),
            "replay": replay,
            "account": account,
        });
        let digest = blake3::hash(material.to_string().as_bytes()).to_hex();
        Some(GrantKey {
            key: format!("draft {name}.{tool}#{}", &digest[..24]),
            label: format!("draft {name}.{tool}"),
        })
    }

    /// The account a call would run under: its connection id (which is
    /// unique to that account, so a re-added or different one differs).
    fn resolve_account(&self, name: &str, args: &Value) -> Result<Option<String>, ()> {
        match &self.host.connections {
            Some(conns) => conns
                .resolve(
                    tern_auth::Kind::Connector,
                    name,
                    args.get("connection").and_then(Value::as_str),
                    Effect::Read,
                )
                .map(|r| r.map(|r| r.conn.id))
                .map_err(|_| ()),
            None => Ok(None),
        }
    }

    /// Whether this test call replays fixtures rather than calling the API.
    fn replays(&self, name: &str, tool: &str, args: &Value) -> bool {
        match mode_arg(args) {
            Ok(Mode::Replay) => true,
            Ok(Mode::Live) | Err(_) => false,
            Ok(Mode::Auto) => crate::fixtures::Fixtures::load(&self.dirs.draft_fixtures(name))
                .ok()
                .flatten()
                .is_some_and(|f| f.find(tool, &test_args(args)).is_some()),
        }
    }
}

#[async_trait]
impl Tool for ConnectorTest {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "connector_test".into(),
            description:
                "Run one tool of a draft connector with the given arguments and return its result. A live call is recorded as a fixture (sanitized: no credentials) beside the draft; testing the same tool with the same arguments again replays it offline, with no network and no approval, so re-test freely after editing. With several accounts connected, `connection` picks one (else the default)."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "tool": { "type": "string" },
                    "args": { "type": "object" },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "live", "replay"],
                        "description": "auto (default): replay the recorded call with these exact arguments if there is one, else call the API live and record it. live: call the API and record again. replay: offline only."
                    },
                    "connection": { "type": "string", "description": "The account to test under" }
                },
                "required": ["name", "tool"]
            }),
        }
    }

    /// A draft that can reach the network or read secrets is run only with
    /// approval: it is code the model just wrote. Testing a tool has that
    /// tool's effect, so a destructive one asks every time and a
    /// `money_movement` one is refused. Replaying fixtures sends nothing,
    /// so it needs no approval (unless the script reads raw secrets).
    fn policy(&self, _: &ToolCtx, args: &Value) -> ToolPolicy {
        let name = name_arg(args).ok();
        let loaded = name.and_then(|n| self.inspected.inspect(&self.host, &self.dirs, n).ok());
        let tool = args.get("tool").and_then(Value::as_str);
        let replay = match (name, tool) {
            (Some(n), Some(t)) => self.replays(n, t, args),
            _ => false,
        };
        let risky = loaded.as_ref().is_some_and(|l| {
            let c = &l.fingerprint.capabilities;
            !c.secrets.is_empty() || (!replay && (!c.http.is_empty() || !c.auth.is_empty()))
        });
        let def = loaded.as_ref().and_then(|l| {
            l.vm.spec
                .tools
                .iter()
                .find(|t| Some(t.name.as_str()) == tool)
        });
        let effect = match def {
            Some(_) if replay && !risky => Effect::Read,
            Some(d) if risky => d.effect(),
            _ => Effect::Write,
        };
        // The grant key is built here, from the same snapshot (source,
        // capabilities, replay) the policy and the call use.
        // The plan is this call's own (by run and call id).
        if let (Some(n), Some(t), Some(l)) = (name, tool, loaded.as_ref()) {
            let account = self.resolve_account(n, args);
            let key = def.and_then(|d| self.build_key(l, n, t, d, replay, &account));
            self.inspected.set_plan(
                plan_id(None),
                Plan {
                    replay,
                    source: l.fingerprint.hash.clone(),
                    key,
                    account,
                },
            );
        }
        ToolPolicy {
            granted: !risky,
            forbidden: def.and_then(|d| d.forbidden),
            ..ToolPolicy::new(effect, false).forced(risky)
        }
    }

    fn allow_always(&self) -> bool {
        false
    }

    fn forget_plans(&self, run_id: &str, call_id: Option<&str>) {
        self.inspected.forget(run_id, call_id);
    }

    /// What a scoped grant covers: this one tool of this draft, at exactly
    /// this source and these declared capabilities (hosts, secrets, auth,
    /// rate), with this effect, offline or live, under this account (the
    /// resolved account's id, slug and address, so a different account or a
    /// changed default asks again). Edit the draft, test another tool, or
    /// reach a new host and the key changes, so the user is asked again.
    /// Arguments are not part of it: they can only reach what the
    /// capabilities allow. It is the key `policy` planned for this call,
    /// so it can't disagree with the replay-or-live decision `call` runs.
    fn grant_key(&self, _args: &Value) -> Option<GrantKey> {
        // Only the plan `policy` made for this very call; with none there is
        // no key (no grant offered or matched) and `call` refuses to go live.
        self.inspected.plan(&plan_id(None)).and_then(|p| p.key)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let name = name_arg(args).ok()?;
        let tool = args.get("tool").and_then(Value::as_str).unwrap_or("?");
        let caps = self
            .inspected
            .inspect(&self.host, &self.dirs, name)
            .map(|l| l.vm.capability_summary())
            .unwrap_or_else(|e| format!("invalid draft: {e}"));
        Some(format!("connector_test draft {name}.{tool} ({caps})"))
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let (name, tool) = match (name_arg(&args), arg(&args, "tool")) {
            (Ok(n), Ok(t)) => (n, t),
            (Err(e), _) | (_, Err(e)) => return e,
        };
        let mode = match mode_arg(&args) {
            Ok(m) => m,
            Err(e) => return ToolResult::error(e),
        };
        let (vm, loaded_hash) = match self.inspected.verify(&self.host, &self.dirs, name) {
            Ok(l) => (l.vm, l.fingerprint.hash),
            Err(e) => return ToolResult::error(e),
        };
        let tool_args = test_args(&args);
        // This call's own plan, used up now. Without one nothing decided
        // whether it is offline or live, so only an explicit offline replay
        // may run: never a live call nobody was asked about.
        let plan = self.inspected.take_plan(&plan_id(Some(ctx)));
        let replay = match &plan {
            Some(p) => {
                // The draft must be the one the policy and key were judged on.
                if p.source != loaded_hash {
                    return ToolResult::error(format!(
                        "draft {name} changed since its approval was decided; nothing was run"
                    ));
                }
                p.replay
            }
            None if mode == Mode::Replay => true,
            None => {
                return ToolResult::error(format!(
                    "no approval was decided for this test of {name}.{tool}; nothing was run"
                ));
            }
        };
        let fixtures_path = self.dirs.draft_fixtures(name);
        let fixtures = match crate::fixtures::Fixtures::load(&fixtures_path) {
            Ok(f) => f,
            Err(e) => return ToolResult::error(e),
        };
        if replay {
            let Some(case) = fixtures.as_ref().and_then(|f| f.find(tool, &tool_args)) else {
                return ToolResult::error(format!(
                    "no recorded call of {name}.{tool} with these arguments{}; test it live (mode \"live\") to record one",
                    if mode == Mode::Auto { " any more" } else { "" }
                ));
            };
            return match crate::fixtures::check_case(&vm, case).await {
                crate::fixtures::Outcome::Pass => ToolResult::ok(format!(
                    "[replayed offline from the recorded fixture; mode \"live\" calls the API again]\n{}",
                    case.result
                )),
                crate::fixtures::Outcome::Changed { got } => ToolResult::ok(format!(
                    "[replayed offline from the recorded fixture; the result differs from the recorded one, which was:\n{}]\n{got}",
                    clip(&case.result, 1500)
                )),
                crate::fixtures::Outcome::Failed(e) => {
                    ToolResult::error(format!("{name}.{tool} (replaying fixtures): {e}"))
                }
            };
        }
        // A test only reads what its account allows; it runs under the
        // account asked for, else the default.
        let conn = match &self.host.connections {
            Some(c) => match c.resolve(
                tern_auth::Kind::Connector,
                name,
                args.get("connection").and_then(Value::as_str),
                Effect::Read,
            ) {
                Ok(r) => r,
                Err(e) => return ToolResult::error(e),
            },
            None => None,
        };
        // It must be the account the approval (and grant key) named: if the
        // default changed in between, ask again instead.
        let account = conn.as_ref().map(|r| r.conn.id.clone());
        if plan.is_some_and(|p| p.account != Ok(account)) {
            return ToolResult::error(format!(
                "the account for {name} changed since its approval was decided; nothing was run"
            ));
        }
        if !self.host.record_fixtures {
            return match vm.call_as(tool, &tool_args, conn).await {
                Ok(out) => ToolResult::ok(out),
                Err(e) => ToolResult::error(format!("{name}.{tool}: {e}")),
            };
        }
        match crate::fixtures::record_case(&vm, tool, &tool_args, conn).await {
            // Nothing went over the network: nothing worth replaying.
            Ok((out, case)) if case.exchanges.is_empty() => ToolResult::ok(out),
            Ok((out, case)) => {
                let mut f = fixtures.unwrap_or_else(|| crate::fixtures::Fixtures::new(name));
                let n = case.exchanges.len();
                f.upsert(case);
                let note = match f.save(&fixtures_path) {
                    Ok(()) => format!(
                        "[recorded {n} HTTP exchange{} as a fixture; testing these arguments again replays offline]",
                        if n == 1 { "" } else { "s" }
                    ),
                    Err(e) => format!("[not recorded: {e}]"),
                };
                ToolResult::ok(format!("{out}\n\n{note}"))
            }
            Err(e) => ToolResult::error(format!("{name}.{tool}: {e}")),
        }
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// `. Changes: + http api.x.com/v2/*, - rate 60/min`, or that nothing changed.
fn capability_diff(old: &[String], new: &[String]) -> String {
    let added = new
        .iter()
        .filter(|l| !old.contains(l))
        .map(|l| format!("+ {l}"));
    let removed = old
        .iter()
        .filter(|l| !new.contains(l))
        .map(|l| format!("- {l}"));
    let changes: Vec<String> = added.chain(removed).collect();
    if changes.is_empty() {
        ". Capabilities unchanged from the active one".into()
    } else {
        format!(". Changes from the active one: {}", changes.join(", "))
    }
}

// ---- connector_activate ----

pub struct ConnectorActivate {
    pub host: Host,
    pub dirs: Dirs,
    pub inspected: Inspected,
}

#[async_trait]
impl Tool for ConnectorActivate {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "connector_activate".into(),
            description: "Promote a tested draft connector so its tools are available in future messages. The user must approve its capabilities.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        }
    }

    fn policy(&self, _: &ToolCtx, args: &Value) -> ToolPolicy {
        if let Ok(name) = name_arg(args) {
            let _ = self.inspected.inspect(&self.host, &self.dirs, name);
        }
        ToolPolicy::new(Effect::Write, false).forced(true)
    }

    /// Each activation is a fresh decision: never "always allow".
    fn allow_always(&self) -> bool {
        false
    }

    /// The approval prompt shows exactly what the connector may access
    /// and, when it replaces an active one, what changes.
    fn summary(&self, args: &Value) -> Option<String> {
        let name = name_arg(args).ok()?;
        let active = self.dirs.active(name);
        let replacing = if active.exists() {
            " (replacing the active one)"
        } else {
            ""
        };
        Some(match self.inspected.inspect(&self.host, &self.dirs, name) {
            Ok(l) => {
                let mut out = format!(
                    "activate Luau connector {name}{replacing} — {}",
                    l.vm.capability_summary()
                );
                if let Ok(old) = crate::load_file(&self.host, &active) {
                    let rate = self.host.default_rate;
                    out.push_str(&capability_diff(
                        &old.spec.capabilities.lines(rate),
                        &l.vm.spec.capabilities.lines(rate),
                    ));
                }
                out
            }
            Err(e) => format!("activate Luau connector {name} — invalid draft: {e}"),
        })
    }

    async fn call(&self, _: &ToolCtx, args: Value) -> ToolResult {
        let name = match name_arg(&args) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let Loaded {
            vm,
            fingerprint,
            source,
        } = match self.inspected.verify(&self.host, &self.dirs, name) {
            Ok(l) => l,
            Err(e) => return ToolResult::error(e),
        };
        if let Err(e) = std::fs::create_dir_all(&self.dirs.active) {
            return ToolResult::error(format!("creating connectors directory: {e}"));
        }
        // Write the exact bytes that were approved, then record them, so the
        // loader refuses the connector if it changes later.
        if let Err(e) = std::fs::write(self.dirs.active(name), &source) {
            return ToolResult::error(format!("activating: {e}"));
        }
        if let Err(e) = record_approval(&self.host, name, &fingerprint) {
            let _ = std::fs::remove_file(self.dirs.active(name));
            return ToolResult::error(format!("recording approval: {e}"));
        }
        let _ = std::fs::remove_file(self.dirs.draft(name));
        // Fixtures go with the connector, so its tests replay where it lives.
        let fixtures = self.dirs.draft_fixtures(name);
        if fixtures.exists() {
            let _ = std::fs::rename(&fixtures, self.dirs.active_fixtures(name));
        }
        let tools: Vec<String> = vm
            .spec
            .tools
            .iter()
            .map(|t| tool_name(name, &t.name))
            .collect();
        ToolResult::ok(format!(
            "activated {name}; from the next message its tools are available: {}",
            tools.join(", ")
        ))
    }
}

// ---- connector_from_openapi ----

/// Operations generated when none are named.
pub const DEFAULT_OPENAPI_TOOLS: usize = 8;
/// Most operations one generated connector takes.
pub const MAX_OPENAPI_TOOLS: usize = 40;
/// Operations per page of a listing.
const LISTING_PAGE: usize = 50;
/// How long a fetched document is kept for the next call.
const DOCUMENT_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// Write the draft for these operations and validate it. Shared by the
/// tool and `tern connectors from-openapi`.
pub fn draft_from_openapi(
    host: &Host,
    dirs: &Dirs,
    doc: &crate::openapi::Document,
    name: &str,
    chosen: &[usize],
    source: &str,
) -> Result<(Vm, crate::openapi::Generated), String> {
    let generated = crate::openapi::generate(doc, name, chosen, source)?;
    std::fs::create_dir_all(&dirs.drafts).map_err(|e| format!("creating drafts directory: {e}"))?;
    std::fs::write(dirs.draft(name), &generated.source)
        .map_err(|e| format!("writing draft: {e}"))?;
    let vm = load_draft(host, dirs, name)
        .map_err(|e| format!("the generated draft is invalid (a bug in the generator; edit it with connector_write): {e}"))?;
    Ok((vm, generated))
}

/// What to tell the agent (or print) after generating: the draft, what it
/// left out, and the next steps.
pub fn openapi_report(
    vm: &Vm,
    doc: &crate::openapi::Document,
    chosen: &[usize],
    generated: &crate::openapi::Generated,
) -> String {
    let mut out = format!("draft saved and valid:\n{}\n", describe(vm));
    for n in &generated.notes {
        out.push_str(&format!("\n{n}"));
    }
    if let Some(plan) = &generated.oauth_plan {
        out.push_str(&format!(
            "\nPlan for auth_propose (only if auth_discover finds nothing): {plan}"
        ));
    }
    let left = doc.operations.len() - chosen.len();
    if left > 0 {
        out.push_str(&format!(
            "\n\n{} of {} operations generated; {left} left out. To change the set, call connector_from_openapi again with `operations` (names from list: true, \"GET /path\", or \"tag:<tag>\").",
            chosen.len(),
            doc.operations.len()
        ));
    }
    out.push_str("\n\nNext: test each tool with connector_test (the first live call is recorded as a fixture; later tests replay it offline), then connector_activate: the user approves its capabilities. It is not active until then.");
    out
}

type Cached = (
    String,
    std::time::Instant,
    std::sync::Arc<crate::openapi::Document>,
);

pub struct ConnectorFromOpenapi {
    pub host: Host,
    pub dirs: Dirs,
    /// Operations generated when none are named (`[connectors] openapi_tools`).
    pub default_tools: usize,
    /// Most operations a connector takes (`[connectors] openapi_max_tools`).
    pub max_tools: usize,
    allow_loopback: bool,
    cache: Mutex<Option<Cached>>,
}

impl ConnectorFromOpenapi {
    pub fn new(host: Host, dirs: Dirs) -> Self {
        Self {
            host,
            dirs,
            default_tools: DEFAULT_OPENAPI_TOOLS,
            max_tools: MAX_OPENAPI_TOOLS,
            allow_loopback: false,
            cache: Mutex::new(None),
        }
    }

    /// Tests fetch documents from a local server.
    #[cfg(test)]
    pub fn allowing_loopback(mut self) -> Self {
        self.allow_loopback = true;
        self
    }

    async fn document(
        &self,
        url: &str,
        base_url: Option<&str>,
    ) -> Result<std::sync::Arc<crate::openapi::Document>, String> {
        let key = format!("{url}\0{}", base_url.unwrap_or_default());
        if let Ok(c) = self.cache.lock()
            && let Some((k, at, doc)) = c.as_ref()
            && *k == key
            && at.elapsed() < DOCUMENT_TTL
        {
            return Ok(doc.clone());
        }
        let fetched = tern_core::net::get_public(
            url,
            crate::openapi::MAX_DOCUMENT,
            self.allow_loopback,
            std::time::Duration::from_secs(60),
        )
        .await
        .map_err(|e| format!("fetching {url}: {e}"))?;
        if !fetched.status.is_success() {
            return Err(format!("fetching {url}: HTTP {}", fetched.status));
        }
        if fetched.truncated {
            return Err(format!(
                "{url} is larger than {} bytes",
                crate::openapi::MAX_DOCUMENT
            ));
        }
        let text = String::from_utf8_lossy(&fetched.body);
        let doc = std::sync::Arc::new(crate::openapi::parse(&text, Some(url), base_url)?);
        if let Ok(mut c) = self.cache.lock() {
            *c = Some((key, std::time::Instant::now(), doc.clone()));
        }
        Ok(doc)
    }
}

fn str_list(args: &Value, key: &str) -> Option<Vec<String>> {
    args.get(key)?.as_array().map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

#[async_trait]
impl Tool for ConnectorFromOpenapi {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "connector_from_openapi".into(),
            description: "Generate a draft Luau connector from a service's OpenAPI 3 or Swagger 2 document (JSON or YAML): one tool per chosen operation, with typed params, an effect from the method (GET reads, DELETE is destructive, the rest write), exact host and path allowlists, auth from its security schemes, and a rate if it states one. Prefer this to writing a connector by hand whenever the service publishes such a document. With no `operations`, a small API gets its first few read operations and a large one returns the operation list to choose from (`list: true` pages through it, narrowed by `tag` or `search`). The draft is tested and activated like any other: connector_test, then connector_activate with the user's approval.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The OpenAPI or Swagger document's URL (https)" },
                    "name": { "type": "string", "description": "Connector name, [a-z][a-z0-9_]*; defaults to one from the document's title" },
                    "operations": {
                        "type": "array", "items": { "type": "string" },
                        "description": "Which operations: tool names from the listing, operationIds, \"GET /path\", or \"tag:<tag>\" for a whole tag"
                    },
                    "list": { "type": "boolean", "description": "Only list operations (paged); write nothing" },
                    "tag": { "type": "string", "description": "With list: only this tag" },
                    "search": { "type": "string", "description": "With list: only operations whose name, path or summary contains this" },
                    "offset": { "type": "integer", "minimum": 0, "description": "With list: where the page starts" },
                    "base_url": { "type": "string", "description": "Where requests go, when the document's servers are missing, relative or wrong" }
                },
                "required": ["url"]
            }),
        }
    }

    fn egress_host(&self, args: &Value) -> Option<String> {
        let url = reqwest::Url::parse(args.get("url")?.as_str()?).ok()?;
        url.host_str().map(str::to_ascii_lowercase)
    }

    /// Fetching a public document and writing a draft: nothing runs, and
    /// activation is its own approval.
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        NOT_IDEMPOTENT
    }

    async fn call(&self, _: &ToolCtx, args: Value) -> ToolResult {
        let url = match arg(&args, "url") {
            Ok(u) => u,
            Err(e) => return e,
        };
        let base_url = args.get("base_url").and_then(Value::as_str);
        let doc = match self.document(url, base_url).await {
            Ok(d) => d,
            Err(e) => return ToolResult::error(e),
        };
        let filter = crate::openapi::Filter {
            tag: args.get("tag").and_then(Value::as_str).map(str::to_owned),
            search: args
                .get("search")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        if args.get("list").and_then(Value::as_bool).unwrap_or(false) {
            return ToolResult::ok(crate::openapi::listing(&doc, &filter, offset, LISTING_PAGE));
        }
        let asked = str_list(&args, "operations").filter(|v| !v.is_empty());
        if asked.is_none() && doc.operations.len() > self.max_tools {
            return ToolResult::ok(format!(
                "{}\nThis API is too large to generate whole. Nothing was written: call again with `operations` (up to {}), for example the reads you need or \"tag:<tag>\".",
                crate::openapi::listing(&doc, &filter, offset, LISTING_PAGE),
                self.max_tools
            ));
        }
        let chosen = match crate::openapi::select(
            &doc,
            asked.as_deref(),
            self.default_tools,
            self.max_tools,
        ) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(e),
        };
        let name = match args.get("name").and_then(Value::as_str) {
            Some(_) => match name_arg(&args) {
                Ok(n) => n.to_owned(),
                Err(e) => return e,
            },
            None => doc.suggested_name(),
        };
        match draft_from_openapi(&self.host, &self.dirs, &doc, &name, &chosen, url) {
            Ok((vm, generated)) => ToolResult::ok(openapi_report(&vm, &doc, &chosen, &generated)),
            Err(e) => ToolResult::error(e),
        }
    }
}

const CONNECTOR_DOCS: &str = r#"Write (or overwrite) a draft Luau connector in ~/.tern/connectors/drafts/ and validate it. If the service publishes an OpenAPI or Swagger document, use connector_from_openapi instead: it generates the connector (tools, effects, allowlists, auth) from the document, and you edit the result here only if needed. A connector file returns a table:

return {
  name = "weather",                       -- must equal the `name` argument
  description = "Look up forecasts",
  capabilities = {
    http = { "api.weather.example/v1/*" },  -- hosts ctx.http may call, optionally limited to paths
                                          -- ("*.example.com" allowed; "*" in a path is one segment,
                                          -- a trailing "*" is the rest)
    auth = { secret = "WEATHER_API_KEY" },  -- tern adds "Authorization: Bearer <value>" to every
                                          -- request to those hosts; the script never sees it
    rate = "60/min",                      -- optional; requests per minute, hour or sec
  },
  tools = {
    forecast = {
      description = "Forecast for a city",
      effect = "read",                    -- "read" (never asks), "write" (default; asks unless
                                          -- always-allowed) or "destructive" (asks every time)
      untrusted = true,                   -- returns third-party text (mail, web pages, messages)
      idempotent = true,                  -- safe to re-run after a crash (default false)
      params = { type = "object", properties = { city = { type = "string" } }, required = { "city" } },
      run = function(ctx, args)
        local res = ctx.http.get("https://api.weather.example/v1/forecast?q=" .. args.city)
        if not res.ok then error("HTTP " .. res.status) end
        return res.json                   -- tables are returned as JSON, strings as-is
      end,
    },
  },
}

Signing in comes first: before writing a connector for a service, call auth_discover (with the API's base URL when you know it). If it signs in with OAuth, call auth_propose with the plan_id so the user chooses; if discovery says "unknown", look in the service's developer docs for OAuth and pass what you find to auth_propose as a researched plan (tern checks the addresses), else use a key. Never pick for the user when both are offered.

OAuth (when the user chose signing in): declare the provider from the plan and the scopes the tools need, and tern sends the account's token, refreshing it as needed; the script never sees it:
  capabilities = { http = { "gmail.googleapis.com" }, oauth = { google = { scopes = { "https://www.googleapis.com/auth/gmail.readonly" } } } }
Then call connection_add (with the plan_id) so the user signs in, and connector_test. ctx.connection is { slug, label, account }: the account the call runs under. When the user has several accounts (work and personal Gmail), tern adds a `connection` argument to every tool and routes each call; the script doesn't change.

Keys (when there is no OAuth, or the user chose a key) go in capabilities.auth, never in the script. Forms:
  auth = "TOKEN"                                               -- Authorization: Bearer <TOKEN>
  auth = { secret = "KEY", header = "X-Api-Key" }              -- X-Api-Key: <KEY> (scheme "" unless the header is Authorization)
  auth = { secret = "TOKEN", header = "Authorization", scheme = "token" }
  auth = { secret = "KEY", query = "api_key" }                 -- ?api_key=<KEY>
  auth = { basic = { user = "JIRA_EMAIL", password = "JIRA_TOKEN" } }   -- HTTP Basic; user or password may be left out
  auth = "oauth"                                               -- the OAuth token, placed explicitly (with capabilities.oauth)
Add hosts = { "api.example.com" } to an entry to send it to only some of the http hosts, and use a list of entries for several. A request sends every entry for its host; pass auth = false in a request's options to send none, or auth = "NAME" to send only that one. The value is removed from responses and errors before the script sees them. capabilities.secrets with ctx.secret(name) still works for old connectors but is legacy: the script sees the value, and tern flags it to the user.

A "read" tool may only send GET/HEAD; set read_via_post = true for one that reads via POST (GraphQL, search APIs). Tools that move money or trade must say money_movement = true or trade = true: tern refuses to run them. Requests outside the declared hosts and paths fail, including after redirects. Past the rate (default 120/min per connector, shared by every caller, scripts included) a request fails with "rate limited … try again in Ns"; a 429 or 503 with Retry-After pauses the connector for that long.

ctx API: ctx.http.request{method, url, headers, json, body, timeout, auth} and ctx.http.get/post/put/patch/delete(url, opts) return {status, ok, headers, body, json}; ctx.kv.get(key) / ctx.kv.set(key, value) (per-connector, per-account storage; set nil to delete); ctx.connection; ctx.log(level, msg); ctx.now() (unix ms). If a key in capabilities.auth isn't set, call secret_request so the user can enter it privately (or connection_add with method "key" for another account), before connector_test; never ask for secrets or tokens in the chat. Globals: json.encode, json.decode, string, table, math, os.time/os.date. No file or process access, no require. Each call has a 5 second limit.

Testing: connector_test's first live call of a tool with given arguments is saved as a fixture beside the draft (<name>.fixtures.json: requests and responses with credentials and secret values removed). Testing the same arguments again replays it offline, with no network or approval, so after editing the draft re-run your earlier tests to check nothing broke; pass mode "live" to call the API again. The fixtures move with the connector when it is activated, and `tern connectors test <name>` replays them all."#;
