//! Channels and posts (docs/channels-design.md §5.1). `{id}` is a channel's
//! id or slug; Home's is `home`.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/channels[?archived=1]` | `[Channel]`: Home first, with `last_post_id`, `needs_you`, `active_runs` |
//! | POST | `/v1/channels` | create `{slug, title?, kind?: "topic", instructions?}` |
//! | PATCH | `/v1/channels/{id}` | `{title?, instructions?, archived?}` (`instructions: null` clears them) |
//! | GET | `/v1/channels/{id}/posts?before=&limit=` | `[Post]`, newest first |
//! | POST | `/v1/channels/{id}/posts` | send `{text, reply_to?, model?}` → `{post?, run_id, session_id, queued, new_segment}` |

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Deserializer, Serialize};
use tern_core::channels::{Post, check_slug};
use tern_store::ChannelRow;

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};
use super::channels::{Sent, view};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/channels", get(list).post(create))
        .route("/v1/channels/{id}", axum::routing::patch(update))
        .route("/v1/channels/{id}/posts", get(posts).post(send))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub slug: String,
    pub title: String,
    /// `main` (Home) or `topic`.
    pub kind: String,
    #[serde(default)]
    pub instructions: Option<String>,
    /// The main thread (Home: the current segment), once there is one.
    #[serde(default)]
    pub main_session_id: Option<String>,
    #[serde(default)]
    pub last_post_id: Option<i64>,
    /// Home: approvals and secret requests waiting for an answer.
    #[serde(default)]
    pub needs_you: usize,
    /// Runs in progress in the channel's threads.
    #[serde(default)]
    pub active_runs: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    pub updated_at: i64,
}

pub(super) fn channel_view(m: &Shared, c: ChannelRow) -> Channel {
    let active_runs = m
        .active_runs()
        .into_iter()
        .filter(|(session, _)| {
            m.rt.store.session(session).ok().flatten().is_some_and(|s| {
                s.channel_id.as_deref() == Some(c.id.as_str()) && s.source != "subagent"
            })
        })
        .count();
    let needs_you = if c.kind == "main" {
        m.routes.api.pending().len() + m.routes.secrets.pending().len()
    } else {
        0
    };
    Channel {
        last_post_id: m.rt.store.last_post_id(&c.id).ok().flatten(),
        needs_you,
        active_runs,
        archived: c.archived_at.is_some(),
        id: c.id,
        slug: c.slug,
        title: c.title,
        kind: c.kind,
        instructions: c.instructions,
        main_session_id: c.main_session_id,
        updated_at: c.updated_at,
    }
}

pub(super) fn find(m: &Shared, id: &str) -> Result<ChannelRow, ApiError> {
    m.rt.store
        .channel(id)
        .map_err(internal)?
        .ok_or_else(|| not_found("channel"))
}

#[derive(Deserialize)]
struct ListQuery {
    archived: Option<String>,
}

async fn list(State(m): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Vec<Channel>> {
    let all = matches!(q.archived.as_deref(), Some("1" | "true"));
    let rows = m.rt.store.channels(all).map_err(internal)?;
    Ok(Json(
        rows.into_iter().map(|c| channel_view(&m, c)).collect(),
    ))
}

#[derive(Deserialize)]
struct CreateBody {
    slug: String,
    title: Option<String>,
    kind: Option<String>,
    instructions: Option<String>,
}

async fn create(
    State(m): State<Shared>,
    Json(b): Json<CreateBody>,
) -> Result<(StatusCode, Json<Channel>), ApiError> {
    let slug = b.slug.trim().trim_start_matches('#').to_owned();
    check_slug(&slug).map_err(bad)?;
    if b.kind.as_deref().is_some_and(|k| k != "topic") {
        return Err(bad(
            "only topic channels can be created; Home is the main channel",
        ));
    }
    if m.rt.store.channel(&slug).map_err(internal)?.is_some() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!("there is already a channel #{slug}"),
        ));
    }
    let title = b
        .title
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| slug.clone());
    let row =
        m.rt.store
            .create_channel(&ChannelRow {
                id: tern_core::agent::new_id(),
                slug,
                title,
                kind: "topic".into(),
                instructions: b.instructions.filter(|i| !i.trim().is_empty()),
                created_by: "user".into(),
                ..ChannelRow::default()
            })
            .map_err(internal)?;
    m.channels_changed();
    Ok((StatusCode::CREATED, Json(channel_view(&m, row))))
}

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
fn double_option<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
struct UpdateBody {
    title: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    instructions: Option<Option<String>>,
    archived: Option<bool>,
}

async fn update(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Channel> {
    let c = find(&m, &id)?;
    if c.kind == "main" && b.archived == Some(true) {
        return Err(bad("Home can't be archived"));
    }
    let title = b.title.as_deref().map(str::trim);
    if title == Some("") {
        return Err(bad("title is empty"));
    }
    let instructions = b
        .instructions
        .as_ref()
        .map(|i| i.as_deref().map(str::trim).filter(|t| !t.is_empty()));
    m.rt.store
        .update_channel(&c.id, title, instructions, b.archived)
        .map_err(internal)?;
    m.channels_changed();
    Ok(Json(channel_view(&m, find(&m, &c.id)?)))
}

#[derive(Deserialize)]
struct PostsQuery {
    before: Option<i64>,
    limit: Option<usize>,
}

async fn posts(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<PostsQuery>,
) -> ApiResult<Vec<Post>> {
    let c = find(&m, &id)?;
    let rows =
        m.rt.store
            .posts(&c.id, q.before, q.limit.unwrap_or(50).clamp(1, 200))
            .map_err(internal)?;
    Ok(Json(
        rows.into_iter().map(|p| view(&m.rt.store, p)).collect(),
    ))
}

#[derive(Deserialize)]
struct SendBody {
    text: String,
    reply_to: Option<i64>,
    model: Option<String>,
}

async fn send(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<SendBody>,
) -> ApiResult<Sent> {
    let c = find(&m, &id)?;
    let sent = m
        .send_to_channel(&c, &b.text, b.reply_to, b.model.as_deref())
        .await
        .map_err(bad)?;
    Ok(Json(sent))
}
