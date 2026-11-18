//! Agents managing the team from chat: `agent_create`, `agent_edit`,
//! `agent_delete`, `channel_create`, `channel_edit`, `channel_delete`
//! (routines already have `schedule_*`).
//!
//! The host enforces every limit (`tern_core::narrow`): an agent never gives
//! another a tool, connector or autonomy ceiling it doesn't have itself, and
//! never edits or deletes one that can do more than it can. A refusal is a
//! plain error that says it is final, so a model doesn't retry another
//! spelling of the same ask. Writes ask the user; deletes always do.

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::agents::ToolScope;
use tern_core::approval::Autonomy;
use tern_core::models::Change;
use tern_core::narrow;
use tern_core::subagent::READ_ONLY_TOOLS;
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;
use tern_store::{AgentChange, AgentRow, ChannelRow};

use crate::runtime::{NewAgent, Runtime};

/// The runtime the tools act through, bound once it is shared (the tools
/// are built before it exists).
pub type AdminSlot = Arc<OnceLock<Weak<Runtime>>>;

fn spec(name: &str, description: &str, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

fn args<T: serde::de::DeserializeOwned>(raw: Value) -> Result<T, ToolResult> {
    serde_json::from_value(raw).map_err(|e| ToolResult::error(format!("invalid arguments: {e}")))
}

/// What a refused request says: it is final.
fn refused(why: impl std::fmt::Display) -> ToolResult {
    ToolResult::error(format!(
        "Refused: {why}. Don't try again with other spellings; do it another way or tell the user."
    ))
}

/// The caller: its runtime, agent, run, and what it may do.
struct Caller {
    rt: Arc<Runtime>,
    me: AgentRow,
    /// Frozen tool scope of the run (`None` = everything).
    scope: Option<Vec<String>>,
    level: Autonomy,
    known: Vec<String>,
}

impl Caller {
    fn new(slot: &AdminSlot, ctx: &ToolCtx) -> Result<Self, ToolResult> {
        let rt = slot
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| ToolResult::error("this isn't available in this process"))?;
        let store = &rt.store;
        let db = |e: tern_store::StoreError| ToolResult::error(e.to_string());
        let run = store
            .run(&ctx.run_id)
            .map_err(db)?
            .ok_or_else(|| ToolResult::error("run not found"))?;
        let id = store.session_agent(&ctx.session_id).map_err(db)?;
        let me = store
            .agent(&id)
            .map_err(db)?
            .ok_or_else(|| ToolResult::error("you aren't an agent"))?;
        let scope = run
            .tool_scope
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok());
        let level = Autonomy::of_run(run.autonomy.as_deref());
        let known = rt.agent.tools().builtin_names();
        Ok(Self {
            rt,
            me,
            scope,
            level,
            known,
        })
    }

    fn ceiling(&self) -> Option<Autonomy> {
        self.me.autonomy.as_deref().and_then(Autonomy::parse)
    }

    fn scope(&self) -> Option<&[String]> {
        self.scope.as_deref()
    }

    /// An active agent by id, slug or name.
    fn agent(&self, name: &str) -> Result<AgentRow, ToolResult> {
        let wanted = name.trim().trim_start_matches('@');
        let agents = self
            .rt
            .store
            .agents(false)
            .map_err(|e| ToolResult::error(e.to_string()))?;
        agents
            .iter()
            .find(|a| a.id == wanted || a.slug == wanted || a.name.eq_ignore_ascii_case(wanted))
            .cloned()
            .ok_or_else(|| {
                let slugs: Vec<&str> = agents.iter().map(|a| a.slug.as_str()).collect();
                refused(format!(
                    "there is no agent {name:?}; the agents are: {}",
                    slugs.join(", ")
                ))
            })
    }

    /// The frozen scope of `a`, as it would run.
    fn frozen(&self, a: &AgentRow) -> Vec<String> {
        a.tools
            .as_deref()
            .and_then(|t| ToolScope::parse(t).ok())
            .unwrap_or_default()
            .freeze(&self.known)
    }

    /// `a` may be edited or deleted by this caller: not the default agent,
    /// and nothing that can do more than the caller can.
    fn may_change(&self, a: &AgentRow) -> Result<(), ToolResult> {
        if a.is_default() {
            return Err(refused(
                "the default agent is the user's to change, in the app",
            ));
        }
        let target_ceiling = a.autonomy.as_deref().and_then(Autonomy::parse);
        narrow::within_reach(
            self.ceiling(),
            self.scope(),
            target_ceiling,
            &self.frozen(a),
        )
        .map_err(refused)
    }

    fn changed(&self) {
        let _ = self.rt.changes.send(Change::AgentsChanged {});
    }

    fn channels_changed(&self) {
        let _ = self.rt.changes.send(Change::ChannelsChanged {});
    }
}

/// `tools` as a list of names, or the preset `read_only`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Tools {
    Preset(String),
    Names(Vec<String>),
}

fn builtins_of(tools: Option<Tools>, known: &[String]) -> Result<Option<Vec<String>>, ToolResult> {
    Ok(match tools {
        None => None,
        Some(Tools::Preset(p)) if narrow::normalize_tool(&p) == "read_only" => Some(
            known
                .iter()
                .filter(|n| READ_ONLY_TOOLS.contains(&n.as_str()))
                .cloned()
                .collect(),
        ),
        Some(Tools::Preset(p)) => {
            return Err(refused(format!(
                "tools is a list of tool names or \"read_only\", not {p:?}"
            )));
        }
        Some(Tools::Names(n)) => Some(narrow::resolve_tools(&n, known).map_err(refused)?),
    })
}

fn autonomy_of(s: Option<String>) -> Result<Option<Autonomy>, ToolResult> {
    s.map(|s| {
        Autonomy::parse(
            s.trim()
                .to_ascii_lowercase()
                .replace(['-', ' '], "_")
                .as_str(),
        )
        .or_else(|| (s.trim().eq_ignore_ascii_case("read only")).then_some(Autonomy::ReadOnly))
        .ok_or_else(|| {
            refused(format!(
                "autonomy is one of readonly, supervised, code, autonomous, yolo, not {s:?}"
            ))
        })
    })
    .transpose()
}

const POWER_NOTE: &str =
    "It never gets a tool, connector or autonomy ceiling you don't have yourself.";

// ---- agent_create ----

pub struct AgentCreate(pub AdminSlot);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    name: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    tools: Option<Tools>,
    #[serde(default)]
    connectors: Option<Vec<String>>,
    #[serde(default)]
    autonomy: Option<String>,
}

#[async_trait]
impl Tool for AgentCreate {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "agent_create",
            &format!(
                "Create a new agent for the user, with its own chat and memory. Give a `name`; its starting role is written from the name unless you give `role` (a short persona). `tools` is a list of tool names (spelling is forgiving) or \"read_only\"; `connectors` names connectors; `autonomy` is its ceiling (default supervised). {POWER_NOTE} The user is asked to approve."
            ),
            json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string", "description": "e.g. 'Scout'. Its handle is @scout." },
                    "role": { "type": "string", "description": "Its persona, a few lines. Leave out to have it written from the name." },
                    "tools": { "description": "Tool names, or 'read_only'. Default: the narrow default, minus what you lack.",
                        "oneOf": [ { "enum": ["read_only"] }, { "type": "array", "items": { "type": "string" } } ] },
                    "connectors": { "type": "array", "items": { "type": "string" } },
                    "autonomy": { "enum": ["readonly", "supervised", "code", "autonomous", "yolo"] }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Write, false)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let n = args.get("name").and_then(Value::as_str).unwrap_or("?");
        Some(format!("Create agent {n}"))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: CreateArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let c = match Caller::new(&self.0, ctx) {
            Ok(c) => c,
            Err(e) => return e,
        };
        match create(&c, a).await {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => e,
        }
    }
}

async fn create(c: &Caller, a: CreateArgs) -> Result<Value, ToolResult> {
    let builtins = builtins_of(a.tools, &c.known)?;
    let scope = narrow::narrow_scope(
        c.scope(),
        builtins.as_deref(),
        a.connectors.as_deref(),
        &c.known,
    )
    .map_err(refused)?;
    let level =
        narrow::narrow_autonomy(c.level, c.ceiling(), autonomy_of(a.autonomy)?).map_err(refused)?;
    let granted = scope.freeze(&c.known);
    let role = a.role.filter(|r| !r.trim().is_empty());
    let row =
        c.rt.create_agent(NewAgent {
            name: a.name,
            persona: role.clone(),
            tools: Some(scope),
            autonomy: Some(level),
            ..NewAgent::default()
        })
        .map_err(|e| refused(format!("{e:#}")))?;
    c.changed();
    if role.is_none() && tern_core::role::load(&c.rt.home, &row.id).is_some_and(|s| s.open()) {
        let (rt, agent) = (c.rt.clone(), row.clone());
        tokio::spawn(async move {
            match rt.infer_role(&agent, false).await {
                Ok(_) => {
                    let _ = rt.changes.send(Change::AgentsChanged {});
                }
                Err(e) => tracing::warn!(agent = %agent.id, "starter role not written: {e:#}"),
            }
        });
    }
    Ok(json!({
        "created": format!("@{}", row.slug),
        "name": row.name,
        "autonomy": level.as_str(),
        "tools": granted,
        "role": if role.is_some() { "as you wrote it" } else { "being written from its name" },
    }))
}

// ---- agent_edit ----

pub struct AgentEdit(pub AdminSlot);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    agent: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    tools: Option<Tools>,
    #[serde(default)]
    connectors: Option<Vec<String>>,
    #[serde(default)]
    autonomy: Option<String>,
}

#[async_trait]
impl Tool for AgentEdit {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "agent_edit",
            &format!(
                "Change an agent's name, role, tools, connectors or autonomy ceiling. Only agents that can do no more than you can; the default agent and yourself (beyond name and role) are the user's. What you set replaces the old value. {POWER_NOTE} The user is asked to approve."
            ),
            json!({
                "type": "object",
                "required": ["agent"],
                "properties": {
                    "agent": { "type": "string", "description": "Its name or @handle." },
                    "name": { "type": "string" },
                    "role": { "type": "string", "description": "Its new persona." },
                    "tools": { "description": "Tool names, or 'read_only'.",
                        "oneOf": [ { "enum": ["read_only"] }, { "type": "array", "items": { "type": "string" } } ] },
                    "connectors": { "type": "array", "items": { "type": "string" } },
                    "autonomy": { "enum": ["readonly", "supervised", "code", "autonomous", "yolo"] }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Write, true)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let n = args.get("agent").and_then(Value::as_str).unwrap_or("?");
        Some(format!("Edit agent {n}"))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: EditArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let c = match Caller::new(&self.0, ctx) {
            Ok(c) => c,
            Err(e) => return e,
        };
        match edit(&c, a) {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => e,
        }
    }
}

fn edit(c: &Caller, a: EditArgs) -> Result<Value, ToolResult> {
    let target = c.agent(&a.agent)?;
    let me = target.id == c.me.id;
    if me && (a.tools.is_some() || a.connectors.is_some() || a.autonomy.is_some()) {
        return Err(refused(
            "you can change your own name and role, but your tools and autonomy are the user's to change",
        ));
    }
    if !me {
        c.may_change(&target)?;
    } else if target.is_default() && a.name.is_some() {
        return Err(refused("the default agent is renamed in the app"));
    }
    let name = a.name.as_deref().map(str::trim);
    if name == Some("") {
        return Err(refused("the name is empty"));
    }
    let mut change = AgentChange {
        name: name.map(str::to_owned),
        ..AgentChange::default()
    };
    // What a field replaces starts from what the agent has.
    if a.tools.is_some() || a.connectors.is_some() {
        let mut scope = target
            .tools
            .as_deref()
            .and_then(|t| ToolScope::parse(t).ok())
            .unwrap_or_default();
        if let Some(b) = builtins_of(a.tools, &c.known)? {
            scope.builtins = narrow::narrow_scope(c.scope(), Some(&b), None, &c.known)
                .map_err(refused)?
                .builtins;
        }
        if let Some(conn) = a.connectors.as_deref() {
            scope.connectors = narrow::narrow_scope(c.scope(), Some(&[]), Some(conn), &c.known)
                .map_err(refused)?
                .connectors;
        }
        change.tools = Some(Some(
            serde_json::to_string(&scope).map_err(|e| ToolResult::error(e.to_string()))?,
        ));
    }
    if let Some(level) = autonomy_of(a.autonomy)? {
        let level = narrow::narrow_autonomy(c.level, c.ceiling(), Some(level)).map_err(refused)?;
        change.autonomy = Some(Some(level.as_str().to_owned()));
    }
    let mut done = Vec::new();
    if change.name.is_some() {
        done.push("name");
    }
    if change.tools.is_some() {
        done.push("tools");
    }
    if change.autonomy.is_some() {
        done.push("autonomy");
    }
    let role = a.role.filter(|r| !r.trim().is_empty());
    if let Some(r) = &role {
        c.rt.write_persona(&target.id, r)
            .map_err(|e| ToolResult::error(format!("{e:#}")))?;
        done.push("role");
    }
    if done.is_empty() {
        return Err(refused(
            "nothing to change: give a name, role, tools, connectors or autonomy",
        ));
    }
    c.rt.store
        .update_agent(&target.id, &change)
        .map_err(|e| refused(e.to_string()))?;
    c.changed();
    Ok(json!({ "edited": format!("@{}", target.slug), "changed": done }))
}

// ---- agent_delete ----

pub struct AgentDelete(pub AdminSlot);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    agent: String,
    /// `delete`, or the agent to hand its routines to.
    #[serde(default)]
    routines: Option<String>,
}

#[async_trait]
impl Tool for AgentDelete {
    fn effect(&self) -> Effect {
        Effect::Destructive
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "agent_delete",
            "Delete an agent for good. Only an agent that can do no more than you can; never the default agent or yourself. If it has routines, say what becomes of them: `routines` is \"delete\" or the name of an agent to hand them to. The user is always asked.",
            json!({
                "type": "object",
                "required": ["agent"],
                "properties": {
                    "agent": { "type": "string" },
                    "routines": { "type": "string", "description": "'delete', or the agent to hand its routines to." }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Destructive, false)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let n = args.get("agent").and_then(Value::as_str).unwrap_or("?");
        Some(format!("Delete agent {n}"))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: DeleteArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let c = match Caller::new(&self.0, ctx) {
            Ok(c) => c,
            Err(e) => return e,
        };
        match delete_agent(&c, a) {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => e,
        }
    }
}

fn delete_agent(c: &Caller, a: DeleteArgs) -> Result<Value, ToolResult> {
    let target = c.agent(&a.agent)?;
    if target.id == c.me.id {
        return Err(refused("you can't delete yourself"));
    }
    c.may_change(&target)?;
    let routines = a
        .routines
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let opts = crate::manage::DeleteAgent {
        delete_routines: routines.is_some_and(|r| r.eq_ignore_ascii_case("delete")),
        reassign_to: routines
            .filter(|r| !r.eq_ignore_ascii_case("delete"))
            .map(|r| c.agent(r).map(|t| t.id))
            .transpose()?,
    };
    let done =
        c.rt.delete_agent(&target, &opts)
            .map_err(|e| refused(e.to_string()))?;
    c.changed();
    let _ =
        c.rt.changes
            .send(Change::SchedulesChanged { id: String::new() });
    Ok(json!({ "deleted": format!("@{}", target.slug), "touched": done.describe() }))
}

// ---- channels ----

/// A topic channel by slug or id.
fn topic(c: &Caller, name: &str) -> Result<ChannelRow, ToolResult> {
    let found =
        c.rt.store
            .channel(name.trim().trim_start_matches('#'))
            .map_err(|e| ToolResult::error(e.to_string()))?;
    match found {
        Some(ch) if ch.kind == "topic" && ch.archived_at.is_none() => Ok(ch),
        Some(_) => Err(refused(
            "only topic channels can be changed this way; Home and DMs belong to the app",
        )),
        None => Err(refused(format!("there is no channel #{name}"))),
    }
}

pub struct ChannelCreate(pub AdminSlot);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelCreateArgs {
    slug: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    agents: Vec<String>,
}

#[async_trait]
impl Tool for ChannelCreate {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "channel_create",
            "Create a topic channel, optionally with agents in it and instructions every agent there follows. The user is asked to approve.",
            json!({
                "type": "object",
                "required": ["slug"],
                "properties": {
                    "slug": { "type": "string", "description": "Lowercase letters, digits and dashes, e.g. 'launch'." },
                    "title": { "type": "string" },
                    "instructions": { "type": "string", "description": "What every agent in the channel should know." },
                    "agents": { "type": "array", "items": { "type": "string" }, "description": "Agents to add, by name." }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Write, false)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let n = args.get("slug").and_then(Value::as_str).unwrap_or("?");
        Some(format!("Create channel #{}", n.trim_start_matches('#')))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: ChannelCreateArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let c = match Caller::new(&self.0, ctx) {
            Ok(c) => c,
            Err(e) => return e,
        };
        match channel_create(&c, a) {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => e,
        }
    }
}

fn channel_create(c: &Caller, a: ChannelCreateArgs) -> Result<Value, ToolResult> {
    let slug = a.slug.trim().trim_start_matches('#').to_owned();
    tern_core::channels::check_slug(&slug).map_err(refused)?;
    let store = &c.rt.store;
    let db = |e: tern_store::StoreError| ToolResult::error(e.to_string());
    if store.channel(&slug).map_err(db)?.is_some() {
        return Err(refused(format!("there is already a channel #{slug}")));
    }
    // Every name is checked before the channel exists.
    let members: Vec<AgentRow> = a
        .agents
        .iter()
        .map(|n| c.agent(n))
        .collect::<Result<_, _>>()?;
    let row = store
        .create_channel(&ChannelRow {
            id: tern_core::agent::new_id(),
            title: a
                .title
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| slug.clone()),
            slug,
            kind: "topic".into(),
            instructions: a.instructions.filter(|i| !i.trim().is_empty()),
            created_by: c.me.id.clone(),
            ..ChannelRow::default()
        })
        .map_err(db)?;
    for m in &members {
        store.add_channel_agent(&row, &m.id).map_err(db)?;
    }
    c.channels_changed();
    Ok(json!({
        "created": format!("#{}", row.slug),
        "agents": members.iter().map(|m| format!("@{}", m.slug)).collect::<Vec<_>>(),
    }))
}

pub struct ChannelEdit(pub AdminSlot);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelEditArgs {
    channel: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    add_agents: Vec<String>,
    #[serde(default)]
    remove_agents: Vec<String>,
    #[serde(default)]
    lead: Option<String>,
}

#[async_trait]
impl Tool for ChannelEdit {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "channel_edit",
            "Change a topic channel: its name (`slug`), title, instructions (an empty string clears them), who is in it, and who leads it. The user is asked to approve.",
            json!({
                "type": "object",
                "required": ["channel"],
                "properties": {
                    "channel": { "type": "string", "description": "Its slug, e.g. 'launch'." },
                    "slug": { "type": "string" },
                    "title": { "type": "string" },
                    "instructions": { "type": "string" },
                    "add_agents": { "type": "array", "items": { "type": "string" } },
                    "remove_agents": { "type": "array", "items": { "type": "string" } },
                    "lead": { "type": "string", "description": "The agent that answers posts that mention no one." }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Write, true)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let n = args.get("channel").and_then(Value::as_str).unwrap_or("?");
        Some(format!("Edit channel #{}", n.trim_start_matches('#')))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: ChannelEditArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let c = match Caller::new(&self.0, ctx) {
            Ok(c) => c,
            Err(e) => return e,
        };
        match channel_edit(&c, a) {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => e,
        }
    }
}

fn channel_edit(c: &Caller, a: ChannelEditArgs) -> Result<Value, ToolResult> {
    let ch = topic(c, &a.channel)?;
    let store = &c.rt.store;
    let db = |e: tern_store::StoreError| ToolResult::error(e.to_string());
    // Everything is checked before anything changes.
    let add: Vec<AgentRow> = a
        .add_agents
        .iter()
        .map(|n| c.agent(n))
        .collect::<Result<_, _>>()?;
    let remove: Vec<AgentRow> = a
        .remove_agents
        .iter()
        .map(|n| c.agent(n))
        .collect::<Result<_, _>>()?;
    let lead = a.lead.as_deref().map(|n| c.agent(n)).transpose()?;
    for r in &remove {
        if r.is_default() || r.id == ch.lead() || lead.as_ref().is_some_and(|l| l.id == r.id) {
            return Err(refused(format!(
                "{} can't leave #{}: it's the lead or the default agent",
                r.name, ch.slug
            )));
        }
    }
    let title = a.title.as_deref().map(str::trim);
    if title == Some("") {
        return Err(refused("the title is empty"));
    }
    let mut done = Vec::new();
    if let Some(slug) = a.slug.as_deref() {
        c.rt.rename_channel_slug(&ch, slug)
            .map_err(|e| refused(e.to_string()))?;
        done.push("name");
    }
    if title.is_some() || a.instructions.is_some() {
        let instructions = a
            .instructions
            .as_deref()
            .map(|i| Some(i.trim()).filter(|t| !t.is_empty()));
        store
            .update_channel(&ch.id, title, instructions, None)
            .map_err(db)?;
        done.push(if title.is_some() {
            "title"
        } else {
            "instructions"
        });
    }
    for m in &add {
        store.add_channel_agent(&ch, &m.id).map_err(db)?;
    }
    if let Some(l) = &lead {
        store.add_channel_agent(&ch, &l.id).map_err(db)?;
        store.set_channel_lead(&ch.id, &l.id).map_err(db)?;
        done.push("lead");
    }
    for m in &remove {
        store.remove_channel_agent(&ch, &m.id).map_err(db)?;
    }
    if !add.is_empty() || !remove.is_empty() {
        done.push("members");
    }
    if done.is_empty() {
        return Err(refused("nothing to change"));
    }
    c.channels_changed();
    let _ = c.rt.changes.send(Change::AgentsChanged {});
    Ok(json!({ "edited": format!("#{}", ch.slug), "changed": done }))
}

pub struct ChannelDelete(pub AdminSlot);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelDeleteArgs {
    channel: String,
}

#[async_trait]
impl Tool for ChannelDelete {
    fn effect(&self) -> Effect {
        Effect::Destructive
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "channel_delete",
            "Delete a topic channel and its conversations for good. The user is always asked.",
            json!({
                "type": "object",
                "required": ["channel"],
                "properties": { "channel": { "type": "string", "description": "Its slug." } }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Destructive, false)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let n = args.get("channel").and_then(Value::as_str).unwrap_or("?");
        Some(format!("Delete channel #{}", n.trim_start_matches('#')))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: ChannelDeleteArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let c = match Caller::new(&self.0, ctx) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let ch = match topic(&c, &a.channel) {
            Ok(ch) => ch,
            Err(e) => return e,
        };
        match c.rt.delete_channel(&ch) {
            Ok(done) => {
                c.channels_changed();
                let _ =
                    c.rt.changes
                        .send(Change::SchedulesChanged { id: String::new() });
                ToolResult::ok(
                    json!({ "deleted": format!("#{}", ch.slug), "touched": done.describe() })
                        .to_string(),
                )
            }
            Err(e) => refused(e),
        }
    }
}

/// Register the tools, acting through `slot`.
pub fn register(registry: &mut tern_core::tool::ToolRegistry, slot: &AdminSlot) {
    registry.register(Arc::new(AgentCreate(slot.clone())));
    registry.register(Arc::new(AgentEdit(slot.clone())));
    registry.register(Arc::new(AgentDelete(slot.clone())));
    registry.register(Arc::new(ChannelCreate(slot.clone())));
    registry.register(Arc::new(ChannelEdit(slot.clone())));
    registry.register(Arc::new(ChannelDelete(slot.clone())));
}
