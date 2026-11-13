//! Documents over the API (docs/documents-design.md §6): what the agent's
//! `create_document` and `edit_document` made, so the app can show it, list
//! its revisions and clear its "from the web" mark. Read-only apart from the
//! review mark; editing by the user is a follow-up.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/documents[?session=][&q=][&limit=]` | documents, newest first |
//! | GET | `/v1/documents/{id}` | the document and its latest text |
//! | GET | `/v1/documents/{id}/revisions` | every revision, newest first |
//! | GET | `/v1/documents/{id}/revisions/{rev}` | one revision's text |
//! | POST | `/v1/documents/{id}/reviewed` | clear the "from the web" mark (ledgered) |

use axum::extract::{Path as UrlPath, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_store::{DocumentFilter, DocumentRow, RevisionRow};

use super::api::{ApiResult, Shared, internal, not_found};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/documents", get(list))
        .route("/v1/documents/{id}", get(show))
        .route("/v1/documents/{id}/revisions", get(revisions))
        .route("/v1/documents/{id}/revisions/{rev}", get(revision))
        .route("/v1/documents/{id}/reviewed", post(reviewed))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub title: String,
    /// `markdown`, `text`, `code` or `csv`.
    pub kind: String,
    /// Relative to the workspace.
    pub path: String,
    pub workspace_id: String,
    pub rev: i64,
    pub bytes: i64,
    /// From web or other untrusted content, not yet reviewed.
    pub tainted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub visibility: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub rev: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_rev: Option<i64>,
    /// `agent`, `user`, `external` or `system`.
    pub author_kind: String,
    pub author_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub added: i64,
    pub removed: i64,
    pub bytes: i64,
    pub tainted: bool,
    pub rebased: bool,
    pub created_at: i64,
}

fn doc_view(d: &DocumentRow) -> Document {
    Document {
        id: d.id.clone(),
        title: d.title.clone(),
        kind: d.kind.clone(),
        path: d.path.clone(),
        workspace_id: d.workspace_id.clone(),
        rev: d.current_rev,
        bytes: d.size,
        tainted: d.tainted,
        agent_id: d.agent_id.clone(),
        session_id: d.session_id.clone(),
        visibility: d.visibility.clone(),
        created_at: d.created_at,
        updated_at: d.updated_at,
    }
}

fn rev_view(r: &RevisionRow) -> Revision {
    Revision {
        rev: r.rev,
        parent_rev: r.parent_rev,
        author_kind: r.author_kind.clone(),
        author_id: r.author_id.clone(),
        run_id: r.run_id.clone(),
        summary: r.summary.clone(),
        added: r.added,
        removed: r.removed,
        bytes: r.size,
        tainted: r.tainted,
        rebased: r.rebased,
        created_at: r.created_at,
    }
}

#[derive(Deserialize)]
struct Filter {
    session: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
}

async fn list(State(m): State<Shared>, Query(f): Query<Filter>) -> ApiResult<Vec<Document>> {
    let rows =
        m.rt.store
            .documents(&DocumentFilter {
                session_id: f.session.as_deref(),
                workspace_id: None,
                query: f.q.as_deref(),
                limit: Some(f.limit.unwrap_or(200).min(500)),
            })
            .map_err(internal)?;
    Ok(Json(rows.iter().map(doc_view).collect()))
}

async fn show(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Value> {
    let store = &m.rt.store;
    let d = store
        .document(&id)
        .map_err(internal)?
        .ok_or_else(|| not_found("document"))?;
    let content = store
        .revision_content(&id, d.current_rev)
        .map_err(internal)?
        .unwrap_or_default();
    let mut v = serde_json::to_value(doc_view(&d)).map_err(internal)?;
    v["content"] = json!(content);
    Ok(Json(v))
}

async fn revisions(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Vec<Revision>> {
    if m.rt.store.document(&id).map_err(internal)?.is_none() {
        return Err(not_found("document"));
    }
    let rows = m.rt.store.revisions(&id).map_err(internal)?;
    Ok(Json(rows.iter().map(rev_view).collect()))
}

async fn revision(
    State(m): State<Shared>,
    UrlPath((id, rev)): UrlPath<(String, i64)>,
) -> ApiResult<Value> {
    let store = &m.rt.store;
    let r = store
        .revision(&id, rev)
        .map_err(internal)?
        .ok_or_else(|| not_found("revision"))?;
    let content = store
        .revision_content(&id, rev)
        .map_err(internal)?
        .unwrap_or_default();
    let mut v = serde_json::to_value(rev_view(&r)).map_err(internal)?;
    v["content"] = json!(content);
    Ok(Json(v))
}

async fn reviewed(State(m): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Value> {
    if m.rt.store.mark_document_reviewed(&id).map_err(internal)? {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(not_found("document"))
    }
}
