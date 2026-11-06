//! The daemon's local API: JSON over HTTP on a Unix socket (mode 0600).
//! Used by `tern` to attach the TUI, and by future clients.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/health` | pid, version, and capabilities (see `auth::health`) |
//! | GET | `/v1/info` | default model and tools |
//! | GET | `/v1/tools` | full tool specs: `[{name, description, input_schema, effect, untrusted}]` |
//! | POST | `/v1/models/check` | validate `{model}` |
//! | POST | `/v1/runs` | send a message: `{text, session_id?, model?}`; `channel` and `branched_from` (side chats) are refused: channels take `POST /v1/channels/{id}/posts` |
//! | GET | `/v1/runs/active` | runs in progress: `[{run_id, session_id, parent_run_id?, label?, channel_id?, author_id?}]` |
//! | POST | `/v1/runs/{id}/resume` | resume an interrupted run |
//! | POST | `/v1/runs/{id}/cancel` | cancel a run and its sub-agents |
//! | GET | `/v1/runs/{id}/wait` | block until the run finishes |
//! | GET | `/v1/runs/{id}/session` | the run's session id |
//! | GET | `/v1/runs/{id}/children[?recursive=1]` | its sub-agents: `[ChildRun]` (`api_subagents.rs`) |
//! | POST | `/v1/runs/{id}/message` | `{text}` from the user to a running sub-agent (`api_subagents.rs`) |
//! | GET | `/v1/sessions[?source=][&channel=]` | recent sessions; sub-agent sessions only with `?source=subagent`; `channel`: that channel's threads |
//! | GET | `/v1/sessions/{id}[?limit=][&before=]` | a session and its messages; `limit` keeps the newest that many (before index `before`, default the end), and `total` and `offset` say where the page sits |
//! | PUT | `/v1/sessions/{id}/model` | set `{model}` |
//! | GET | `/v1/sessions/{id}/usage[?include=subagents]` | token usage (with sub-agents': the run trees) and how full the context is |
//! | GET | `/v1/sessions/{id}/subagents` | every sub-agent under the session's runs (`api_subagents.rs`) |
//! | GET | `/v1/sessions/{id}/runs[?include=subagents]` | runs, with the flag their sub-agent runs (`api_runs.rs`) |
//! | GET | `/v1/events` | Server-Sent Events: agent events and approval requests |
//! | GET | `/v1/approvals` | approvals waiting for an API client |
//! | POST | `/v1/approvals/{id}` | answer `{decision: "once" \| "always" \| "deny"}` |
//! | GET / POST | `/v1/outbox[/{id}[/approve\|/reject]]` | deferred approvals (`api_outbox.rs`) |
//! | GET / POST | `/v1/secret-requests[/{id}]` | the agent's prompts for secrets (`api_secrets.rs`) |
//! | GET | `/v1/status` | version, uptime, counts, gateway health (`api_lifecycle.rs`) |
//! | POST | `/v1/shutdown[?dry_run=1]` | stop with exit code 0 (`api_lifecycle.rs`) |
//! | GET / POST | `/v1/mcp-serve/{tools,call}` | what `tern mcp serve` offers, and its calls (`api_mcp_serve.rs`) |
//!
//! Agent events on `/v1/events` carry `session_id` as well as `run_id`, and
//! events of sub-agent runs also `parent_run_id` and `root_session_id`.
//! Besides the run events (`run_started`, `text_delta`, `tool_started`,
//! `tool_output`, `tool_finished`, …) there are `subagent_spawned` and
//! `subagent_message` (docs/subagents-design.md §3.1). Approvals of a
//! sub-agent's calls carry `parent_run_id`, `agent_label` and
//! `root_session_id`.
//! Agent events also carry `channel_id`, the channel of the run's session,
//! and `agent_id`, its agent (`tern` by default). Agent profiles are in
//! `api_agents.rs`.
//! Channels and posts are in `api_channels.rs`; schedules in `api_schedules.rs`; session runs and journal steps in
//! `api_runs.rs`; sub-agent runs and messages in `api_subagents.rs`;
//! secrets in `api_secrets.rs`; providers, model lists and the default model
//! in `api_providers.rs`; MCP servers in `api_mcp.rs`; the config editor's
//! settings in `api_config.rs`.
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
use tern_core::config::ModelRef;
use tern_core::types::{ContentBlock, Usage};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use super::manager::{ApiApproval, ApiAuthPrompt, ApiSecretRequest, AuthPromptEvent, Manager};

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
    let listener = bind_listening(bind)?;
    #[cfg(unix)]
    if bind != socket {
        std::os::unix::fs::symlink(bind, socket)?;
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

/// Bind a unix socket so the path only exists once it is listening. A plain
/// `bind` creates the file before `listen`, so a client that waits for the
/// file and connects at once can be refused. Bind a sibling name, set the
/// mode, then rename it into place.
fn bind_listening(path: &Path) -> anyhow::Result<tokio::net::UnixListener> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".new");
        let tmp = std::path::PathBuf::from(tmp);
        // The extra suffix could push a path at the length limit over it.
        let staged = tmp.as_os_str().len() <= tern_core::home::MAX_SOCKET_PATH;
        let at = if staged { tmp.as_path() } else { path };
        if staged && at.symlink_metadata().is_ok() {
            std::fs::remove_file(at)?;
        }
        let listener = tokio::net::UnixListener::bind(at)
            .map_err(|e| anyhow::anyhow!("binding {}: {e}", at.display()))?;
        std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o600))?;
        if staged {
            std::fs::rename(at, path)?;
        }
        Ok(listener)
    }
    #[cfg(not(unix))]
    {
        tokio::net::UnixListener::bind(path)
            .map_err(|e| anyhow::anyhow!("binding {}: {e}", path.display()))
    }
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
        .merge(super::api_channels::routes())
        .merge(super::suggestions::routes())
        .merge(super::api_agents::routes())
        .merge(super::api_workspaces::routes())
        .merge(super::api_config::routes())
        .merge(super::api_schedules::routes())
        .merge(super::api_webhooks::routes())
        .merge(super::api_outbox::routes())
        .merge(super::api_ledger::routes())
        .merge(super::api_tasks::routes())
        .merge(super::api_skills::routes())
        .merge(super::api_runs::routes())
        .merge(super::api_subagents::routes())
        .merge(super::api_secrets::routes())
        .merge(super::api_connections::routes())
        .merge(super::api_bundles::routes())
        .merge(super::api_providers::routes())
        .merge(super::api_mcp::routes())
        .merge(super::api_mcp_serve::routes())
        .merge(super::api_lifecycle::routes())
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
    /// Gateways routines can deliver to (`<name>:<chat>`), Telegram and
    /// gateway connectors alike, by name.
    #[serde(default)]
    pub gateways: Vec<InfoGateway>,
    /// Whether the OS sandbox is confirmed active for shell commands. Script
    /// grants ("allow this python script for this run") apply only then.
    #[serde(default)]
    pub sandbox: Option<InfoSandbox>,
}

#[derive(Serialize, Deserialize)]
pub struct InfoSandbox {
    pub verified: bool,
    /// One line: what confines commands, or why that isn't confirmed.
    pub detail: String,
}

#[derive(Serialize, Deserialize)]
pub struct InfoGateway {
    pub name: String,
    /// `telegram`, `ntfy`, `slack`, … (a connector's `gateway.kind`).
    pub kind: String,
}

async fn info(State(m): State<Shared>) -> Json<Info> {
    let mut gateways: Vec<InfoGateway> = m
        .routes
        .gateways
        .get()
        .map(|g| {
            g.values()
                .map(|g| InfoGateway {
                    name: g.name().to_owned(),
                    kind: g.kind().to_owned(),
                })
                .collect()
        })
        .unwrap_or_default();
    gateways.sort_by(|a, b| a.name.cmp(&b.name));
    let sandbox = {
        let (config, home) = (m.rt.config.clone(), m.rt.home.clone());
        tokio::task::spawn_blocking(move || crate::runtime::shell_sandbox_status(&config, &home))
            .await
            .ok()
            .map(|v| InfoSandbox {
                verified: v.is_verified(),
                detail: v.describe(),
            })
    };
    Json(Info {
        sandbox,
        gateways,
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

/// A tool spec plus its declared effect (`read` / `write` / `destructive`)
/// and whether it returns third-party text.
#[derive(Serialize, Deserialize)]
pub struct ApiTool {
    #[serde(flatten)]
    pub spec: tern_core::types::ToolSpec,
    pub effect: tern_core::tool::Effect,
    pub untrusted: bool,
}

async fn tools(State(m): State<Shared>) -> Json<Vec<ApiTool>> {
    let reg = m.rt.agent.tools();
    let effects = reg.effects();
    Json(
        reg.specs()
            .into_iter()
            .map(|spec| {
                let (effect, untrusted) = effects.get(&spec.name).copied().unwrap_or_default();
                ApiTool {
                    spec,
                    effect,
                    untrusted,
                }
            })
            .collect(),
    )
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
    /// Refused: threads beside a channel's conversation were removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Refused: side chats were removed. Old side chats keep the column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branched_from: Option<String>,
    /// The folder for this conversation (a workspace id or a folder path),
    /// bound before the run starts, so the very first run already uses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Started {
    pub session_id: String,
    pub run_id: String,
    pub model: String,
}

async fn start_run(State(m): State<Shared>, Json(b): Json<StartBody>) -> ApiResult<Started> {
    // Side chats and new threads beside a stream were removed: a
    // conversation in a channel goes through the channel (DECISIONS.md
    // "Flat conversations everywhere").
    if b.branched_from.is_some() {
        return Err(bad(
            "side chats were removed: send in the stream (POST /v1/channels/{id}/posts)",
        ));
    }
    if b.channel.is_some() {
        return Err(bad(
            "starting a thread in a channel was removed: use POST /v1/channels/{id}/posts",
        ));
    }
    let mut thread = b.session_id.clone();
    if let Some(w) = b.workspace.as_deref() {
        let ws = m.rt.resolve_ws_ref(w).map_err(bad)?;
        let id = match thread.clone() {
            Some(id) => id,
            None => {
                m.rt.store
                    .create_session(&tern_core::agent::new_id(), "api", None)
                    .map_err(internal)?
                    .id
            }
        };
        m.rt.bind_workspace(tern_store::Bind::Session(&id), Some(&ws))
            .map_err(bad)?;
        thread = Some(id);
    }
    let s = m
        .start(
            thread.as_deref(),
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
    /// The channel of its session, for member lines (docs/channels-design.md §5.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// Who is working, as posts name them: the agent's id (`tern`,
    /// `tern:research`), a sub-agent's `<agent id>/<label>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
}

async fn active_runs(State(m): State<Shared>) -> Json<Vec<ActiveRun>> {
    Json(
        m.active_runs()
            .into_iter()
            .map(|(session_id, run_id)| {
                let row = m.rt.store.run(&run_id).ok().flatten();
                let (parent_run_id, label) =
                    row.map(|r| (r.parent_run_id, r.label)).unwrap_or_default();
                let session = m.rt.store.session(&session_id).ok().flatten();
                let agent = session
                    .as_ref()
                    .map_or(tern_store::DEFAULT_AGENT, |s| s.agent())
                    .to_owned();
                let channel_id = session.and_then(|s| s.channel_id);
                let label = label.filter(|_| parent_run_id.is_some());
                let author_id = Some(match &label {
                    Some(l) => format!("{agent}/{l}"),
                    None => agent,
                });
                ActiveRun {
                    run_id,
                    session_id,
                    label,
                    parent_run_id,
                    channel_id,
                    author_id,
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
            let final_text = super::channels::final_text(&m.rt.store, &run).map_err(internal)?;
            let status =
                serde_json::from_value(Value::String(run.status.clone())).map_err(internal)?;
            return Ok(Json(RunOutcome {
                run_id: id,
                status,
                final_text,
                error: run.error,
                usage: Usage::default(),
                quiet: run.quiet,
                tool_result: None,
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
    /// Only threads of this channel (slug or id): for Home, its History.
    channel: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// A segment of a stream: the one it continues, and why it was cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_reason: Option<String>,
    /// A side thread: `message:<id>` or `post:<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branched_from: Option<String>,
    /// Moved here from this channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
    /// Its agent, when not the default one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

impl SessionItem {
    pub(super) fn new(s: tern_store::SessionRow, messages: i64, runs: i64) -> Self {
        Self {
            id: s.id,
            title: s.title,
            source: s.source,
            model: s.model,
            messages,
            runs,
            updated_at: s.updated_at,
            channel_id: s.channel_id,
            prev_session_id: s.prev_session_id,
            segment_reason: s.segment_reason,
            branched_from: s.branched_from,
            moved_from: s.moved_from,
            agent_id: s.agent_id,
        }
    }
}

async fn sessions(
    State(m): State<Shared>,
    Query(q): Query<SessionsQuery>,
) -> ApiResult<Vec<SessionItem>> {
    let limit = q.limit.unwrap_or(20).min(200);
    if let Some(c) = q.channel.as_deref().filter(|c| !c.is_empty()) {
        let c = super::api_channels::find(&m, c)?;
        let rows =
            m.rt.store
                .channel_sessions(&c.id, limit)
                .map_err(internal)?;
        let mut out = Vec::with_capacity(rows.len());
        for s in rows {
            let (messages, runs) = m.rt.store.session_counts(&s.id).map_err(internal)?;
            out.push(SessionItem::new(s, messages, runs));
        }
        return Ok(Json(out));
    }
    let list =
        m.rt.store
            .list_sessions_from(limit, q.source.as_deref().filter(|s| !s.is_empty()))
            .map_err(internal)?;
    Ok(Json(
        list.into_iter()
            .map(|s| SessionItem::new(s.session, s.messages, s.runs))
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
    /// How many messages the session has in all; an older daemon's reply
    /// has none.
    #[serde(default)]
    pub total: usize,
    /// The index of `messages[0]` among them; 0 for the whole session.
    #[serde(default)]
    pub offset: usize,
}

#[derive(Deserialize)]
struct SessionPage {
    /// At most this many messages (the newest before `before`). Absent:
    /// all of them.
    limit: Option<usize>,
    /// Page back from this message index (exclusive); absent: from the end.
    before: Option<usize>,
}

async fn session(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(page): Query<SessionPage>,
) -> ApiResult<SessionDetail> {
    let s =
        m.rt.store
            .session(&id)
            .map_err(internal)?
            .ok_or_else(|| not_found("session"))?;
    let messages = m.rt.store.session_messages(&id).map_err(internal)?;
    let total = messages.len();
    let end = page.before.map_or(total, |b| b.min(total));
    let offset = page.limit.map_or(0, |l| end.saturating_sub(l.max(1)));
    let show_reasoning = m.rt.live().show_reasoning;
    let messages = messages
        .into_iter()
        .skip(offset)
        .take(end - offset)
        .map(|row| SessionMessage {
            role: row.role,
            run_id: row.run_id,
            content: shown_blocks(&row.content_json, show_reasoning),
            summary: row.summary_of.is_some(),
        })
        .collect();
    Ok(Json(SessionDetail {
        id: s.id,
        title: s.title,
        model: s.model,
        messages,
        total,
        offset,
    }))
}

/// A stored message's blocks for a front end: reasoning only when
/// `show_reasoning` is on (the journal keeps it either way, for echoing).
pub(super) fn shown_blocks(content_json: &str, show_reasoning: bool) -> Vec<ContentBlock> {
    let mut blocks: Vec<ContentBlock> = serde_json::from_str(content_json).unwrap_or_default();
    if !show_reasoning {
        blocks.retain(|b| !b.is_thinking());
    }
    blocks
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

/// A session's token totals, plus how full its context was at the latest
/// model call (absent before the first).
#[derive(Serialize, Deserialize)]
pub struct SessionUsage {
    #[serde(flatten)]
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Where the agent starts summarizing older messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_at: Option<u64>,
}

async fn usage(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<Include>,
) -> ApiResult<SessionUsage> {
    let (input_tokens, output_tokens, cache_read_tokens, cache_write_tokens) = if q.subagents() {
        m.rt.store.session_tree_usage(&id)
    } else {
        m.rt.store.session_usage(&id)
    }
    .map_err(internal)?;
    let context = m.rt.store.session_context(&id).map_err(internal)?;
    let window = context
        .as_ref()
        .and_then(|(model, _)| model.as_deref()?.parse::<ModelRef>().ok())
        .map(|model| m.rt.agent.context_window(&model.provider));
    Ok(Json(SessionUsage {
        usage: Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
        },
        context_tokens: context.map(|(_, tokens)| tokens),
        context_window: window,
        compact_at: window.map(|w| (w as f64 * tern_core::compaction::THRESHOLD) as u64),
    }))
}

/// What `/v1/events` sends: agent events as-is, plus approval requests.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApiEvent {
    Approval {
        approval_requested: ApiApproval,
    },
    SecretRequest {
        secret_requested: ApiSecretRequest,
    },
    AuthPrompt {
        auth_prompt: Box<ApiAuthPrompt>,
    },
    AuthPromptClosed {
        auth_prompt_closed: String,
    },
    Change(tern_core::models::Change),
    Agent(tern_core::events::AgentEvent),
    /// This connection fell behind and `missed` events were skipped (a
    /// `run_finished` may be among them): the client should re-read what
    /// it keeps, such as `/v1/runs/active`.
    Dropped {
        events_dropped: EventsDropped,
    },
}

#[derive(Serialize, Deserialize)]
pub struct EventsDropped {
    pub missed: u64,
}

fn dropped(missed: u64) -> ApiEvent {
    tracing::warn!(
        missed,
        "an event stream client fell behind; told it to resync"
    );
    ApiEvent::Dropped {
        events_dropped: EventsDropped { missed },
    }
}

async fn events(State(m): State<Shared>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let agent = m.rt.agent.subscribe();
    let approvals = m.routes.api.subscribe();
    let secrets = m.routes.secrets.subscribe();
    let auth = m.routes.auth.subscribe();
    let changes = m.rt.subscribe_changes();
    let stream = futures::stream::unfold(
        (
            agent,
            approvals,
            (secrets, auth),
            changes,
            m,
            HashMap::<String, RunPlace>::new(),
        ),
        |(mut agent, mut approvals, (mut secrets, mut auth), mut changes, m, mut sessions)| async move {
            let ev = tokio::select! {
                e = agent.recv() => match e {
                    Ok(e) => ApiEvent::Agent(e),
                    Err(RecvError::Lagged(n)) => dropped(n),
                    Err(RecvError::Closed) => return None,
                },
                a = approvals.recv() => match a {
                    Ok(a) => ApiEvent::Approval { approval_requested: a },
                    Err(RecvError::Lagged(n)) => dropped(n),
                    Err(RecvError::Closed) => return None,
                },
                r = secrets.recv() => match r {
                    Ok(r) => ApiEvent::SecretRequest { secret_requested: r },
                    Err(RecvError::Lagged(n)) => dropped(n),
                    Err(RecvError::Closed) => return None,
                },
                a = auth.recv() => match a {
                    Ok(AuthPromptEvent::Open(p)) => ApiEvent::AuthPrompt { auth_prompt: p },
                    Ok(AuthPromptEvent::Closed(id)) => ApiEvent::AuthPromptClosed {
                        auth_prompt_closed: id,
                    },
                    Err(RecvError::Lagged(n)) => dropped(n),
                    Err(RecvError::Closed) => return None,
                },
                c = changes.recv() => match c {
                    Ok(c) => ApiEvent::Change(c),
                    Err(RecvError::Lagged(n)) => dropped(n),
                    Err(RecvError::Closed) => return None,
                },
            };
            let mut data = serde_json::to_value(&ev).unwrap_or_default();
            if let ApiEvent::Agent(_) = ev {
                with_session(&m, &mut sessions, &mut data);
            }
            let data = serde_json::to_string(&data).unwrap_or_default();
            Some((
                Ok(Event::default().data(data)),
                (agent, approvals, (secrets, auth), changes, m, sessions),
            ))
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Where a run's events belong.
#[derive(Clone)]
struct RunPlace {
    session_id: String,
    channel_id: Option<String>,
    /// The session's agent (agent profiles §7.1).
    agent_id: String,
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
    let session = store.session(&r.session_id).ok()??;
    Some(RunPlace {
        agent_id: session.agent().to_owned(),
        channel_id: session.channel_id,
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
    if let Some(c) = place.channel_id {
        obj.entry("channel_id").or_insert_with(|| Value::String(c));
    }
    obj.entry("agent_id")
        .or_insert_with(|| Value::String(place.agent_id));
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
    auth: Option<axum::Extension<super::auth::Auth>>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<ApproveBody>,
) -> ApiResult<Value> {
    if auth.is_some() && m.routes.api.is_local_only(&id) {
        return Err(ApiError(
            axum::http::StatusCode::FORBIDDEN,
            "this approval runs a command on the host outside the sandbox; answer it in the \
             TUI or desktop app on that machine"
                .into(),
        ));
    }
    let decision = match b.decision.as_str() {
        "once" => Decision::AllowOnce,
        "always" => Decision::AlwaysAllow,
        "run" => Decision::AllowRun,
        "session" => Decision::AllowSession,
        "host" => Decision::AlwaysAllowHost,
        "deny" => Decision::Deny,
        other => {
            return Err(bad(format!(
                "decision {other:?} must be once, always, run, session, host, or deny"
            )));
        }
    };
    if m.routes.api.answer(&id, decision, auth.is_some()) {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(not_found("pending approval"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client that connects the moment the socket path appears is never
    /// refused: the path only shows up once the socket is listening.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_socket_path_appears_only_when_listening() {
        for i in 0..200 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.sock");
            let p = path.clone();
            let client = std::thread::spawn(move || {
                let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while !p.exists() {
                    assert!(std::time::Instant::now() < end, "socket never appeared");
                }
                std::os::unix::net::UnixStream::connect(&p)
            });
            let _listener = bind_listening(&path).unwrap();
            let r = client.join().unwrap();
            assert!(r.is_ok(), "round {i}: {:?}", r.err());
        }
    }

    #[test]
    fn a_lagging_client_is_told_to_resync() {
        let v = serde_json::to_value(dropped(40)).unwrap_or_default();
        assert_eq!(v, json!({ "events_dropped": { "missed": 40 } }));
    }
}
