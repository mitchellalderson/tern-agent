//! Agent profiles (docs/agent-profiles-design.md §7.1). `{id}` is an
//! agent's id or slug; the default agent's is `tern`.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/agents[?archived=1]` | `[Agent]`: the default first, with `main_channel_id`, `last_activity`, `needs_you`, `active_runs` |
//! | POST | `/v1/agents` | create `{name, slug?, avatar?, persona?, model?, subagent_model?, tools?, memory?, template?}`, with its DM channel |
//! | PATCH | `/v1/agents/{id}` | any of those but `template` and `persona`; `archived: true` archives (not the default agent) |
//! | GET / PUT | `/v1/agents/{id}/persona` | `{persona}`; GET adds `system_sections`, what tern adds to every persona |
//! | GET | `/v1/agents/templates` | `[Template]` for the create sheet |
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
use tern_core::agents::{self, ToolScope};
use tern_core::config::ModelChain;
use tern_core::models::Change;
use tern_store::{AgentChange, AgentRow, DEFAULT_AGENT};

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/agents", get(list).post(create))
        .route("/v1/agents/templates", get(templates))
        .route("/v1/agents/{id}", axum::routing::patch(update))
        .route("/v1/agents/{id}/persona", get(persona).put(set_persona))
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
    #[serde(default)]
    pub delegates: Vec<String>,
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_default: bool,
    pub updated_at: i64,
}

pub(super) fn agent_view(m: &Shared, a: AgentRow) -> Agent {
    let store = &m.rt.store;
    let active_runs = m
        .active_runs()
        .into_iter()
        .filter(|(session, _)| {
            store
                .session(session)
                .ok()
                .flatten()
                .is_some_and(|s| s.agent() == a.id && s.source != "subagent")
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
        delegates: a
            .delegates
            .as_deref()
            .and_then(|d| serde_json::from_str(d).ok())
            .unwrap_or_default(),
        archived: a.archived_at.is_some(),
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
}

async fn list(State(m): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Vec<Agent>> {
    let all = matches!(q.archived.as_deref(), Some("1" | "true"));
    let rows = m.rt.store.agents(all).map_err(internal)?;
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
    let mut ch = AgentChange {
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
        delegates: b
            .delegates
            .map(|v| v.map(|d| serde_json::to_string(&d)).transpose())
            .transpose()
            .map_err(internal)?,
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
