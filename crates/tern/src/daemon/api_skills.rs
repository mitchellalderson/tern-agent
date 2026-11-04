//! Skills and skill capture over the API (docs/personal-agent-tooling-spec.md
//! §7, T5b). A long, clean run is offered once as a skill; accepting writes a
//! draft the user reviews and publishes, so nothing changes the agent's
//! prompt unasked.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/skills` | `[{name, description, draft, captured_from?}]`: published skills and drafts |
//! | GET | `/v1/skills/{name}` | one skill with its `SKILL.md` as `content` |
//! | POST | `/v1/skills/{name}/publish` | a draft becomes a skill the next run lists |
//! | POST | `/v1/skills/{name}/discard` | deletes a draft (never a published skill) |
//! | GET | `/v1/skill-offers[?state=pending\|accepted\|dismissed\|all]` | offers, newest first (default `pending`; offers lapse after 7 days) |
//! | POST | `/v1/skill-offers/{id}/accept` | `{slug?}`: one model call writes the draft → `{offer, skill}` |
//! | POST | `/v1/skill-offers/{id}/dismiss` | closes the offer; the run is not offered again |
//!
//! Pending offers also show in Tasks (`kind: skill_offer`, "needs you").

use std::time::Duration;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tern_core::config::ModelRef;
use tern_core::skill_capture as capture;
use tern_core::skills;
use tern_core::types::{ChatRequest, ContentBlock, Message, Role, StreamEvent};
use tern_store::SkillOfferRow;

use super::api::{ApiError, ApiResult, Shared, bad, internal, not_found};
use super::manager::Manager;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/skills", get(list_skills))
        .route("/v1/skills/{name}", get(one_skill))
        .route("/v1/skills/{name}/publish", post(publish))
        .route("/v1/skills/{name}/discard", post(discard))
        .route("/v1/skill-offers", get(list_offers))
        .route("/v1/skill-offers/{id}/accept", post(accept))
        .route("/v1/skill-offers/{id}/dismiss", post(dismiss))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillView {
    pub name: String,
    pub description: String,
    pub draft: bool,
    /// The run a draft was captured from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_from: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillFull {
    #[serde(flatten)]
    pub skill: SkillView,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Offer {
    pub id: i64,
    pub run_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub tool_calls: u32,
    pub state: String,
    /// The draft's name once accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
}

impl From<SkillOfferRow> for Offer {
    fn from(o: SkillOfferRow) -> Self {
        Self {
            id: o.id,
            run_id: o.run_id,
            session_id: o.session_id,
            title: o.title,
            tool_calls: o.tool_calls,
            state: o.state,
            skill: o.skill,
            created_at: o.created_at,
            decided_at: o.decided_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Accepted {
    pub offer: Offer,
    pub skill: SkillView,
}

#[derive(Deserialize, Default)]
pub struct AcceptBody {
    /// Overrides the name the model chose.
    pub slug: Option<String>,
}

fn captured_from(text: &str) -> Option<String> {
    let rest = text.strip_prefix("---")?;
    let front = &rest[..rest.find("\n---")?];
    front.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == "captured_from").then(|| v.trim().to_owned())
    })
}

fn view(s: &skills::Skill) -> SkillView {
    let text = std::fs::read_to_string(s.dir.join("SKILL.md")).unwrap_or_default();
    SkillView {
        name: s.name.clone(),
        description: s.description.clone(),
        draft: s.draft,
        captured_from: captured_from(&text),
    }
}

pub fn skill_views(root: &std::path::Path) -> Vec<SkillView> {
    skills::discover_all(root).iter().map(view).collect()
}

async fn list_skills(State(m): State<Shared>) -> ApiResult<Vec<SkillView>> {
    Ok(Json(skill_views(&m.rt.home.skills_dir())))
}

async fn one_skill(
    State(m): State<Shared>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<SkillFull> {
    let all = skills::discover_all(&m.rt.home.skills_dir());
    let s = all
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| not_found("skill"))?;
    let content = std::fs::read_to_string(s.dir.join("SKILL.md")).map_err(internal)?;
    Ok(Json(SkillFull {
        skill: view(s),
        content,
    }))
}

fn draft_error(e: skills::DraftError) -> ApiError {
    match e {
        skills::DraftError::NotFound(_) => ApiError(StatusCode::NOT_FOUND, e.to_string()),
        skills::DraftError::Io(_) => internal(e),
    }
}

async fn publish(State(m): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult<SkillView> {
    let root = m.rt.home.skills_dir();
    skills::publish_draft(&root, &name).map_err(draft_error)?;
    skill_views(&root)
        .into_iter()
        .find(|s| s.name == name)
        .map(Json)
        .ok_or_else(|| not_found("skill"))
}

async fn discard(
    State(m): State<Shared>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<serde_json::Value> {
    skills::discard_draft(&m.rt.home.skills_dir(), &name).map_err(draft_error)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ListQuery {
    state: Option<String>,
}

async fn list_offers(State(m): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Vec<Offer>> {
    let state = match q.state.as_deref() {
        None => Some("pending"),
        Some("all") => None,
        Some(s) => Some(s),
    };
    Ok(Json(
        m.offers(state)
            .map_err(internal)?
            .into_iter()
            .map(Offer::from)
            .collect(),
    ))
}

fn pending(m: &Manager, id: i64) -> Result<SkillOfferRow, ApiError> {
    let o =
        m.rt.store
            .skill_offer(id)
            .map_err(internal)?
            .ok_or_else(|| not_found("skill offer"))?;
    if o.state != "pending" {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!("this offer was already {}", o.state),
        ));
    }
    if o.created_at < tern_store::now_ms() - capture::OFFER_TTL_MS {
        return Err(ApiError(
            StatusCode::GONE,
            "this offer lapsed; ask again after another run".into(),
        ));
    }
    Ok(o)
}

async fn accept(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<i64>,
    body: Option<Json<AcceptBody>>,
) -> ApiResult<Accepted> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let offer = pending(&m, id)?;
    m.capture_skill(&offer, b.slug.as_deref())
        .await
        .map(Json)
        .map_err(|e| match e.downcast_ref::<capture::DraftError>() {
            Some(_) => bad(e),
            None => internal(e),
        })
}

async fn dismiss(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<i64>,
) -> ApiResult<serde_json::Value> {
    let o = pending(&m, id)?;
    m.rt.store
        .decide_skill_offer(o.id, "dismissed", None)
        .map_err(internal)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Biggest transcript sent to the drafting call, in bytes.
const MAX_TRANSCRIPT: usize = 60_000;
const DRAFT_TIMEOUT: Duration = Duration::from_secs(180);

impl Manager {
    /// Offers that haven't lapsed, newest first.
    pub fn offers(&self, state: Option<&str>) -> anyhow::Result<Vec<SkillOfferRow>> {
        Ok(self
            .rt
            .store
            .skill_offers(state, tern_store::now_ms() - capture::OFFER_TTL_MS)?)
    }

    /// A run's messages as the model saw them: its input and everything it
    /// said and did.
    fn run_messages(&self, run: &tern_store::RunRow) -> anyhow::Result<Vec<Message>> {
        Ok(self
            .rt
            .store
            .session_messages(&run.session_id)?
            .into_iter()
            .filter(|r| r.summary_of.is_none())
            .filter(|r| {
                r.run_id.as_deref() == Some(run.id.as_str()) || Some(r.id) == run.input_message_id
            })
            .filter_map(|r| {
                let content: Vec<ContentBlock> = serde_json::from_str(&r.content_json).ok()?;
                Some(Message {
                    role: if r.role == "assistant" {
                        Role::Assistant
                    } else {
                        Role::User
                    },
                    content,
                })
            })
            .collect())
    }

    /// Called once a run has finished: offer it as a skill when it was a
    /// long, clean run of the user's own. Runs that started tainted or
    /// became tainted (they read untrusted content, which would end up
    /// in the draft), unattended runs (routines, the outbox, MCP callers)
    /// and sub-agents are never offered. Returns the offer, if one was made.
    pub fn maybe_offer_skill(&self, run_id: &str) -> anyhow::Result<Option<SkillOfferRow>> {
        let config = self.rt.live().skills;
        if !config.capture {
            return Ok(None);
        }
        let store = &self.rt.store;
        let Some(run) = store.run(run_id)? else {
            return Ok(None);
        };
        let unattended = matches!(run.trigger.as_str(), "cron" | "outbox" | "mcp");
        if run.status != "completed"
            || run.parent_run_id.is_some()
            || run.quiet
            || unattended
            || run.taint.is_some()
            || run.input_taint.is_some()
        {
            return Ok(None);
        }
        let shape = capture::run_shape(&self.run_messages(&run)?);
        if !capture::worth_offering(shape, config.capture_min_tool_calls) {
            return Ok(None);
        }
        let title = store.session(&run.session_id)?.and_then(|s| s.title);
        let offer = store.create_skill_offer(
            &run.id,
            &run.session_id,
            shape.tool_calls,
            title.as_deref(),
        )?;
        if offer.is_some() {
            tracing::info!(run = %run.id, tool_calls = shape.tool_calls, "offering to save a run as a skill");
        }
        Ok(offer)
    }

    /// Accept `offer`: one model call writes the draft from the run's
    /// transcript, saved as `skills/<slug>/SKILL.md` with `draft: true`.
    pub async fn capture_skill(
        &self,
        offer: &SkillOfferRow,
        slug: Option<&str>,
    ) -> anyhow::Result<Accepted> {
        let store = &self.rt.store;
        let run = store
            .run(&offer.run_id)?
            .ok_or_else(|| anyhow::anyhow!("the run is gone"))?;
        let messages = self.run_messages(&run)?;
        let transcript = tern_core::tool::truncate_middle(
            &tern_core::compaction::transcript(&messages),
            MAX_TRANSCRIPT,
        );
        let model: ModelRef = run
            .model
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("the run has no model"))?
            .parse()?;
        let provider = self.rt.agent.provider(&model)?;
        let req = ChatRequest {
            model: model.model.clone(),
            system: capture::DRAFT_SYSTEM_PROMPT.into(),
            messages: vec![capture::draft_request(&transcript)],
            tools: Vec::new(),
            max_tokens: 2500,
            forbid_tool_calls: true,
        };
        let call = async {
            let mut stream = provider
                .stream(req, tokio_util::sync::CancellationToken::new())
                .await?;
            let mut text = String::new();
            while let Some(ev) = stream.next().await {
                if let StreamEvent::TextDelta(t) = ev? {
                    text.push_str(&t);
                }
            }
            anyhow::Ok(text)
        };
        let reply = tokio::time::timeout(DRAFT_TIMEOUT, call)
            .await
            .map_err(|_| anyhow::anyhow!("the model took over {DRAFT_TIMEOUT:?} to write it"))??;
        let draft = capture::finish_draft(&reply, &run.id, slug)?;
        let root = self.rt.home.skills_dir();
        let name = skills::write_draft(&root, &draft.slug, &draft.content)?;
        if !store.decide_skill_offer(offer.id, "accepted", Some(&name))? {
            // Decided meanwhile (a second accept): keep the first draft only.
            let _ = skills::discard_draft(&root, &name);
            anyhow::bail!("this offer was decided meanwhile");
        }
        let skill = skill_views(&root)
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| anyhow::anyhow!("the draft was not written"))?;
        let offer = store
            .skill_offer(offer.id)?
            .ok_or_else(|| anyhow::anyhow!("the offer is gone"))?;
        Ok(Accepted {
            offer: offer.into(),
            skill,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_captured_from() {
        let t = "---\nname: a\ndescription: d\ndraft: true\ncaptured_from: run9\n---\nbody";
        assert_eq!(captured_from(t).as_deref(), Some("run9"));
        assert_eq!(captured_from("---\nname: a\n---\nx"), None);
        assert_eq!(captured_from("no frontmatter"), None);
    }
}
