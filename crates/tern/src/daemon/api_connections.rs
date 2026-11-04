//! Connections (accounts) and sign-in cards over the API (tooling spec
//! §6.7–6.8). Nothing here ever returns a token or key.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/connections` | `{connections: [...], targets: [...]}`: every account, and every connector and MCP server one can be added to |
//! | POST | `/v1/connections` | add an account: `{kind?, target, connection?, label?, method?, device?, values?}`; answers when the sign-in ends |
//! | POST | `/v1/connections/{kind}/{target}/{slug}/reconnect` | sign in again (`{device?, values?}`) |
//! | POST | `/v1/connections/{kind}/{target}/{slug}/default` | make it the default |
//! | PATCH | `/v1/connections/{kind}/{target}/{slug}` | `{label}` |
//! | DELETE | `/v1/connections/{kind}/{target}/{slug}` | revoke at the provider when it can, delete its secrets, kv and grants |
//! | GET | `/v1/auth-prompts` | sign-in cards waiting for an answer |
//! | POST | `/v1/auth-prompts/{id}` | answer `{answer}`: an option's id, `device`, or `cancel` |
//!
//! Cards arrive on `/v1/events` as `{auth_prompt: …}` and close with
//! `{auth_prompt_closed: id}` (and, for the agent's, the `tool_finished` of
//! their `call_id`). An OAuth add holds the request open while the user
//! signs in: the card it opens is on the event stream like any other. All of
//! it is owner-only.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path as UrlPath, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_auth::tools::{ConnectError, Target};
use tern_auth::{Conn, Kind};

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};
use super::manager::{AnswerError, ApiAuthPrompt};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/connections", get(list).post(add))
        .route(
            "/v1/connections/{kind}/{target}/{slug}",
            axum::routing::patch(rename).delete(remove),
        )
        .route(
            "/v1/connections/{kind}/{target}/{slug}/default",
            post(make_default),
        )
        .route(
            "/v1/connections/{kind}/{target}/{slug}/reconnect",
            post(reconnect),
        )
        .route("/v1/auth-prompts", get(pending))
        .route("/v1/auth-prompts/{id}", post(answer))
}

/// An account, for clients.
#[derive(Serialize)]
pub struct ApiConnection {
    #[serde(flatten)]
    pub conn: Conn,
    /// `gmail · work (me@work.com)`.
    pub title: String,
}

/// Something accounts can be added to.
#[derive(Serialize)]
pub struct ApiTarget {
    pub kind: Kind,
    pub name: String,
    /// `oauth` when tern knows a sign-in for it, `key` when it names secrets.
    pub methods: Vec<&'static str>,
    pub key_names: Vec<String>,
}

#[derive(Serialize)]
struct Listing {
    connections: Vec<ApiConnection>,
    targets: Vec<ApiTarget>,
}

fn api_conn(c: Conn) -> ApiConnection {
    ApiConnection {
        title: c.title(),
        conn: c,
    }
}

async fn targets(m: &Shared) -> Vec<Target> {
    let f = m.rt.auth_targets.clone();
    tokio::task::spawn_blocking(move || f())
        .await
        .unwrap_or_default()
}

async fn list(State(m): State<Shared>) -> ApiResult<Listing> {
    m.rt.connections.reload();
    let targets = targets(&m)
        .await
        .into_iter()
        .map(|t| {
            let mut methods = Vec::new();
            if m.rt.auth.config_for(&m.rt.connections, &t).is_some() {
                methods.push("oauth");
            }
            if !t.key_names.is_empty() {
                methods.push("key");
            }
            ApiTarget {
                kind: t.kind,
                name: t.name,
                methods,
                key_names: t.key_names,
            }
        })
        .collect();
    Ok(Json(Listing {
        connections: m.rt.connections.all().into_iter().map(api_conn).collect(),
        targets,
    }))
}

#[derive(Deserialize)]
struct AddBody {
    #[serde(default)]
    kind: Option<String>,
    target: String,
    #[serde(default)]
    connection: Option<String>,
    #[serde(default)]
    label: Option<String>,
    /// `oauth` or `key`; default OAuth when tern knows a sign-in.
    #[serde(default)]
    method: Option<String>,
    /// Use a device code instead of a browser on tern's machine (a remote
    /// core).
    #[serde(default)]
    device: bool,
    /// For `key`: the values, by the names the target declares.
    #[serde(default)]
    values: BTreeMap<String, String>,
}

fn kind_of(s: &str) -> Result<Kind, ApiError> {
    Kind::parse(s).ok_or_else(|| bad(format!("kind must be connector or mcp, not {s:?}")))
}

fn connect_err(e: ConnectError) -> ApiError {
    match e {
        ConnectError::Skipped(why) => ApiError(axum::http::StatusCode::CONFLICT, why),
        ConnectError::TimedOut => ApiError(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            "the sign-in timed out".into(),
        ),
        ConnectError::Failed(e) => bad(e),
    }
}

async fn connect(
    m: &Shared,
    target: Target,
    slug: Option<String>,
    label: Option<String>,
    method: &str,
    device: bool,
    values: BTreeMap<String, String>,
) -> ApiResult<Value> {
    let conns = m.rt.connections.clone();
    let recorded = match method {
        "oauth" => {
            let shown = match &slug {
                Some(s) => format!("{} · {s}", target.name),
                None => target.name.clone(),
            };
            let asker: Arc<dyn tern_core::approval::ApprovalHandler> =
                Arc::new(super::manager::Router(m.routes.clone()));
            let present =
                tern_auth::tools::card_presenter(asker, String::new(), String::new(), None, shown);
            let req = tern_auth::tools::Connect {
                target: &target,
                slug: slug.as_deref(),
                label: label.as_deref(),
                kept: None,
                device,
            };
            tern_auth::tools::connect_oauth(&m.rt.auth, &conns, req, present)
                .await
                .map_err(connect_err)?
        }
        "key" => {
            let slug = slug.unwrap_or_else(|| {
                tern_auth::signin::new_slug(&conns, target.kind, &target.name, None)
            });
            tern_auth::tools::connect_key(&conns, &target, &slug, label.as_deref(), &values)
                .map_err(connect_err)?
        }
        other => return Err(bad(format!("method must be oauth or key, not {other:?}"))),
    };
    let c = recorded.conn().clone();
    let status = match recorded {
        tern_auth::signin::Recorded::Connected(_) => "connected",
        tern_auth::signin::Recorded::Reconnected(_) => "reconnected",
    };
    tracing::info!(target = %c.target, connection = %c.slug, status, "connection signed in");
    Ok(Json(json!({ "status": status, "connection": api_conn(c) })))
}

async fn add(State(m): State<Shared>, Json(b): Json<AddBody>) -> ApiResult<Value> {
    let all = m.rt.auth_targets.clone();
    let target = tern_auth::tools::find_target(&all, &b.target, b.kind.as_deref()).map_err(bad)?;
    let slug = b
        .connection
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    if let Some(s) = &slug {
        m.rt.connections
            .check_new(target.kind, &target.name, s)
            .map_err(bad)?;
    }
    let method = b.method.unwrap_or_else(|| {
        if m.rt.auth.config_for(&m.rt.connections, &target).is_some() {
            "oauth".into()
        } else {
            "key".into()
        }
    });
    connect(&m, target, slug, b.label, &method, b.device, b.values).await
}

fn find(m: &Shared, kind: &str, target: &str, slug: &str) -> Result<Conn, ApiError> {
    m.rt.connections.reload();
    m.rt.connections
        .get(kind_of(kind)?, target, slug)
        .ok_or_else(|| not_found("connection"))
}

#[derive(Deserialize, Default)]
struct ReconnectBody {
    #[serde(default)]
    device: bool,
    #[serde(default)]
    values: BTreeMap<String, String>,
}

async fn reconnect(
    State(m): State<Shared>,
    UrlPath((kind, target, slug)): UrlPath<(String, String, String)>,
    body: Option<Json<ReconnectBody>>,
) -> ApiResult<Value> {
    let c = find(&m, &kind, &target, &slug)?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let all = m.rt.auth_targets.clone();
    let t = tern_auth::tools::find_target(&all, &target, Some(&kind)).map_err(bad)?;
    let method = c.auth.method();
    let method = if method == "none" { "key" } else { method };
    connect(&m, t, Some(slug), None, method, b.device, b.values).await
}

async fn make_default(
    State(m): State<Shared>,
    UrlPath((kind, target, slug)): UrlPath<(String, String, String)>,
) -> ApiResult<ApiConnection> {
    let c = find(&m, &kind, &target, &slug)?;
    m.rt.connections.set_default(&c).map_err(internal)?;
    Ok(Json(api_conn(find(&m, &kind, &target, &slug)?)))
}

#[derive(Deserialize)]
struct RenameBody {
    label: String,
}

async fn rename(
    State(m): State<Shared>,
    UrlPath((kind, target, slug)): UrlPath<(String, String, String)>,
    Json(b): Json<RenameBody>,
) -> ApiResult<ApiConnection> {
    let c = find(&m, &kind, &target, &slug)?;
    m.rt.connections.rename(&c, &b.label).map_err(internal)?;
    Ok(Json(api_conn(find(&m, &kind, &target, &slug)?)))
}

async fn remove(
    State(m): State<Shared>,
    UrlPath((kind, target, slug)): UrlPath<(String, String, String)>,
) -> ApiResult<Value> {
    let c = find(&m, &kind, &target, &slug)?;
    m.rt.connections.remove(&c).await.map_err(internal)?;
    tracing::info!(target = %c.target, connection = %c.slug, "connection removed");
    Ok(Json(json!({ "ok": true })))
}

async fn pending(State(m): State<Shared>) -> Json<Vec<ApiAuthPrompt>> {
    Json(m.routes.auth.pending())
}

#[derive(Deserialize)]
struct AnswerBody {
    answer: String,
}

async fn answer(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<AnswerBody>,
) -> ApiResult<Value> {
    match m.routes.auth.answer(&id, &b.answer) {
        Ok(()) => Ok(Json(json!({ "ok": true }))),
        Err(AnswerError::NotFound) => Err(not_found("pending sign-in card")),
        Err(AnswerError::NotAsked(a)) => {
            Err(bad(format!("{a:?} isn't an answer this card offers")))
        }
    }
}
