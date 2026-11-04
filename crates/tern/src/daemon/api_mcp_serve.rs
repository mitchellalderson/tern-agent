//! What `tern mcp serve` (T6d) asks the daemon: the tools it may offer an
//! MCP client, and one call at a time, run under tern's policy.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/mcp-serve/tools[?agent=]` | `{agent, tools: [ApiTool]}`: what the client may see |
//! | POST | `/v1/mcp-serve/call` | `{name, arguments, session_id?, client?, agent?}` → [`CallReply`] |
//!
//! A call is a run with trigger `mcp` in the client's session (source
//! `mcp`, created on its first call), journaled like any other: it shows
//! in tern, its approvals wait in tern (the client never sees them), and a
//! write nobody answers goes to the outbox. It starts tainted
//! ([`tern_core::agent::mcp::TAINT`]). Results are redacted of every known
//! secret value. `agent` overrides `[mcp.serve] agent` for one client
//! (`tern mcp serve --agent`).
//!
//! What a client may see is the intersection of [`EXPOSABLE`] (plus every
//! Luau connector tool) with the agent's tool scope; with no agent set,
//! only [`DEFAULT_SCOPE`]. Nothing that reads files, runs commands, fetches
//! URLs, asks for secrets or signs in is ever offered.
//!
//! Over TCP, the list is `read` and a call is `owner` (it isn't in
//! `auth::CHAT_WRITES`).

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tern_core::agent::mcp;
use tern_core::approval::Autonomy;
use tern_core::config::ModelRef;
use tern_core::types::ToolCall;
use tokio_util::sync::CancellationToken;

use super::api::{ApiError, ApiResult, ApiTool, Shared, bad, internal};

/// Built-in tools an MCP client may ever be offered, besides connector
/// tools (`lua__*`).
pub const EXPOSABLE: &[&str] = &[
    "memory_search",
    "memory_save",
    "channel_read",
    "channel_post",
    "schedule_list",
    "schedule_create",
    "schedule_update",
    "schedule_delete",
    "skill_read",
];

/// What a client sees when `[mcp.serve] agent` isn't set: reads only.
pub const DEFAULT_SCOPE: &[&str] = &[
    "memory_search",
    "channel_read",
    "schedule_list",
    "skill_read",
];

/// The prefix of Luau connector tools.
const CONNECTOR: &str = "lua__";

fn exposable(name: &str) -> bool {
    EXPOSABLE.contains(&name) || name.starts_with(CONNECTOR)
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/mcp-serve/tools", get(tools))
        .route("/v1/mcp-serve/call", post(call))
}

/// The scope in force: the agent (slug or id, `None` when unset) and the
/// tools it allows, as a frozen run scope (names and `*` patterns).
struct Exposure {
    agent: Option<tern_store::AgentRow>,
    scope: Vec<String>,
}

impl Exposure {
    fn of(m: &Shared, agent: Option<&str>) -> Result<Self, ApiError> {
        let agent = agent
            .map(str::to_owned)
            .or_else(|| m.rt.live().mcp.serve.agent);
        let Some(slug) = agent else {
            return Ok(Self {
                agent: None,
                scope: DEFAULT_SCOPE.iter().map(|s| (*s).to_owned()).collect(),
            });
        };
        let row =
            m.rt.store
                .agent(&slug)
                .map_err(internal)?
                .filter(|a| a.archived_at.is_none())
                .ok_or_else(|| bad(format!("[mcp.serve] agent {slug:?} not found")))?;
        let builtins = m.rt.agent.tools().builtin_names();
        let scope = match row.tools.as_deref() {
            None => tern_core::agents::ToolScope::default().freeze(&builtins),
            Some(t) => tern_core::agents::ToolScope::parse(t)
                .map_err(internal)?
                .freeze(&builtins),
        };
        Ok(Self {
            agent: Some(row),
            scope,
        })
    }

    fn allows(&self, name: &str) -> bool {
        exposable(name) && tern_core::tool::in_scope(Some(&self.scope), name)
    }
}

#[derive(Deserialize)]
struct ToolsQuery {
    agent: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ServedTools {
    /// The agent whose scope applies (slug), or `None` for the default.
    pub agent: Option<String>,
    pub tools: Vec<ApiTool>,
}

async fn tools(State(m): State<Shared>, Query(q): Query<ToolsQuery>) -> ApiResult<ServedTools> {
    let exposure = Exposure::of(&m, q.agent.as_deref())?;
    let reg = m.rt.agent.tools();
    reg.prepare().await;
    let effects = reg.effects();
    let journal = m.rt.agent.journal();
    let tools = reg
        .specs()
        .into_iter()
        .filter(|s| exposure.allows(&s.name))
        .map(|mut spec| {
            let (effect, untrusted) = effects.get(&spec.name).copied().unwrap_or_default();
            spec.description = journal.redact_text(&spec.description);
            ApiTool {
                spec,
                effect,
                untrusted,
            }
        })
        .collect();
    Ok(Json(ServedTools {
        agent: exposure.agent.map(|a| a.slug),
        tools,
    }))
}

#[derive(Deserialize, Serialize)]
pub struct CallBody {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
    /// The client's session, from an earlier reply; a new one if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The client's name (MCP `clientInfo.name`), for the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// One call's result, as the MCP client gets it.
#[derive(Debug, Serialize, Deserialize)]
pub struct CallReply {
    pub session_id: String,
    pub run_id: String,
    /// Redacted; wrapped in `<untrusted>` when `untrusted`.
    pub content: String,
    pub is_error: bool,
    /// Third-party text: the client should treat it as data.
    pub untrusted: bool,
}

async fn call(State(m): State<Shared>, Json(b): Json<CallBody>) -> ApiResult<CallReply> {
    let exposure = Exposure::of(&m, b.agent.as_deref())?;
    if !exposure.allows(&b.name) {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("tool {:?} is not offered to MCP clients", b.name),
        ));
    }
    let client = b
        .client
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or("an MCP client");
    let client: String = client
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    let session = session(
        &m,
        b.session_id.as_deref(),
        &client,
        exposure.agent.as_ref(),
    )?;
    let call = ToolCall {
        id: format!("mcp_{}", tern_core::agent::new_id()),
        name: b.name.clone(),
        input: match b.arguments {
            Value::Null => Value::Object(Default::default()),
            v => v,
        },
    };
    // Only the tool called: a scope pattern can't widen a call's reach.
    let scope = serde_json::to_string(&[&call.name]).map_err(internal)?;
    let model = ModelRef {
        provider: "tern".into(),
        model: mcp::TRIGGER.into(),
    };
    let memory = exposure
        .agent
        .as_ref()
        .is_some_and(|a| a.private_memory())
        .then_some("private");
    let run =
        m.rt.agent
            .start_run_as(
                &session,
                &mcp::input_text(&client, &call),
                &model,
                "",
                mcp::TRIGGER,
                Some(&scope),
                memory,
                Some(mcp::TAINT),
            )
            .map_err(internal)?;
    let live = m.rt.live();
    let autonomy = live
        .mcp
        .serve
        .autonomy
        .map_or(live.autonomy, |a: Autonomy| a.min(live.autonomy));
    m.rt.store
        .set_run_autonomy(&run.id, autonomy.as_str())
        .map_err(internal)?;
    let egress = exposure
        .agent
        .as_ref()
        .and_then(|a| a.egress.as_deref())
        .and_then(tern_core::approval::Egress::parse)
        .unwrap_or(live.taint.egress);
    m.rt.store
        .set_run_egress(&run.id, egress.as_str())
        .map_err(internal)?;
    tracing::info!(run = %run.id, tool = %call.name, %client, "MCP call");

    // A client that hangs up cancels its call (and any prompt it waits on).
    let cancel = CancellationToken::new();
    let _guard = cancel.clone().drop_guard();
    let (agent_rt, run_id) = (m.rt.clone(), run.id.clone());
    let outcome = tokio::spawn(async move { agent_rt.agent.drive(&run_id, cancel).await })
        .await
        .map_err(internal)?
        .map_err(internal)?;
    let result = outcome.tool_result.unwrap_or_else(|| {
        tern_core::tool::ToolResult::error(
            outcome
                .error
                .unwrap_or_else(|| format!("the call ended {}", outcome.status.as_str())),
        )
    });
    let journal = m.rt.agent.journal();
    let content = journal.redact_text(&result.content);
    let content = if result.untrusted {
        tern_core::agent::taint::wrap(&call.name, &content)
    } else {
        content
    };
    Ok(Json(CallReply {
        session_id: session,
        run_id: run.id,
        content,
        is_error: result.is_error,
        untrusted: result.untrusted,
    }))
}

/// The client's session: `id` if it is an MCP session, else a new one
/// (in the agent's name, so its work reads as that agent's).
fn session(
    m: &Shared,
    id: Option<&str>,
    client: &str,
    agent: Option<&tern_store::AgentRow>,
) -> Result<String, ApiError> {
    if let Some(id) = id
        && let Some(s) = m.rt.store.session(id).map_err(internal)?
        && s.source == mcp::SOURCE
    {
        return Ok(s.id);
    }
    let s =
        m.rt.store
            .create_session_with_ref(&tern_core::agent::new_id(), mcp::SOURCE, client, None)
            .map_err(internal)?;
    if let Some(a) = agent {
        m.rt.store
            .set_session_agent(&s.id, &a.id)
            .map_err(internal)?;
    }
    Ok(s.id)
}
