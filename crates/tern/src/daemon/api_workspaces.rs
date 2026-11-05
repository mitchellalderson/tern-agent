//! Workspaces: the folder an agent can reach (docs/alpha-workspaces.md §4).
//! Owner scope, local front ends. `{id}` is a workspace id (`default`,
//! `ws_...`). A `{ref}` in a body is an id or an absolute folder path (a new
//! folder is validated and added). The agent has no tool that touches any of
//! this.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/workspaces[?archived=1]` | `[Workspace]`, the default first |
//! | POST | `/v1/workspaces` | add `{path, name?}` → 201 `{workspace, verdict}`; a folder Tern refuses → 422 `{error, verdict}`; one already added is returned (200) |
//! | POST | `/v1/workspaces/validate` | `{path, kind?: "workspace" \| "read_root"}` → `Verdict` (always 200, persists nothing): `canonical`, `shortcut_of?`, `kind`, `errors[]`, `warnings[]`, `suggested_root?`, `flags`, `denied` |
//! | GET | `/v1/workspaces/{id}` | `Workspace` (with `shell_state` checked now) |
//! | PATCH | `/v1/workspaces/{id}` | `{name?, network?: "off" \| null, locate?: <path>}`; `locate` points a missing folder at a new place (same folder: silently; another folder needs `confirm: true`) |
//! | POST | `/v1/workspaces/{id}/check` | re-check it now → `Workspace` with its status |
//! | DELETE | `/v1/workspaces/{id}[?reassign_to=<id>]` | archive (never the default); 409 `{error, impact}` while channels, topics, agents or routines use it unless `reassign_to`; files on disk are never touched |
//! | GET / POST | `/v1/workspaces/{id}/roots` | read-only extra folders (or single files): `[Root]`; POST `{path}` → 201 `{root, verdict}` |
//! | DELETE | `/v1/workspaces/{id}/roots?path=` | remove one |
//! | GET / PUT / DELETE | `/v1/channels/{id}/workspace`, `/v1/sessions/{id}/workspace`, `/v1/agents/{id}/workspace` | read, set `{workspace: <ref> \| null}` or clear the binding → `Binding {workspace, effective, source}`. A channel's change applies from its next run; Home's topic change starts a new topic (`POST /v1/channels/home/posts` with `workspace`) |
//!
//! `Workspace.shell_state` is what the header chip shows: `sandboxed`,
//! `read_only`, `unverified` or `unbounded`. Every change sends
//! `workspaces_changed`.

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_core::workspace::FsFlags;
use tern_core::workspace::validate::{Verdict, validate_read_root, validate_workspace};
use tern_store::{Bind, DEFAULT_WORKSPACE, WorkspaceRow};

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};
use crate::workspaces::{Refused, ShellState, WorkspaceError};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/workspaces", get(list).post(add))
        .route("/v1/workspaces/validate", post(validate))
        .route(
            "/v1/workspaces/{id}",
            get(one).patch(update).delete(archive),
        )
        .route("/v1/workspaces/{id}/check", post(check))
        .route(
            "/v1/workspaces/{id}/roots",
            get(roots).post(add_root).delete(remove_root),
        )
        .route(
            "/v1/channels/{id}/workspace",
            get(get_channel_binding)
                .put(put_channel_binding)
                .delete(clear_channel_binding),
        )
        .route(
            "/v1/sessions/{id}/workspace",
            get(get_session_binding)
                .put(put_session_binding)
                .delete(clear_session_binding),
        )
        .route(
            "/v1/agents/{id}/workspace",
            get(get_agent_binding)
                .put(put_agent_binding)
                .delete(clear_agent_binding),
        )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Root {
    pub path: String,
    /// `user` or `git_dir`.
    pub kind: String,
    pub is_file: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsedBy {
    pub channels: Vec<String>,
    pub sessions: u32,
    pub agents: Vec<String>,
    pub routines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub path: String,
    pub is_default: bool,
    /// `ok`, `missing`, `replaced`, `denied` or `invalid`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_detail: Option<String>,
    /// `off` when this folder is stricter than the global setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    pub created_by: String,
    pub archived: bool,
    pub writable: bool,
    pub flags: FsFlags,
    /// `sandboxed`, `read_only`, `unverified`, `unbounded`.
    pub shell_state: ShellState,
    pub roots: Vec<Root>,
    pub used_by: UsedBy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    /// The folder this one is bound to itself; `None` inherits.
    pub workspace: Option<Workspace>,
    /// The folder a run would get now.
    pub effective: Workspace,
    /// `session`, `channel`, `agent` or `default`.
    pub source: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Added {
    pub workspace: Workspace,
    pub verdict: Verdict,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RootAdded {
    pub root: Root,
    pub verdict: Verdict,
}

fn refused(r: &Refused) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "error": r.to_string(), "verdict": r.0 })),
    )
        .into_response()
}

/// An error that may be a refused folder (422) or an unavailable one (409).
fn fail(e: anyhow::Error) -> Response {
    if let Some(r) = e.downcast_ref::<Refused>() {
        return refused(r);
    }
    if let Some(w) = e.downcast_ref::<WorkspaceError>() {
        return ApiError(StatusCode::CONFLICT, w.to_string()).into_response();
    }
    bad(e).into_response()
}

pub(super) async fn view(m: &Shared, row: WorkspaceRow) -> Result<Workspace, ApiError> {
    let rt = m.rt.clone();
    tokio::task::spawn_blocking(move || view_blocking(&rt, row, true))
        .await
        .map_err(internal)?
}

fn view_blocking(
    rt: &crate::runtime::Runtime,
    row: WorkspaceRow,
    block: bool,
) -> Result<Workspace, ApiError> {
    let roots = rt
        .store
        .workspace_roots(&row.id)
        .map_err(internal)?
        .into_iter()
        .map(|r| Root {
            path: r.path,
            kind: r.kind,
            is_file: r.is_file,
        })
        .collect();
    let used = rt.store.workspace_use(&row.id).map_err(internal)?;
    let path = std::path::Path::new(&row.path);
    let writable = std::fs::metadata(path).is_ok_and(|m| m.is_dir() && !m.permissions().readonly());
    let shell_state = if row.status == "ok" {
        rt.shell_state(path, writable, block)
    } else {
        ShellState::Unbounded
    };
    Ok(Workspace {
        is_default: row.id == DEFAULT_WORKSPACE,
        flags: row
            .fs_flags
            .as_deref()
            .and_then(|f| serde_json::from_str(f).ok())
            .unwrap_or_default(),
        id: row.id,
        name: row.name,
        path: row.path,
        status: row.status,
        status_detail: row.status_detail,
        network: row.network,
        created_by: row.created_by,
        archived: row.archived_at.is_some(),
        writable,
        shell_state,
        roots,
        used_by: UsedBy {
            channels: used.channels,
            sessions: used.sessions,
            agents: used.agents,
            routines: used.schedules,
        },
    })
}

#[derive(Deserialize)]
struct ListQuery {
    archived: Option<String>,
}

async fn list(State(m): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Vec<Workspace>> {
    let all = matches!(q.archived.as_deref(), Some("1" | "true"));
    let rows = m.rt.store.workspaces(all).map_err(internal)?;
    let rt = m.rt.clone();
    // The sandbox check is not waited for here: a workspace list is read often.
    let out = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|r| view_blocking(&rt, r, false))
            .collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(internal)??;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct AddBody {
    path: String,
    name: Option<String>,
}

async fn add(State(m): State<Shared>, Json(b): Json<AddBody>) -> Response {
    let existing = validate_workspace(&b.path, &m.rt.validate_ctx())
        .canonical
        .and_then(|c| m.rt.store.workspace_by_path(&c).ok().flatten());
    let (row, verdict) = match m.rt.add_workspace(&b.path, b.name.as_deref()) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let code = if existing.is_some() {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    match view(&m, row).await {
        Ok(workspace) => (code, Json(Added { workspace, verdict })).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct ValidateBody {
    path: String,
    kind: Option<String>,
}

/// Judge a folder without saving anything: the words for the confirm sheet.
async fn validate(State(m): State<Shared>, Json(b): Json<ValidateBody>) -> ApiResult<Verdict> {
    let ctx = m.rt.validate_ctx();
    let read_root = b.kind.as_deref() == Some("read_root");
    let v = tokio::task::spawn_blocking(move || {
        if read_root {
            validate_read_root(&b.path, &ctx)
        } else {
            validate_workspace(&b.path, &ctx)
        }
    })
    .await
    .map_err(internal)?;
    Ok(Json(v))
}

fn find(m: &Shared, id: &str) -> Result<WorkspaceRow, ApiError> {
    m.rt.store
        .workspace(id)
        .map_err(internal)?
        .ok_or_else(|| not_found("workspace"))
}

async fn one(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Workspace> {
    Ok(Json(view(&m, find(&m, &id)?).await?))
}

#[derive(Deserialize)]
struct UpdateBody {
    name: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    network: Option<Option<String>>,
    /// Point it at the folder's new place.
    locate: Option<String>,
    #[serde(default)]
    confirm: bool,
}

fn double_option<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

async fn update(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<UpdateBody>,
) -> Response {
    let row = match find(&m, &id) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    if let Some(name) = b.name.as_deref() {
        let name = name.trim();
        if name.is_empty() {
            return bad("the name is empty").into_response();
        }
        if let Err(e) = m.rt.store.rename_workspace(&id, name) {
            return internal(e).into_response();
        }
    }
    if let Some(net) = &b.network {
        match net.as_deref() {
            None | Some("off") => {
                if let Err(e) = m.rt.store.set_workspace_network(&id, net.as_deref()) {
                    return internal(e).into_response();
                }
            }
            Some(_) => {
                return bad(
                    "network can only be \"off\" (stricter than the global setting) or null",
                )
                .into_response();
            }
        }
    }
    if let Some(path) = b.locate.as_deref() {
        if id == DEFAULT_WORKSPACE {
            return bad("the default folder is set in config.toml (workspace = ...)")
                .into_response();
        }
        let v = validate_workspace(path, &m.rt.validate_ctx());
        if !v.is_ok() {
            return refused(&Refused(v));
        }
        let same = row.dev.is_some() && v.dev == row.dev && v.ino == row.ino;
        if !same && !b.confirm {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "this isn't the folder it was; the conversation's file paths may not match",
                    "needs_confirm": true,
                    "verdict": v,
                })),
            )
                .into_response();
        }
        if let Some(other) =
            m.rt.store
                .workspace_by_path(v.canonical.as_deref().unwrap_or_default())
                .ok()
                .flatten()
            && other.id != id
        {
            return ApiError(
                StatusCode::CONFLICT,
                format!("{} is already the folder {}", other.path, other.name),
            )
            .into_response();
        }
        if let Err(e) = m.rt.store.set_workspace_path(
            &id,
            v.canonical.as_deref().unwrap_or_default(),
            v.dev,
            v.ino,
        ) {
            return internal(e).into_response();
        }
    }
    let _ =
        m.rt.changes
            .send(tern_core::models::Change::WorkspacesChanged {});
    match view(&m, find(&m, &id).unwrap_or(row)).await {
        Ok(w) => Json(w).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn check(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    if let Err(e) = find(&m, &id) {
        return e.into_response();
    }
    let rt = m.rt.clone();
    let id2 = id.clone();
    // The result is the row's new status, so an unusable folder is not an error here.
    let _ = tokio::task::spawn_blocking(move || rt.check_workspace(&id2)).await;
    match find(&m, &id) {
        Ok(row) => match view(&m, row).await {
            Ok(w) => Json(w).into_response(),
            Err(e) => e.into_response(),
        },
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct ArchiveQuery {
    reassign_to: Option<String>,
}

async fn archive(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<ArchiveQuery>,
) -> Response {
    if let Err(e) = find(&m, &id) {
        return e.into_response();
    }
    if id == DEFAULT_WORKSPACE {
        return bad("the default workspace can't be removed").into_response();
    }
    let used = match m.rt.store.workspace_use(&id) {
        Ok(u) => u,
        Err(e) => return internal(e).into_response(),
    };
    if !used.is_empty() {
        match q.reassign_to.as_deref() {
            Some(to) => {
                let target = match m.rt.resolve_ws_ref(to) {
                    Ok(t) if t.id != id => t,
                    Ok(_) => return bad("can't reassign a folder to itself").into_response(),
                    Err(e) => return fail(e),
                };
                if let Err(e) = m.rt.store.reassign_workspace(&id, &target.id) {
                    return internal(e).into_response();
                }
            }
            None => {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": "this folder is in use; pass reassign_to to move what uses it",
                        "impact": UsedBy {
                            channels: used.channels,
                            sessions: used.sessions,
                            agents: used.agents,
                            routines: used.schedules,
                        },
                    })),
                )
                    .into_response();
            }
        }
    }
    match m.rt.store.archive_workspace(&id) {
        Ok(_) => {
            let _ =
                m.rt.changes
                    .send(tern_core::models::Change::WorkspacesChanged {});
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

async fn roots(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Vec<Root>> {
    find(&m, &id)?;
    let rows = m.rt.store.workspace_roots(&id).map_err(internal)?;
    Ok(Json(
        rows.into_iter()
            .map(|r| Root {
                path: r.path,
                kind: r.kind,
                is_file: r.is_file,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct RootBody {
    path: String,
}

async fn add_root(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<RootBody>,
) -> Response {
    if let Err(e) = find(&m, &id) {
        return e.into_response();
    }
    match m.rt.add_read_root(&id, &b.path) {
        Ok((r, verdict)) => (
            StatusCode::CREATED,
            Json(RootAdded {
                root: Root {
                    path: r.path,
                    kind: r.kind,
                    is_file: r.is_file,
                },
                verdict,
            }),
        )
            .into_response(),
        Err(e) => fail(e),
    }
}

#[derive(Deserialize)]
struct RootQuery {
    path: String,
}

async fn remove_root(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<RootQuery>,
) -> ApiResult<Value> {
    find(&m, &id)?;
    // Stored canonical; accept the path as the user would type it too.
    let canon = std::fs::canonicalize(&q.path)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_else(|| q.path.clone());
    let removed =
        m.rt.store
            .remove_workspace_root(&id, &canon)
            .map_err(internal)?
            || m.rt
                .store
                .remove_workspace_root(&id, &q.path)
                .map_err(internal)?;
    if !removed {
        return Err(not_found("read-only folder"));
    }
    let _ =
        m.rt.changes
            .send(tern_core::models::Change::WorkspacesChanged {});
    Ok(Json(json!({ "ok": true })))
}

// ---- bindings ----

#[derive(Deserialize)]
struct BindBody {
    /// A workspace id or a folder path; `null` clears.
    workspace: Option<String>,
}

enum Target {
    Session(String),
    Channel(String),
    Agent(String),
}

impl Target {
    fn bind(&self) -> Bind<'_> {
        match self {
            Target::Session(i) => Bind::Session(i),
            Target::Channel(i) => Bind::Channel(i),
            Target::Agent(i) => Bind::Agent(i),
        }
    }
}

fn target_channel(m: &Shared, id: &str) -> Result<Target, ApiError> {
    let c =
        m.rt.store
            .channel(id)
            .map_err(internal)?
            .ok_or_else(|| not_found("channel"))?;
    Ok(Target::Channel(c.id))
}

fn target_session(m: &Shared, id: &str) -> Result<Target, ApiError> {
    m.rt.store
        .session(id)
        .map_err(internal)?
        .ok_or_else(|| not_found("session"))?;
    Ok(Target::Session(id.to_owned()))
}

fn target_agent(m: &Shared, id: &str) -> Result<Target, ApiError> {
    let a =
        m.rt.store
            .agent(id)
            .map_err(internal)?
            .ok_or_else(|| not_found("agent"))?;
    Ok(Target::Agent(a.id))
}

/// What a binding looks like now.
async fn binding(m: &Shared, t: &Target) -> Result<Binding, ApiError> {
    let store = &m.rt.store;
    let own = store.bound_workspace(t.bind()).map_err(internal)?;
    let live = |id: Option<String>| -> Result<Option<WorkspaceRow>, ApiError> {
        match id {
            Some(id) => Ok(store
                .workspace(&id)
                .map_err(internal)?
                .filter(|w| w.archived_at.is_none())),
            None => Ok(None),
        }
    };
    let own_row = live(own)?;
    let (effective, source) = match (&own_row, t) {
        (Some(w), _) => (
            w.clone(),
            match t {
                Target::Session(_) => "session",
                Target::Channel(_) => "channel",
                Target::Agent(_) => "agent",
            },
        ),
        (None, Target::Session(s)) => {
            let (id, src) = store.resolve_workspace(s).map_err(internal)?;
            (find(m, &id)?, src.as_str())
        }
        (None, Target::Channel(c)) => {
            let agent = store
                .channel(c)
                .map_err(internal)?
                .and_then(|c| c.agent_id)
                .unwrap_or_else(|| tern_store::DEFAULT_AGENT.to_owned());
            match live(
                store
                    .bound_workspace(Bind::Agent(&agent))
                    .map_err(internal)?,
            )? {
                Some(w) => (w, "agent"),
                None => (find(m, DEFAULT_WORKSPACE)?, "default"),
            }
        }
        (None, Target::Agent(_)) => (find(m, DEFAULT_WORKSPACE)?, "default"),
    };
    Ok(Binding {
        workspace: match own_row {
            Some(r) => Some(view(m, r).await?),
            None => None,
        },
        effective: view(m, effective).await?,
        source: source.to_owned(),
    })
}

async fn put_binding(m: &Shared, t: Target, b: BindBody) -> Response {
    let row = match b.workspace.as_deref() {
        Some(r) => match m.rt.resolve_ws_ref(r) {
            Ok(w) => Some(w),
            Err(e) => return fail(e),
        },
        None => None,
    };
    if let Err(e) = m.rt.bind_workspace(t.bind(), row.as_ref()) {
        return bad(e).into_response();
    }
    match binding(m, &t).await {
        Ok(b) => Json(b).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn get_binding(m: &Shared, t: Result<Target, ApiError>) -> ApiResult<Binding> {
    Ok(Json(binding(m, &t?).await?))
}

async fn get_channel_binding(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Binding> {
    get_binding(&m, target_channel(&m, &id)).await
}

async fn put_channel_binding(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<BindBody>,
) -> Response {
    match target_channel(&m, &id) {
        Ok(t) => put_binding(&m, t, b).await,
        Err(e) => e.into_response(),
    }
}

async fn clear_channel_binding(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    put_channel_binding(State(m), UrlPath(id), Json(BindBody { workspace: None })).await
}

async fn get_session_binding(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Binding> {
    get_binding(&m, target_session(&m, &id)).await
}

async fn put_session_binding(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<BindBody>,
) -> Response {
    match target_session(&m, &id) {
        Ok(t) => put_binding(&m, t, b).await,
        Err(e) => e.into_response(),
    }
}

async fn clear_session_binding(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    put_session_binding(State(m), UrlPath(id), Json(BindBody { workspace: None })).await
}

async fn get_agent_binding(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Binding> {
    get_binding(&m, target_agent(&m, &id)).await
}

async fn put_agent_binding(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<BindBody>,
) -> Response {
    match target_agent(&m, &id) {
        Ok(t) => put_binding(&m, t, b).await,
        Err(e) => e.into_response(),
    }
}

async fn clear_agent_binding(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    put_agent_binding(State(m), UrlPath(id), Json(BindBody { workspace: None })).await
}
