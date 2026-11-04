//! Agent profiles (docs/agent-profiles-design.md §7.1). `{id}` is an
//! agent's id or slug; the default agent's is `tern`.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/agents[?archived=1\|deleted=1]` | `[Agent]` (`deleted=1`: archived and deleted ones too, deleted marked `deleted`, so history can name them): the default first, with `main_channel_id`, `last_activity`, `needs_you`, `active_runs` |
//! | POST | `/v1/agents` | create `{name, slug?, avatar?, persona?, model?, subagent_model?, tools?, memory?, egress?, autonomy?, template?}`, with its DM channel |
//! | PATCH | `/v1/agents/{id}` | any of those but `template` and `persona`, plus `telegram` (`{bot_token_secret}`: a bot of its own, from the next daemon start; `null` removes it), and `delegates` (agents it may hand work to; `null`: all for the default agent, none for others); `archived: true` archives (not the default agent) |
//! | DELETE | `/v1/agents/{id}?confirm=true[&reassign_to=<agent>\|&delete_routines=true]` | delete it (never the default agent): its conversations stay in History under its name; refused (409) while it has runs in progress or writes in the outbox, or routines that `reassign_to` or `delete_routines` doesn't place |
//! | GET | `/v1/agents/{id}/impact` | what deleting it touches: `{routines, led_channels, member_channels, conversations, memories, running_runs, pending_outbox, blockers}` |
//! | GET / PUT | `/v1/agents/{id}/persona` | `{persona}`; GET adds `system_sections`, what tern adds to every persona |
//! | GET | `/v1/agents/templates` | `[Template]` for the create sheet |
//! | GET | `/v1/agents/{id}/work?limit=` | its recent work, newest first: delegated batches (`delegated` posts in its DM) and routine runs |
//! | GET | `/v1/memories?agent=&q=&limit=` | memories: `agent` = `shared`, an agent (its own), or absent for all |
//!
//! `model` is a model or a fallback chain (a list). The default agent's
//! model is `default_model`: setting it there writes config.toml, as
//! `PUT /v1/config/default_model` does. Changes apply to an agent's next
//! run, never one already working. Every change sends `agents_changed`.

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use tern_core::agents::{self, AgentTelegram, ToolScope};
use tern_core::approval::{Autonomy, Egress};
use tern_core::config::ModelChain;
use tern_core::models::Change;
use tern_store::{AgentChange, AgentRow, DEFAULT_AGENT};

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/agents", get(list).post(create))
        .route("/v1/agents/templates", get(templates))
        .route(
            "/v1/agents/{id}",
            axum::routing::patch(update).delete(delete),
        )
        .route("/v1/agents/{id}/impact", get(impact))
        .route("/v1/agents/{id}/persona", get(persona).put(set_persona))
        .route("/v1/agents/{id}/work", get(work))
        .route("/v1/memories", get(memories))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Avatar {
    pub glyph: String,
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub avatar: Option<Avatar>,
    /// A model or a chain; absent = `default_model`.
    #[serde(default)]
    pub model: Option<ModelChain>,
    #[serde(default)]
    pub subagent_model: Option<ModelChain>,
    /// Absent = every tool (the default agent's scope).
    #[serde(default)]
    pub tools: Option<ToolScope>,
    /// `shared` or `private`.
    pub memory: String,
    /// Agent ids it may hand work to with `agent_spawn {agent}`. Absent:
    /// every agent for the default agent, none for others.
    #[serde(default)]
    pub delegates: Option<Vec<String>>,
    /// The tainted host limit for its runs (`ask` or `allow`); absent = the
    /// configured `[taint] egress`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<Egress>,
    /// The most autonomy its runs may have (`readonly`, `supervised`,
    /// `autonomous`, `yolo`); the lower of this and the configured
    /// `autonomy` applies. Absent = no ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<Autonomy>,
    /// A Telegram bot of its own: the secret holding its token (never the
    /// token) and, for tests, a Bot API URL. It starts when the daemon
    /// does. Absent: reached through the main bot's `/agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram: Option<AgentTelegram>,
    /// Its DM: `home` for the default agent.
    pub main_channel_id: Option<String>,
    #[serde(default)]
    pub last_activity: Option<i64>,
    /// Approvals, secret requests and outbox items from its runs, waiting
    /// for an answer.
    #[serde(default)]
    pub needs_you: usize,
    /// Its top-level runs in progress.
    #[serde(default)]
    pub active_runs: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    /// Deleted: only listed with `?deleted=1`, so history can name it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_default: bool,
    pub updated_at: i64,
}

/// A refused change is the caller's to fix (409); the rest is ours.
pub(super) fn manage_err(e: crate::manage::ManageError) -> ApiError {
    match e {
        crate::manage::ManageError::Refused(m) => ApiError(StatusCode::CONFLICT, m),
        crate::manage::ManageError::Other(e) => internal(format!("{e:#}")),
    }
}

pub(super) fn agent_view(m: &Shared, a: AgentRow) -> Agent {
    let store = &m.rt.store;
    // Its own runs (its DM, its routines) and work delegated to it; not
    // the ordinary sub-agents of its runs.
    let active_runs = m
        .active_runs()
        .into_iter()
        .filter(|(session, run)| {
            store.session(session).ok().flatten().is_some_and(|s| {
                s.agent() == a.id
                    && (s.source != "subagent"
                        || store.run(run).ok().flatten().is_some_and(|r| {
                            r.parent_run_id
                                .as_deref()
                                .and_then(|p| store.run(p).ok().flatten())
                                .and_then(|p| store.session_agent(&p.session_id).ok())
                                .is_some_and(|parent| parent != a.id)
                        }))
            })
        })
        .count();
    let needs_you = m
        .routes
        .api
        .pending()
        .iter()
        .filter(|p| p.agent_id.as_deref().unwrap_or(DEFAULT_AGENT) == a.id)
        .count()
        + m.routes
            .secrets
            .pending()
            .iter()
            .filter(|p| p.agent_id.as_deref().unwrap_or(DEFAULT_AGENT) == a.id)
            .count()
        + store
            .outbox(Some("pending"), 200)
            .unwrap_or_default()
            .into_iter()
            .filter(|o| m.outbox_view(o.clone()).agent_id.as_deref() == Some(a.id.as_str()))
            .count();
    let model = if a.is_default() {
        m.rt.live().default_model
    } else {
        a.model.as_deref().and_then(ModelChain::from_stored)
    };
    Agent {
        main_channel_id: store.agent_channel(&a.id).ok().flatten().map(|c| c.id),
        last_activity: store.agent_last_activity(&a.id).ok().flatten(),
        needs_you,
        active_runs,
        avatar: a
            .avatar
            .as_deref()
            .and_then(|v| serde_json::from_str(v).ok()),
        model,
        subagent_model: a
            .subagent_model
            .as_deref()
            .and_then(ModelChain::from_stored),
        tools: a.tools.as_deref().and_then(|t| ToolScope::parse(t).ok()),
        delegates: a.delegate_list(),
        egress: a.egress.as_deref().and_then(Egress::parse),
        autonomy: a.autonomy.as_deref().and_then(Autonomy::parse),
        telegram: AgentTelegram::parse(a.telegram.as_deref()),
        archived: a.archived_at.is_some(),
        deleted: a.deleted_at.is_some(),
        is_default: a.is_default(),
        id: a.id,
        slug: a.slug,
        name: a.name,
        memory: a.memory,
        updated_at: a.updated_at,
    }
}

pub(super) fn find(m: &Shared, id: &str) -> Result<AgentRow, ApiError> {
    m.rt.store
        .agent(id)
        .map_err(internal)?
        .ok_or_else(|| not_found("agent"))
}

fn changed(m: &Shared) {
    m.rt.refresh_agent_chains();
    let _ = m.rt.changes.send(Change::AgentsChanged {});
    let _ = m.rt.changes.send(Change::ChannelsChanged {});
}

#[derive(Deserialize)]
struct ListQuery {
    archived: Option<String>,
    deleted: Option<String>,
}

async fn list(State(m): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Vec<Agent>> {
    let yes = |v: &Option<String>| matches!(v.as_deref(), Some("1" | "true"));
    let deleted = yes(&q.deleted);
    let mut rows =
        m.rt.store
            .agents(deleted || yes(&q.archived))
            .map_err(internal)?;
    if deleted {
        rows.extend(m.rt.store.deleted_agents().map_err(internal)?);
    }
    Ok(Json(rows.into_iter().map(|a| agent_view(&m, a)).collect()))
}

async fn templates() -> Json<Vec<agents::Template>> {
    Json(agents::templates())
}

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
fn double<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    name: String,
    slug: Option<String>,
    avatar: Option<Avatar>,
    persona: Option<String>,
    model: Option<ModelChain>,
    subagent_model: Option<ModelChain>,
    tools: Option<ToolScope>,
    memory: Option<String>,
    egress: Option<Egress>,
    autonomy: Option<Autonomy>,
    template: Option<String>,
}

fn check_memory(v: &str) -> Result<(), ApiError> {
    match v {
        "shared" | "private" => Ok(()),
        _ => Err(bad(format!(
            "memory must be \"shared\" or \"private\", not {v:?}"
        ))),
    }
}

/// Every model in `chain` resolves to a configured provider.
fn check_chain(m: &Shared, chain: &ModelChain) -> Result<String, ApiError> {
    let mut out = Vec::new();
    for e in chain.entries() {
        out.push(m.rt.check_model(e).map_err(bad)?.to_string());
    }
    ModelChain::new(out)
        .map(|c| c.to_stored())
        .ok_or_else(|| bad("model is empty"))
}

fn check_slug_free(m: &Shared, slug: &str, me: Option<&str>) -> Result<(), ApiError> {
    agents::check_slug(slug).map_err(bad)?;
    if let Some(a) = m.rt.store.agent(slug).map_err(internal)?
        && Some(a.id.as_str()) != me
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!("there is already an agent @{slug}"),
        ));
    }
    if m.rt
        .store
        .channel(&tern_store::dm_slug(slug))
        .map_err(internal)?
        .is_some_and(|c| c.agent_id.as_deref() != me)
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!("the channel #{} is taken", tern_store::dm_slug(slug)),
        ));
    }
    Ok(())
}

async fn create(
    State(m): State<Shared>,
    Json(b): Json<CreateBody>,
) -> Result<(StatusCode, Json<Agent>), ApiError> {
    let slug = match b.slug.as_deref().map(|s| s.trim().trim_start_matches('@')) {
        Some(s) if !s.is_empty() => s.to_owned(),
        _ => agents::slug_for(&b.name),
    };
    check_slug_free(&m, &slug, None)?;
    if let Some(v) = &b.memory {
        check_memory(v)?;
    }
    let row =
        m.rt.create_agent(crate::runtime::NewAgent {
            name: b.name,
            slug: Some(slug),
            avatar: b
                .avatar
                .map(|a| serde_json::to_string(&a))
                .transpose()
                .map_err(internal)?,
            persona: b.persona,
            model: b.model.as_ref().map(|c| check_chain(&m, c)).transpose()?,
            subagent_model: b
                .subagent_model
                .as_ref()
                .map(|c| check_chain(&m, c))
                .transpose()?,
            tools: b.tools,
            memory: b.memory,
            egress: b.egress,
            autonomy: b.autonomy,
            template: b.template,
        })
        .map_err(bad)?;
    changed(&m);
    Ok((StatusCode::CREATED, Json(agent_view(&m, row))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    name: Option<String>,
    slug: Option<String>,
    #[serde(default, deserialize_with = "double")]
    avatar: Option<Option<Avatar>>,
    #[serde(default, deserialize_with = "double")]
    model: Option<Option<ModelChain>>,
    #[serde(default, deserialize_with = "double")]
    subagent_model: Option<Option<ModelChain>>,
    #[serde(default, deserialize_with = "double")]
    tools: Option<Option<ToolScope>>,
    memory: Option<String>,
    #[serde(default, deserialize_with = "double")]
    delegates: Option<Option<Vec<String>>>,
    /// `null` goes back to the configured `[taint] egress`.
    #[serde(default, deserialize_with = "double")]
    egress: Option<Option<Egress>>,
    /// `null` removes the ceiling.
    #[serde(default, deserialize_with = "double")]
    autonomy: Option<Option<Autonomy>>,
    /// `{bot_token_secret}` gives the agent a bot of its own (set the
    /// secret first: `tern secrets set <name>`); `null` removes it.
    /// Applies when the daemon restarts.
    #[serde(default, deserialize_with = "double")]
    telegram: Option<Option<AgentTelegram>>,
    archived: Option<bool>,
}

async fn update(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Agent> {
    let a = find(&m, &id)?;
    let name = b.name.as_deref().map(str::trim).map(str::to_owned);
    if name.as_deref() == Some("") {
        return Err(bad("name is empty"));
    }
    if let Some(s) = &b.slug {
        if a.is_default() && s != DEFAULT_AGENT {
            return Err(bad(
                "the default agent's slug stays `tern`; rename it instead",
            ));
        }
        check_slug_free(&m, s, Some(&a.id))?;
    }
    if let Some(v) = &b.memory {
        check_memory(v)?;
        if a.is_default() && v != "shared" {
            return Err(bad("the default agent's memory is the shared memory"));
        }
    }
    if a.is_default() && b.archived == Some(true) {
        return Err(bad("the default agent can't be archived"));
    }
    let telegram = match &b.telegram {
        None => None,
        Some(None) => Some(None),
        Some(Some(t)) => {
            check_bot_secret(&m, &a, t)?;
            Some(Some(serde_json::to_string(t).map_err(internal)?))
        }
    };
    let mut ch = AgentChange {
        telegram,
        slug: b.slug.clone(),
        name,
        avatar: b
            .avatar
            .map(|v| v.map(|a| serde_json::to_string(&a)).transpose())
            .transpose()
            .map_err(internal)?,
        subagent_model: b
            .subagent_model
            .as_ref()
            .map(|v| v.as_ref().map(|c| check_chain(&m, c)).transpose())
            .transpose()?,
        tools: b
            .tools
            .map(|v| v.map(|t| serde_json::to_string(&t)).transpose())
            .transpose()
            .map_err(internal)?,
        memory: b.memory,
        delegates: match b.delegates {
            None => None,
            Some(None) => Some(None),
            Some(Some(list)) => Some(Some(
                serde_json::to_string(&delegate_ids(&m, &a, &list)?).map_err(internal)?,
            )),
        },
        egress: b.egress.map(|v| v.map(|e| e.as_str().to_owned())),
        autonomy: b.autonomy.map(|v| v.map(|e| e.as_str().to_owned())),
        archived: b.archived,
        ..AgentChange::default()
    };
    match (&b.model, a.is_default()) {
        // The default agent's model is `default_model`.
        (Some(Some(chain)), true) => {
            if !chain.fallbacks().is_empty() {
                return Err(bad(
                    "set the default agent's fallback chain as default_model in config.toml",
                ));
            }
            m.rt.set_default_model(chain.primary()).map_err(bad)?;
        }
        (Some(None), true) => {
            return Err(bad("the default agent always uses default_model"));
        }
        (Some(v), false) => {
            ch.model = Some(v.as_ref().map(|c| check_chain(&m, c)).transpose()?);
        }
        (None, _) => {}
    }
    m.rt.store.update_agent(&a.id, &ch).map_err(internal)?;
    changed(&m);
    Ok(Json(agent_view(&m, find(&m, &a.id)?)))
}

/// Stop the poller of a deleted agent's own Telegram bot (`telegram-<slug>`,
/// started with the daemon), so it doesn't keep taking messages for an
/// agent that is gone. Returns whether one was running.
pub(super) fn stop_bot(m: &Shared, slug: &str) -> bool {
    let name = format!("{}-{slug}", tern_gateway::telegram::SOURCE);
    let Ok(mut tasks) = m.routes.lifecycle.gateway_tasks.lock() else {
        return false;
    };
    let mut stopped = false;
    tasks.retain(|(n, task)| {
        if *n == name {
            task.abort();
            stopped = true;
            false
        } else {
            true
        }
    });
    stopped
}

#[derive(Deserialize)]
struct DeleteQuery {
    confirm: Option<String>,
    reassign_to: Option<String>,
    delete_routines: Option<String>,
}

async fn impact(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<crate::manage::AgentImpact> {
    let a = find(&m, &id)?;
    m.rt.agent_impact(&a).map(Json).map_err(manage_err)
}

/// Delete an agent. Needs `confirm=true`; what to do with its routines is
/// `reassign_to=<agent>` or `delete_routines=true`.
async fn delete(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<crate::manage::AgentImpact> {
    let a = find(&m, &id)?;
    if !matches!(q.confirm.as_deref(), Some("1" | "true")) {
        return Err(bad(format!(
            "deleting @{} can't be undone: pass confirm=true (GET /v1/agents/{}/impact shows what it touches)",
            a.slug, a.slug
        )));
    }
    let bot = agents::AgentTelegram::parse(a.telegram.as_deref()).map(|_| a.slug.clone());
    let done =
        m.rt.delete_agent(
            &a,
            &crate::manage::DeleteAgent {
                reassign_to: q.reassign_to.filter(|r| !r.is_empty()),
                delete_routines: matches!(q.delete_routines.as_deref(), Some("1" | "true")),
            },
        )
        .map_err(manage_err)?;
    if let Some(slug) = bot {
        stop_bot(&m, &slug);
    }
    changed(&m);
    let _ =
        m.rt.changes
            .send(Change::SchedulesChanged { id: String::new() });
    Ok(Json(done))
}

/// A bot's secret must be a valid secret name that no other bot (the main
/// one or another agent's) polls: two pollers on one token fight over
/// updates.
fn check_bot_secret(m: &Shared, me: &AgentRow, t: &AgentTelegram) -> Result<(), ApiError> {
    if me.is_default() {
        return Err(bad(
            "the default agent's bot is [gateway.telegram] in config.toml",
        ));
    }
    if t.api_url.is_some() {
        // The token would go to whatever host this named.
        return Err(bad(
            "api_url isn't set through the API; an agent's bot talks to Telegram",
        ));
    }
    tern_core::secrets::check_name(&t.bot_token_secret).map_err(|e| bad(e.to_string()))?;
    let cfg = m.rt.live();
    if cfg
        .gateway
        .telegram
        .as_ref()
        .is_some_and(|g| g.bot_token_secret == t.bot_token_secret)
    {
        return Err(bad(format!(
            "{} is the main bot's token; an agent's bot needs its own",
            t.bot_token_secret
        )));
    }
    for other in m.rt.store.agents(true).map_err(internal)? {
        if other.id != me.id
            && AgentTelegram::parse(other.telegram.as_deref())
                .is_some_and(|o| o.bot_token_secret == t.bot_token_secret)
        {
            return Err(bad(format!(
                "@{} already uses {}",
                other.slug, t.bot_token_secret
            )));
        }
    }
    Ok(())
}

/// `list` (ids, @names or names) as agent ids: other agents that exist.
fn delegate_ids(m: &Shared, me: &AgentRow, list: &[String]) -> Result<Vec<String>, ApiError> {
    let mut out = Vec::new();
    for name in list {
        let a = find(m, name.trim().trim_start_matches('@'))?;
        if a.id == me.id {
            return Err(bad("an agent can't delegate to itself"));
        }
        if !out.contains(&a.id) {
            out.push(a.id);
        }
    }
    Ok(out)
}

#[derive(Serialize, Deserialize)]
pub struct Persona {
    pub persona: String,
    /// What tern adds under every persona, for the editor's read-only
    /// preview (§1.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_sections: Option<String>,
}

async fn persona(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Persona> {
    let a = find(&m, &id)?;
    let path = agents::persona_path(&m.rt.home, &a.id);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tern_core::defaults::PERSONA_MD.to_owned()
        }
        Err(e) => return Err(internal(format!("reading {}: {e}", path.display()))),
    };
    Ok(Json(Persona {
        system_sections: Some(m.rt.system_sections(&a)),
        persona: text.trim().to_owned(),
    }))
}

async fn set_persona(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<Persona>,
) -> ApiResult<Persona> {
    let a = find(&m, &id)?;
    if b.persona.trim().is_empty() {
        return Err(bad("persona is empty"));
    }
    m.rt.write_persona(&a.id, &b.persona).map_err(internal)?;
    m.rt.store
        .update_agent(&a.id, &AgentChange::default())
        .map_err(internal)?;
    changed(&m);
    Ok(Json(Persona {
        persona: b.persona.trim().to_owned(),
        system_sections: None,
    }))
}

#[derive(Deserialize)]
struct WorkQuery {
    limit: Option<usize>,
}

/// What it did lately: delegated batches and routine runs.
async fn work(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<WorkQuery>,
) -> ApiResult<Vec<super::delegations::Work>> {
    let a = find(&m, &id)?;
    m.recent_work(&a.id, q.limit.unwrap_or(10).clamp(1, 50))
        .map(Json)
        .map_err(internal)
}

#[derive(Deserialize)]
struct MemoriesQuery {
    agent: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: i64,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: i64,
    /// Absent = shared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

async fn memories(State(m): State<Shared>, Query(q): Query<MemoriesQuery>) -> ApiResult<Value> {
    let scope = match q.agent.as_deref() {
        None | Some("") | Some("all") => None,
        Some("shared") => Some("shared".to_owned()),
        Some(a) => Some(find(&m, a)?.id),
    };
    let rows =
        m.rt.store
            .list_memories(
                scope.as_deref(),
                q.q.as_deref(),
                q.limit.unwrap_or(100).clamp(1, 500),
            )
            .map_err(internal)?;
    let out: Vec<Memory> = rows
        .into_iter()
        .map(|r| Memory {
            id: r.id,
            content: r.content,
            tags: r.tags.split_whitespace().map(str::to_owned).collect(),
            created_at: r.created_at,
            agent_id: r.agent_id,
        })
        .collect();
    serde_json::to_value(out).map(Json).map_err(internal)
}
