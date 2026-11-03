//! The daemon's local API: JSON over HTTP on a Unix socket (mode 0600).
//! Used by `tern` to attach the TUI, and by future clients.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/health` | pid, version, and capabilities (see `auth::health`) |
//! | GET | `/v1/info` | default model and tools |
//! | GET | `/v1/tools` | full tool specs: `[{name, description, input_schema}]` |
//! | POST | `/v1/models/check` | validate `{model}` |
//! | POST | `/v1/runs` | send a message: `{text, session_id?, model?}` |
//! | GET | `/v1/runs/active` | runs in progress: `[{run_id, session_id, parent_run_id?, label?}]` |
//! | POST | `/v1/runs/{id}/resume` | resume an interrupted run |
//! | POST | `/v1/runs/{id}/cancel` | cancel a run and its sub-agents |
//! | GET | `/v1/runs/{id}/wait` | block until the run finishes |
//! | GET | `/v1/runs/{id}/session` | the run's session id |
//! | GET | `/v1/runs/{id}/children[?recursive=1]` | its sub-agents: `[ChildRun]` (`api_subagents.rs`) |
//! | POST | `/v1/runs/{id}/message` | `{text}` from the user to a running sub-agent (`api_subagents.rs`) |
//! | GET | `/v1/sessions[?source=]` | recent sessions; sub-agent sessions only with `?source=subagent` |
//! | GET | `/v1/sessions/{id}` | a session and its messages |
//! | PUT | `/v1/sessions/{id}/model` | set `{model}` |
//! | GET | `/v1/sessions/{id}/usage[?include=subagents]` | token usage (with sub-agents': the run trees) |
//! | GET | `/v1/sessions/{id}/subagents` | every sub-agent under the session's runs (`api_subagents.rs`) |
//! | GET | `/v1/sessions/{id}/runs[?include=subagents]` | runs, with the flag their sub-agent runs (`api_runs.rs`) |
//! | GET | `/v1/events` | Server-Sent Events: agent events and approval requests |
//! | GET | `/v1/approvals` | approvals waiting for an API client |
//! | POST | `/v1/approvals/{id}` | answer `{decision: "once" \| "always" \| "deny"}` |
//! | GET / POST | `/v1/secret-requests[/{id}]` | the agent's prompts for secrets (`api_secrets.rs`) |
//!
//! Agent events on `/v1/events` carry `session_id` as well as `run_id`, and
//! events of sub-agent runs also `parent_run_id` and `root_session_id`.
//! Besides the run events (`run_started`, `text_delta`, `tool_started`,
//! `tool_output`, `tool_finished`, …) there are `subagent_spawned` and
//! `subagent_message` (docs/subagents-design.md §3.1). Approvals of a
//! sub-agent's calls carry `parent_run_id`, `agent_label` and
//! `root_session_id`.
//! Schedules are in `api_schedules.rs`; session runs and journal steps in
//! `api_runs.rs`; sub-agent runs and messages in `api_subagents.rs`;
//! secrets in `api_secrets.rs`; providers, model lists and the default model
//! in `api_providers.rs`.
//! Pairing, devices and uploads are in `auth.rs` and `uploads.rs`. TCP
//! listeners (`listen.rs`) serve this same router behind device-token auth.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_core::agent::RunOutcome;
use tern_core::approval::Decision;
use tern_core::types::{ContentBlock, Usage};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use super::manager::{ApiApproval, ApiSecretRequest, Manager};

pub(super) type Shared = Arc<Manager>;

pub(super) struct ApiError(pub(super) StatusCode, pub(super) String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

pub(super) fn bad(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, format!("{e:#}"))
}

pub(super) fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
}

pub(super) fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("{what} not found"))
}

pub(super) type ApiResult<T> = Result<Json<T>, ApiError>;

/// Bind the socket (replacing a stale one) and serve until `shutdown`.
/// `socket` is the advertised path; `bind` is where it really lives. They
/// differ when `socket` is too long to bind, and then `socket` becomes a
/// symlink to `bind`.
pub async fn serve(
    manager: Shared,
    socket: &Path,
    bind: &Path,
    shutdown: CancellationToken,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    // We hold the PID lock, so any existing socket or link is stale.
    for p in [socket, bind] {
        if p.symlink_metadata().is_ok() {
            std::fs::remove_file(p)?;
        }
    }
    if bind != socket {
        prepare_private_dir(bind.parent().unwrap_or(Path::new("/tmp")))?;
    }
    let listener = tokio::net::UnixListener::bind(bind)
        .map_err(|e| anyhow::anyhow!("binding {}: {e}", bind.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bind, std::fs::Permissions::from_mode(0o600))?;
        if bind != socket {
            std::os::unix::fs::symlink(bind, socket)?;
        }
    }
    let app = router(manager);
    Ok(tokio::spawn(async move {
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await;
        if let Err(e) = result {
            tracing::error!(error = %e, "API server stopped");
        }
    }))
}

/// Create (or check) a directory only we can use: mode 0700, owned by us.
fn prepare_private_dir(dir: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        if !dir.exists() {
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
        }
        let meta = std::fs::symlink_metadata(dir)?;
        let ours = tern_core::home::current_uid();
        if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 || Some(meta.uid()) != ours {
            anyhow::bail!(
                "{} must be a directory owned by you with mode 0700",
                dir.display()
            );
        }
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    Ok(())
}

pub fn router(manager: Shared) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/info", get(info))
        .route("/v1/tools", get(tools))
        .route("/v1/models/check", post(check_model))
        .route("/v1/runs", post(start_run))
        .route("/v1/runs/active", get(active_runs))
        .route("/v1/runs/{id}/resume", post(resume_run))
        .route("/v1/runs/{id}/cancel", post(cancel_run))
        .route("/v1/runs/{id}/wait", get(wait_run))
        .route("/v1/runs/{id}/session", get(run_session))
        .route("/v1/sessions", get(sessions))
        .route("/v1/sessions/{id}", get(session))
        .route("/v1/sessions/{id}/model", put(set_model))
        .route("/v1/sessions/{id}/usage", get(usage))
        .route("/v1/events", get(events))
        .route("/v1/approvals", get(pending_approvals))
        .route("/v1/approvals/{id}", post(approve))
        .merge(super::api_schedules::routes())
        .merge(super::api_runs::routes())
        .merge(super::api_subagents::routes())
        .merge(super::api_secrets::routes())
        .merge(super::api_providers::routes())
        .merge(super::auth::routes())
        .with_state(manager)
}

async fn health(auth: Option<axum::Extension<super::auth::Auth>>) -> Json<Value> {
    Json(super::auth::health(auth.as_ref().map(|a| &a.0)))
}

#[derive(Serialize, Deserialize)]
pub struct Info {
    pub default_model: Option<String>,
    /// No usable model yet: clients should offer provider setup.
    #[serde(default)]
    pub setup_needed: bool,
    pub tools: Vec<(String, String)>,
}

async fn info(State(m): State<Shared>) -> Json<Info> {
    Json(Info {
        default_model: m.rt.pick_model(None, None).ok().map(|x| x.to_string()),
        setup_needed: m.rt.setup_needed(),
        tools: m
            .rt
            .agent
            .tools()
            .specs()
            .into_iter()
            .map(|t| (t.name, t.description))
            .collect(),
    })
}

async fn tools(State(m): State<Shared>) -> Json<Vec<tern_core::types::ToolSpec>> {
    Json(m.rt.agent.tools().specs())
}

#[derive(Serialize, Deserialize)]
pub struct ModelBody {
    pub model: String,
}

async fn check_model(State(m): State<Shared>, Json(b): Json<ModelBody>) -> ApiResult<Value> {
    let r = m.rt.resolve_model(&b.model).map_err(bad)?;
    Ok(Json(
        json!({ "ok": true, "resolved": r.model, "warning": r.warning }),
    ))
}

#[derive(Serialize, Deserialize)]
pub struct StartBody {
    pub text: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Started {
    pub session_id: String,
    pub run_id: String,
    pub model: String,
}

async fn start_run(State(m): State<Shared>, Json(b): Json<StartBody>) -> ApiResult<Started> {
    let s = m
        .start(
            b.session_id.as_deref(),
            b.model.as_deref(),
            &b.text,
            "api",
            "user",
            None,
        )
        .await
        .map_err(bad)?;
    Ok(Json(Started {
        session_id: s.session_id,
        run_id: s.run_id,
        model: s.model.to_string(),
    }))
}

#[derive(Serialize, Deserialize)]
pub struct ActiveRun {
    pub run_id: String,
    pub session_id: String,
    /// Set for sub-agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

async fn active_runs(State(m): State<Shared>) -> Json<Vec<ActiveRun>> {
    Json(
        m.active_runs()
            .into_iter()
            .map(|(session_id, run_id)| {
                let row = m.rt.store.run(&run_id).ok().flatten();
                let (parent_run_id, label) =
                    row.map(|r| (r.parent_run_id, r.label)).unwrap_or_default();
                ActiveRun {
                    run_id,
                    session_id,
                    label: label.filter(|_| parent_run_id.is_some()),
                    parent_run_id,
                }
            })
            .collect(),
    )
}

async fn resume_run(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Value> {
    let run =
        m.rt.store
            .run(&id)
            .map_err(internal)?
            .ok_or_else(|| not_found("run"))?;
    if run.status == "completed" {
        return Err(bad(format!("run {id} is already completed")));
    }
    if !m.is_active(&id) {
        m.spawn(&run.session_id, &id, true);
    }
    Ok(Json(json!({ "ok": true })))
}

async fn cancel_run(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> Json<Value> {
    Json(json!({ "cancelled": m.cancel_run(&id) }))
}

/// Long-poll until the run is no longer active, then report how it ended.
async fn wait_run(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<RunOutcome> {
    let mut events = m.rt.agent.subscribe();
    loop {
        let run =
            m.rt.store
                .run(&id)
                .map_err(internal)?
                .ok_or_else(|| not_found("run"))?;
        if run.status != "running" && run.status != "queued" && !m.is_active(&id) {
            let messages =
                m.rt.store
                    .session_messages(&run.session_id)
                    .map_err(internal)?;
            let final_text = messages
                .iter()
                .rev()
                .find(|msg| msg.run_id.as_deref() == Some(id.as_str()) && msg.role == "assistant")
                .and_then(|msg| serde_json::from_str::<Vec<ContentBlock>>(&msg.content_json).ok())
                .map(|blocks| {
                    blocks
                        .into_iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text),
                            _ => None,
                        })
                        .collect::<String>()
                })
                .unwrap_or_default();
            let status =
                serde_json::from_value(Value::String(run.status.clone())).map_err(internal)?;
            return Ok(Json(RunOutcome {
                run_id: id,
                status,
                final_text,
                error: run.error,
                usage: Usage::default(),
            }));
        }
        // Wake on this run finishing, or re-check every second.
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match events.recv().await {
                    Ok(tern_core::events::AgentEvent::RunFinished { run_id, .. })
                        if run_id == id =>
                    {
                        return;
                    }
                    Err(RecvError::Closed) => return,
                    _ => {}
                }
            }
        })
        .await;
    }
}

async fn run_session(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Value> {
    let run =
        m.rt.store
            .run(&id)
            .map_err(internal)?
            .ok_or_else(|| not_found("run"))?;
    Ok(Json(json!({ "session_id": run.session_id })))
}

#[derive(Deserialize)]
struct SessionsQuery {
    limit: Option<usize>,
    /// Only sessions from this source; without it, every source but
    /// `subagent`.
    source: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SessionItem {
    pub id: String,
    pub title: Option<String>,
    pub source: String,
    pub model: Option<String>,
    pub messages: i64,
    pub runs: i64,
    pub updated_at: i64,
}

async fn sessions(
    State(m): State<Shared>,
    Query(q): Query<SessionsQuery>,
) -> ApiResult<Vec<SessionItem>> {
    let list =
        m.rt.store
            .list_sessions_from(
                q.limit.unwrap_or(20).min(200),
                q.source.as_deref().filter(|s| !s.is_empty()),
            )
            .map_err(internal)?;
    Ok(Json(
        list.into_iter()
            .map(|s| SessionItem {
                id: s.session.id,
                title: s.session.title,
                source: s.session.source,
                model: s.session.model,
                messages: s.messages,
                runs: s.runs,
                updated_at: s.session.updated_at,
            })
            .collect(),
    ))
}

#[derive(Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub run_id: Option<String>,
    pub content: Vec<ContentBlock>,
    pub summary: bool,
}

#[derive(Serialize, Deserialize)]
pub struct SessionDetail {
    pub id: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub messages: Vec<SessionMessage>,
}

async fn session(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<SessionDetail> {
    let s =
        m.rt.store
            .session(&id)
            .map_err(internal)?
            .ok_or_else(|| not_found("session"))?;
    let messages =
        m.rt.store
            .session_messages(&id)
            .map_err(internal)?
            .into_iter()
            .map(|row| SessionMessage {
                role: row.role,
                run_id: row.run_id,
                content: serde_json::from_str(&row.content_json).unwrap_or_default(),
                summary: row.summary_of.is_some(),
            })
            .collect();
    Ok(Json(SessionDetail {
        id: s.id,
        title: s.title,
        model: s.model,
        messages,
    }))
}

async fn set_model(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<ModelBody>,
) -> ApiResult<Value> {
    let r = m.rt.resolve_model(&b.model).map_err(bad)?;
    m.rt.store
        .set_session_model(&id, &r.model)
        .map_err(internal)?;
    Ok(Json(
        json!({ "ok": true, "resolved": r.model, "warning": r.warning }),
    ))
}

#[derive(Deserialize)]
pub(super) struct Include {
    /// `subagents` adds sub-agent runs.
    pub(super) include: Option<String>,
}

impl Include {
    pub(super) fn subagents(&self) -> bool {
        self.include
            .as_deref()
            .is_some_and(|i| i.split(',').any(|x| x.trim() == "subagents"))
    }
}

async fn usage(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<Include>,
) -> ApiResult<Usage> {
    let (input_tokens, output_tokens, cache_read_tokens, cache_write_tokens) = if q.subagents() {
        m.rt.store.session_tree_usage(&id)
    } else {
        m.rt.store.session_usage(&id)
    }
    .map_err(internal)?;
    Ok(Json(Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
    }))
}

/// What `/v1/events` sends: agent events as-is, plus approval requests.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApiEvent {
    Approval { approval_requested: ApiApproval },
    SecretRequest { secret_requested: ApiSecretRequest },
    Change(tern_core::models::Change),
    Agent(tern_core::events::AgentEvent),
}

async fn events(State(m): State<Shared>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let agent = m.rt.agent.subscribe();
    let approvals = m.routes.api.subscribe();
    let secrets = m.routes.secrets.subscribe();
    let changes = m.rt.subscribe_changes();
    let stream = futures::stream::unfold(
        (
            agent,
            approvals,
            secrets,
            changes,
            m,
            HashMap::<String, RunPlace>::new(),
        ),
        |(mut agent, mut approvals, mut secrets, mut changes, m, mut sessions)| async move {
            loop {
                let ev = tokio::select! {
                    e = agent.recv() => match e {
                        Ok(e) => ApiEvent::Agent(e),
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return None,
                    },
                    a = approvals.recv() => match a {
                        Ok(a) => ApiEvent::Approval { approval_requested: a },
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return None,
                    },
                    r = secrets.recv() => match r {
                        Ok(r) => ApiEvent::SecretRequest { secret_requested: r },
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return None,
                    },
                    c = changes.recv() => match c {
                        Ok(c) => ApiEvent::Change(c),
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return None,
                    },
                };
                let mut data = serde_json::to_value(&ev).unwrap_or_default();
                if let ApiEvent::Agent(_) = ev {
                    with_session(&m, &mut sessions, &mut data);
                }
                let data = serde_json::to_string(&data).unwrap_or_default();
                return Some((
                    Ok(Event::default().data(data)),
                    (agent, approvals, secrets, changes, m, sessions),
                ));
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Where a run's events belong.
#[derive(Clone)]
struct RunPlace {
    session_id: String,
    /// Sub-agents only: their parent run and the session at the top of
    /// their tree.
    parent: Option<(String, String)>,
}

fn run_place(m: &Shared, run_id: &str) -> Option<RunPlace> {
    let store = &m.rt.store;
    let r = store.run(run_id).ok()??;
    let parent = match r.parent_run_id {
        Some(p) => {
            let root = store.root_run(&p).ok()?;
            let root = store.run(&root).ok()??;
            Some((p, root.session_id))
        }
        None => None,
    };
    Some(RunPlace {
        session_id: r.session_id,
        parent,
    })
}

/// Add `session_id` to an agent event, so a client that attached mid-run
/// can place it, and for a sub-agent's events `parent_run_id` and
/// `root_session_id`. Run lookups are cached per connection.
fn with_session(m: &Shared, cache: &mut HashMap<String, RunPlace>, data: &mut Value) {
    let Some(obj) = data.as_object_mut() else {
        return;
    };
    let Some(run) = obj.get("run_id").and_then(Value::as_str) else {
        return;
    };
    if !cache.contains_key(run) {
        let Some(place) = run_place(m, run) else {
            return;
        };
        if cache.len() > 1000 {
            cache.clear();
        }
        cache.insert(run.to_owned(), place);
    }
    let Some(place) = cache.get(run).cloned() else {
        return;
    };
    obj.entry("session_id")
        .or_insert_with(|| Value::String(place.session_id));
    if let Some((parent, root_session)) = place.parent {
        obj.entry("parent_run_id")
            .or_insert_with(|| Value::String(parent));
        obj.insert("root_session_id".into(), Value::String(root_session));
    }
}

async fn pending_approvals(State(m): State<Shared>) -> Json<Vec<ApiApproval>> {
    Json(m.routes.api.pending())
}

#[derive(Serialize, Deserialize)]
pub struct ApproveBody {
    pub decision: String,
}

async fn approve(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<ApproveBody>,
) -> ApiResult<Value> {
    let decision = match b.decision.as_str() {
        "once" => Decision::AllowOnce,
        "always" => Decision::AlwaysAllow,
        "deny" => Decision::Deny,
        other => {
            return Err(bad(format!(
                "decision {other:?} must be once, always, or deny"
            )));
        }
    };
    if m.routes.api.answer(&id, decision) {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(not_found("pending approval"))
    }
}
