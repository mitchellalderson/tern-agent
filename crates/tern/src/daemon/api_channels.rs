//! Channels and posts (docs/channels-design.md §5.1). `{id}` is a channel's
//! id or slug; Home's is `home`.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/channels[?archived=1]` | `[Channel]`: Home first, with `last_post_id`, `needs_you`, `active_runs` |
//! | POST | `/v1/channels` | create `{slug, title?, kind?: "topic", instructions?}` |
//! | PATCH | `/v1/channels/{id}` | `{title?, instructions?, archived?, lead?, agent_wakes?, max_hops?, wakes_per_hour?}` (`instructions: null` clears them; the last four: topic channels) |
//! | GET | `/v1/channels/{id}/agents` | a topic channel's `{members: [{agent_id, lead?, added_at}], agent_wakes, max_hops, wakes_per_hour}` (also on `Channel` as `agents`) |
//! | POST | `/v1/channels/{id}/agents` | add `{agent}` (id or slug), with a "Name joined" note |
//! | DELETE | `/v1/channels/{id}/agents/{agent}` | take an agent out (not the lead or `tern`) |
//! | GET | `/v1/channels/{id}/posts?before=&limit=` | `[Post]`, newest first |
//! | GET | `/v1/channels/{id}/stream?before=&limit=` | a main channel's stream: its segments' messages and its posts, interleaved, newest first, with a `cut` item where each segment starts (`next`: the cursor for older items) |
//! | POST | `/v1/channels/move` | move conversations into a topic channel: `{sessions: [id], channel? \| new_channel?: {slug?, title, instructions?}}` → `{channel, moved: [MovedThread]}` (owner scope) |
//! | POST | `/v1/posts/{id}/resolve` | mark a `needs_you` post or its Home `pointer` done (both close) |
//! | POST | `/v1/channels/{id}/posts` | send `{text, reply_to?, new_topic?, model?}` → `{post?, run_id, session_id, queued, new_segment, woke?: [{agent_id, run_id, session_id, queued}]}`; in a topic channel the lead answers, or each agent `@mentioned` (who joins if it wasn't a member) |

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
        .route("/v1/channels/{id}/stream", get(stream))
        .route("/v1/channels/{id}/agents", get(members).post(add_member))
        .route(
            "/v1/channels/{id}/agents/{agent}",
            axum::routing::delete(remove_member),
        )
        .route("/v1/channels/move", axum::routing::post(move_in))
        .route("/v1/posts/{id}/resolve", axum::routing::post(resolve))
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
    /// Open `needs_you` posts (Home: pointers to them), and in Home the
    /// approvals and secret requests waiting for an answer.
    #[serde(default)]
    pub needs_you: usize,
    /// Runs in progress in the channel's threads.
    #[serde(default)]
    pub active_runs: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    /// A main channel: whose DM it is (`tern` for Home); a topic channel:
    /// its lead, when not the default agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// A topic channel's agents (agent profiles §5.1), lead first, and
    /// its limits on agents waking agents (§5.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<ChannelAgents>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelAgents {
    pub members: Vec<MemberView>,
    pub agent_wakes: bool,
    pub max_hops: i64,
    pub wakes_per_hour: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberView {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lead: bool,
    pub added_at: i64,
}

fn channel_agents(m: &Shared, c: &ChannelRow) -> Option<ChannelAgents> {
    if c.kind != "topic" {
        return None;
    }
    let w = m.rt.store.channel_wakes(&c.id).ok()?;
    let members = m.rt.store.channel_members(c).ok()?;
    Some(ChannelAgents {
        members: members
            .into_iter()
            .map(|m| MemberView {
                agent_id: m.agent_id,
                lead: m.lead,
                added_at: m.added_at,
            })
            .collect(),
        agent_wakes: w.agent_wakes,
        max_hops: w.max_hops,
        wakes_per_hour: w.wakes_per_hour,
    })
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
    // Open needs-you posts (Home: their pointers). Approvals and secret
    // requests wait where their run is: elsewhere as needs-you posts
    // (counted above, and by their pointer in Home), in Home as cards in
    // its stream, which Home adds.
    let open = m.rt.store.open_needs_you(&c.id).unwrap_or(0);
    let needs_you = if c.id == tern_store::HOME {
        let home = |ch: &Option<String>| crate::daemon::waiting::in_home(&(ch.clone(), None));
        open + m
            .routes
            .api
            .pending()
            .iter()
            .filter(|p| home(&p.channel_id))
            .count()
            + m.routes
                .secrets
                .pending()
                .iter()
                .filter(|p| home(&p.channel_id))
                .count()
    } else {
        open
    };
    Channel {
        agents: channel_agents(m, &c),
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
        agent_id: c.agent_id,
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
    /// Topic channels (agent profiles §5): the agent that answers posts
    /// mentioning no one, and the wake limits.
    lead: Option<String>,
    agent_wakes: Option<bool>,
    max_hops: Option<i64>,
    wakes_per_hour: Option<i64>,
}

async fn update(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Channel> {
    let c = find(&m, &id)?;
    if c.kind == "main" && b.archived == Some(true) {
        return Err(bad(if c.id == tern_store::HOME {
            "Home can't be archived".to_owned()
        } else {
            "an agent's DM is archived with its agent".to_owned()
        }));
    }
    let title = b.title.as_deref().map(str::trim);
    if title == Some("") {
        return Err(bad("title is empty"));
    }
    let instructions = b
        .instructions
        .as_ref()
        .map(|i| i.as_deref().map(str::trim).filter(|t| !t.is_empty()));
    let agents_change = b.lead.is_some()
        || b.agent_wakes.is_some()
        || b.max_hops.is_some()
        || b.wakes_per_hour.is_some();
    if agents_change && c.kind != "topic" {
        return Err(bad("only topic channels have a lead and wake settings"));
    }
    let lead = match b.lead.as_deref() {
        Some(l) => Some(active_agent(&m, l)?),
        None => None,
    };
    if b.max_hops.is_some_and(|h| !(1..=10).contains(&h)) {
        return Err(bad("max_hops is 1 to 10"));
    }
    if b.wakes_per_hour.is_some_and(|n| !(0..=120).contains(&n)) {
        return Err(bad("wakes_per_hour is 0 to 120"));
    }
    m.rt.store
        .update_channel(&c.id, title, instructions, b.archived)
        .map_err(internal)?;
    if let Some(a) = lead {
        m.rt.store
            .set_channel_lead(&c.id, &a.id)
            .map_err(internal)?;
    }
    if agents_change {
        let mut w = m.rt.store.channel_wakes(&c.id).map_err(internal)?;
        w.agent_wakes = b.agent_wakes.unwrap_or(w.agent_wakes);
        w.max_hops = b.max_hops.unwrap_or(w.max_hops);
        w.wakes_per_hour = b.wakes_per_hour.unwrap_or(w.wakes_per_hour);
        m.rt.store.set_channel_wakes(&c.id, &w).map_err(internal)?;
    }
    m.channels_changed();
    Ok(Json(channel_view(&m, find(&m, &c.id)?)))
}

/// An active agent by id or slug.
fn active_agent(m: &Shared, id: &str) -> Result<tern_store::AgentRow, ApiError> {
    m.rt.store
        .agent(id)
        .map_err(internal)?
        .filter(|a| a.archived_at.is_none())
        .ok_or_else(|| bad(format!("no agent {id:?}")))
}

async fn members(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<ChannelAgents> {
    let c = find(&m, &id)?;
    channel_agents(&m, &c)
        .map(Json)
        .ok_or_else(|| bad("only topic channels have members"))
}

#[derive(Deserialize)]
struct AddMemberBody {
    agent: String,
}

async fn add_member(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<AddMemberBody>,
) -> ApiResult<ChannelAgents> {
    let c = find(&m, &id)?;
    if c.kind != "topic" || c.archived_at.is_some() {
        return Err(bad("agents join topic channels that aren't archived"));
    }
    let a = active_agent(&m, &b.agent)?;
    m.join(&c, &a).map_err(internal)?;
    members(State(m), UrlPath(id)).await
}

async fn remove_member(
    State(m): State<Shared>,
    UrlPath((id, agent)): UrlPath<(String, String)>,
) -> ApiResult<ChannelAgents> {
    let c = find(&m, &id)?;
    let a =
        m.rt.store
            .agent(&agent)
            .map_err(internal)?
            .ok_or_else(|| not_found("agent"))?;
    if a.id == tern_store::DEFAULT_AGENT || a.id == c.lead() {
        return Err(bad(format!(
            "{} can't leave #{}: {}",
            a.name,
            c.slug,
            if a.id == c.lead() {
                "it's the lead; make another agent the lead first"
            } else {
                "the default agent is in every channel"
            }
        )));
    }
    if m.rt
        .store
        .remove_channel_agent(&c, &a.id)
        .map_err(internal)?
    {
        m.channels_changed();
    }
    members(State(m), UrlPath(id)).await
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
struct NewChannelBody {
    slug: Option<String>,
    title: String,
    instructions: Option<String>,
}

#[derive(Deserialize)]
struct MoveBody {
    sessions: Vec<String>,
    channel: Option<String>,
    new_channel: Option<NewChannelBody>,
}

#[derive(Serialize, Deserialize)]
pub struct MoveResult {
    pub channel: Channel,
    pub moved: Vec<super::channels::MovedThread>,
}

/// Move conversations (Home segments, side chats, threads elsewhere,
/// History entries) into a topic channel, or into a new one.
async fn move_in(State(m): State<Shared>, Json(b): Json<MoveBody>) -> ApiResult<MoveResult> {
    let target = match (&b.channel, &b.new_channel) {
        (Some(c), None) => find(&m, c)?,
        (None, Some(n)) => {
            let title = n.title.trim();
            if title.is_empty() {
                return Err(bad("new_channel.title is empty"));
            }
            let slug = n
                .slug
                .clone()
                .unwrap_or_else(|| slug_for(title))
                .trim()
                .trim_start_matches('#')
                .to_owned();
            check_slug(&slug).map_err(bad)?;
            if m.rt.store.channel(&slug).map_err(internal)?.is_some() {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    format!("there is already a channel #{slug}"),
                ));
            }
            // Checked before the channel exists, so a refused move leaves
            // no empty channel behind.
            m.check_move(&b.sessions, None).map_err(move_error)?;
            let row =
                m.rt.store
                    .create_channel(&ChannelRow {
                        id: tern_core::agent::new_id(),
                        slug,
                        title: title.to_owned(),
                        kind: "topic".into(),
                        instructions: n.instructions.clone().filter(|i| !i.trim().is_empty()),
                        created_by: "user".into(),
                        ..ChannelRow::default()
                    })
                    .map_err(internal)?;
            m.channels_changed();
            row
        }
        _ => return Err(bad("give either channel or new_channel")),
    };
    let moved = m.move_threads(&b.sessions, &target).map_err(move_error)?;
    Ok(Json(MoveResult {
        channel: channel_view(&m, find(&m, &target.id)?),
        moved,
    }))
}

fn move_error(e: super::channels::MoveError) -> ApiError {
    use super::channels::MoveError;
    match e {
        MoveError::Bad(s) => bad(s),
        MoveError::Busy(s) => ApiError(StatusCode::CONFLICT, s),
        MoveError::Internal(s) => internal(s),
    }
}

/// `[a-z0-9][a-z0-9-]{0,31}` from a title.
fn slug_for(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(32).collect();
    out.trim_end_matches('-').to_owned()
}

async fn resolve(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<i64>,
) -> ApiResult<serde_json::Value> {
    // An outbox item's post closes when the item is decided, and a
    // waiting request's when it's answered.
    if let Some(post) = m.rt.store.post(id).map_err(internal)? {
        if let Some(item) = super::outbox::outbox_of_post(&m.rt.store, &post) {
            return Err(ApiError(
                axum::http::StatusCode::CONFLICT,
                format!("this is outbox #{item}: approve or reject it instead"),
            ));
        }
        if post.resolved_at.is_none()
            && let Some((what, _)) = super::waiting::waiting_of(&m.rt.store, &post)
        {
            return Err(ApiError(
                axum::http::StatusCode::CONFLICT,
                format!("this {what} closes when it's answered: answer it instead"),
            ));
        }
    }
    if !m.rt.store.resolve_post(id).map_err(internal)? {
        return Err(not_found("open needs-you post"));
    }
    m.channels_changed();
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SendBody {
    text: String,
    reply_to: Option<i64>,
    /// Home: start a new segment ("New topic").
    #[serde(default)]
    new_topic: bool,
    model: Option<String>,
}

async fn send(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<SendBody>,
) -> ApiResult<Sent> {
    let c = find(&m, &id)?;
    let sent = m
        .send_to_channel(&c, &b.text, b.reply_to, b.new_topic, b.model.as_deref())
        .await
        .map_err(bad)?;
    Ok(Json(sent))
}

/// One entry of a stream page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamItem {
    /// A message of one of the stream's segments, as in `GET /v1/sessions/{id}`.
    Message {
        session_id: String,
        message_id: i64,
        role: String,
        #[serde(default)]
        run_id: Option<String>,
        content: Vec<tern_core::types::ContentBlock>,
        #[serde(default)]
        summary: bool,
        /// Sent while a run was working and read by it at its next call.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        queued: bool,
        created_at: i64,
    },
    /// A post from elsewhere: a routine's result, later pointers and cards.
    Post { post: Box<Post> },
    /// Where a segment starts: why it was cut, and what was carried over.
    Cut {
        session_id: String,
        #[serde(default)]
        prev_session_id: Option<String>,
        /// `idle`, `day`, `new_topic` or `migrated`.
        #[serde(default)]
        reason: Option<String>,
        /// The carry message, once the segment's first run has made it.
        #[serde(default)]
        carry: Option<String>,
        created_at: i64,
    },
}

impl StreamItem {
    /// Order within a stream: time, then cut before messages before posts.
    fn key(&self) -> (i64, u8, i64) {
        match self {
            StreamItem::Cut { created_at, .. } => (*created_at, 0, 0),
            StreamItem::Message {
                created_at,
                message_id,
                ..
            } => (*created_at, 1, *message_id),
            StreamItem::Post { post } => (post.created_at, 2, post.id),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamPage {
    /// Newest first.
    pub items: Vec<StreamItem>,
    /// Pass as `before` for the page of older items; absent at the start.
    #[serde(default)]
    pub next: Option<String>,
}

/// How a queued message from the user reads in the session (the inbox
/// step's rendering, `tern_core::subagent::render_mail`).
const QUEUED_PREFIX: &str = "[message from the user to this agent]\n";

fn cursor(key: (i64, u8, i64)) -> String {
    format!("{}.{}.{}", key.0, key.1, key.2)
}

fn parse_cursor(s: &str) -> Option<(i64, u8, i64)> {
    let mut it = s.split('.');
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

/// The items of one segment, oldest first: its cut and its messages, with
/// the carry message folded into the cut and queued messages unwrapped.
fn segment_items(
    store: &tern_store::Store,
    s: &tern_store::SessionRow,
) -> anyhow::Result<Vec<StreamItem>> {
    let mut carry = None;
    let mut items = Vec::new();
    for m in store.session_messages(&s.id)? {
        let mut content: Vec<tern_core::types::ContentBlock> =
            serde_json::from_str(&m.content_json).unwrap_or_default();
        let mut queued = false;
        if m.role == "user"
            && m.step_idx.is_some()
            && let [tern_core::types::ContentBlock::Text { text }] = content.as_mut_slice()
        {
            if text.starts_with(tern_core::channels::CARRY_HEADER) {
                carry = Some(text.clone());
                continue;
            }
            // The agent's own catch-up on posts the stream already shows.
            if text.starts_with(tern_core::channels::DIGEST_HEADER) {
                continue;
            }
            if let Some(rest) = text.strip_prefix(QUEUED_PREFIX) {
                *text = rest.to_owned();
                queued = true;
            }
        }
        items.push(StreamItem::Message {
            session_id: s.id.clone(),
            message_id: m.id,
            role: m.role,
            run_id: m.run_id,
            content,
            summary: m.summary_of.is_some(),
            queued,
            created_at: m.created_at,
        });
    }
    if s.prev_session_id.is_some() || s.segment_reason.is_some() {
        items.insert(
            0,
            StreamItem::Cut {
                session_id: s.id.clone(),
                prev_session_id: s.prev_session_id.clone(),
                reason: s.segment_reason.clone(),
                carry,
                created_at: s.created_at,
            },
        );
    }
    Ok(items)
}

#[derive(Deserialize)]
struct StreamQuery {
    before: Option<String>,
    limit: Option<usize>,
}

/// Walk the segments back from the newest until a page is full, then add
/// the channel's posts from the time those cover.
async fn stream(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<StreamQuery>,
) -> ApiResult<StreamPage> {
    let c = find(&m, &id)?;
    if c.kind != "main" {
        return Err(bad(format!(
            "#{} is a topic channel; read its posts",
            c.slug
        )));
    }
    let store = &m.rt.store;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let before = match q.before.as_deref() {
        Some(b) => parse_cursor(b).ok_or_else(|| bad("before: not a cursor from this endpoint"))?,
        None => (i64::MAX, u8::MAX, i64::MAX),
    };
    let mut items = Vec::new();
    let mut seg = c.main_session_id.clone();
    let mut seen = std::collections::HashSet::new();
    let mut floor = 0;
    while let Some(sid) = seg.take() {
        if !seen.insert(sid.clone()) {
            break;
        }
        let Some(s) = store.session(&sid).map_err(internal)? else {
            break;
        };
        let older: Vec<StreamItem> = segment_items(store, &s)
            .map_err(internal)?
            .into_iter()
            .filter(|i| i.key() < before)
            .collect();
        items.extend(older);
        seg = s.prev_session_id;
        if items.len() >= limit && seg.is_some() {
            floor = s.created_at;
            break;
        }
    }
    let top = if before.0 == i64::MAX {
        i64::MAX
    } else {
        before.0 + 1
    };
    for p in store.posts_between(&c.id, floor, top).map_err(internal)? {
        let item = StreamItem::Post {
            post: Box::new(view(store, p)),
        };
        if item.key() < before {
            items.push(item);
        }
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.key()));
    let more = items.len() > limit || floor > 0;
    items.truncate(limit);
    let next = more
        .then(|| items.last().map(|i| cursor(i.key())))
        .flatten();
    Ok(Json(StreamPage { items, next }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture the Dart client decodes is exactly this API's shape.
    #[test]
    fn dart_fixture_matches_the_api() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/desktop/packages/tern_client/test/fixtures/stream_page.json"
        );
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let parsed: StreamPage = serde_json::from_value(fixture.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), fixture);
    }

    #[test]
    fn cursors_round_trip_and_order_items() {
        let k = (1762185600000, 2, 3);
        assert_eq!(parse_cursor(&cursor(k)), Some(k));
        assert_eq!(parse_cursor("junk"), None);
        assert!(
            (5, 0, 0) < (5, 1, 0),
            "a cut sorts before its first message"
        );
    }
}
