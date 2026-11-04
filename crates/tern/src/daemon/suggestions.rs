//! Channel suggestions (docs/channels-design.md C5): accepting creates the
//! channel and can move the conversation it came from and its routines
//! there (the move feature); declining just closes the card.
//!
//! | Method | Path | |
//! |---|---|---|
//! | POST | `/v1/suggestions/{id}/accept` | `{slug?, title?, move_conversation? (default true), routines?: [id] (default the suggested)}` → `{channel, moved: [MovedThread], not_moved?, routines: [id]}` |
//! | POST | `/v1/suggestions/{id}/decline` | closes the card |

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tern_core::channels::check_slug;
use tern_core::models::Change;
use tern_store::{ChannelRow, SuggestionRow};

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};
use super::api_channels::{Channel, channel_view};
use super::channels::{MoveError, MovedThread};
use super::manager::Manager;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/suggestions/{id}/accept", post(accept))
        .route("/v1/suggestions/{id}/decline", post(decline))
}

#[derive(Deserialize, Default)]
pub struct AcceptBody {
    /// Overrides the suggested slug and title.
    pub slug: Option<String>,
    pub title: Option<String>,
    /// Move the conversation it was suggested in (default true).
    pub move_conversation: Option<bool>,
    /// Routines to move (default: the suggested ones).
    pub routines: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Accepted {
    pub channel: Channel,
    pub moved: Vec<MovedThread>,
    /// Why the conversation stayed where it was (still working, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_moved: Option<String>,
    /// The routines now posting there.
    pub routines: Vec<String>,
}

fn pending(m: &Shared, id: i64) -> Result<SuggestionRow, ApiError> {
    let s =
        m.rt.store
            .suggestion(id)
            .map_err(internal)?
            .ok_or_else(|| not_found("suggestion"))?;
    if s.state != "pending" {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!("this suggestion was already {}", s.state),
        ));
    }
    Ok(s)
}

async fn accept(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<i64>,
    body: Option<Json<AcceptBody>>,
) -> ApiResult<Accepted> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let s = pending(&m, id)?;
    let slug = b
        .slug
        .as_deref()
        .map(|x| x.trim().trim_start_matches('#').to_lowercase())
        .unwrap_or_else(|| s.slug.clone());
    check_slug(&slug).map_err(bad)?;
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
        .unwrap_or_else(|| s.title.clone());
    let routines = b.routines.unwrap_or_else(|| s.routines.clone());
    let accepted = m
        .accept_suggestion(
            &s,
            &slug,
            &title,
            b.move_conversation.unwrap_or(true),
            &routines,
        )
        .map_err(internal)?;
    Ok(Json(accepted))
}

async fn decline(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<i64>,
) -> ApiResult<serde_json::Value> {
    let s = pending(&m, id)?;
    m.decline_suggestion(&s).map_err(internal)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

impl Manager {
    /// Create the suggested channel, make the suggesting agent a member,
    /// move the conversation (unless it's still working) and `routines`
    /// there, and close the card.
    pub fn accept_suggestion(
        self: &std::sync::Arc<Self>,
        s: &SuggestionRow,
        slug: &str,
        title: &str,
        move_conversation: bool,
        routines: &[String],
    ) -> anyhow::Result<Accepted> {
        let store = &self.rt.store;
        let channel = store.create_channel(&ChannelRow {
            id: tern_core::agent::new_id(),
            slug: slug.to_owned(),
            title: title.to_owned(),
            kind: "topic".into(),
            created_by: "user".into(),
            ..ChannelRow::default()
        })?;
        if !store.decide_suggestion(s.id, "accepted", Some(&channel.id))? {
            anyhow::bail!("this suggestion was decided meanwhile");
        }
        // The suggesting agent joins the channel it proposed.
        if let Some(run) = s
            .run_id
            .as_deref()
            .and_then(|r| store.run(r).ok().flatten())
            && let Some(a) = store.agent(&store.session_agent(&run.session_id)?)?
        {
            self.join(&channel, &a)?;
        }
        let (mut moved, mut not_moved) = (Vec::new(), None);
        if move_conversation && let Some(session) = &s.session_id {
            match self.move_threads(std::slice::from_ref(session), &channel) {
                Ok(m) => moved = m,
                Err(MoveError::Busy(why) | MoveError::Bad(why)) => not_moved = Some(why),
                Err(MoveError::Internal(e)) => anyhow::bail!(e),
            }
        }
        let mut moved_routines = Vec::new();
        for id in routines {
            let Some(r) = store.schedule(id)? else {
                continue;
            };
            store.set_schedule_channel(&r.id, &channel.id)?;
            if let Some(a) =
                store.agent(r.agent_id.as_deref().unwrap_or(tern_store::DEFAULT_AGENT))?
            {
                self.join(&channel, &a)?;
            }
            moved_routines.push(r.id);
        }
        let mut how = format!("Created #{slug}");
        if !moved.is_empty() {
            how.push_str(", and moved this conversation there");
        }
        if !moved_routines.is_empty() {
            how.push_str(&format!(
                "; {} routine{} now post{} there",
                moved_routines.len(),
                if moved_routines.len() == 1 { "" } else { "s" },
                if moved_routines.len() == 1 { "s" } else { "" }
            ));
        }
        self.close_card(s, &how)?;
        self.channels_changed();
        let channel = store.channel(&channel.id)?.unwrap_or(channel);
        Ok(Accepted {
            channel: channel_view(self, channel),
            moved,
            not_moved,
            routines: moved_routines,
        })
    }

    pub fn decline_suggestion(&self, s: &SuggestionRow) -> anyhow::Result<()> {
        if self.rt.store.decide_suggestion(s.id, "declined", None)? {
            self.close_card(s, &format!("Declined #{}", s.slug))?;
        }
        Ok(())
    }

    /// Rewrite the card to how it ended and close it with its pointer.
    fn close_card(&self, s: &SuggestionRow, how: &str) -> anyhow::Result<()> {
        let store = &self.rt.store;
        let Some(post) = s.post_id.and_then(|p| store.post(p).ok().flatten()) else {
            return Ok(());
        };
        let text = format!("{how} (suggested: {})", s.why);
        store.update_post_text(post.id, &text, post.tainted)?;
        store.resolve_post(post.id)?;
        let pointer = store.post_by_key(&format!("pointer:{}", post.id))?;
        for row in [store.post(post.id)?, pointer].into_iter().flatten() {
            let _ = self.rt.changes.send(Change::PostUpdated {
                post: Box::new(super::channels::view(store, row)),
            });
        }
        Ok(())
    }
}
