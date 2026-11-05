//! `POST /v1/uploads` (deployment spec §7.3): a remote client attaches a
//! file by uploading it into the workspace, since it can't pass a path.
//! Multipart, one file field, at most [`UPLOAD_LIMIT`]. Stored at
//! `uploads/<uuid>/<name>` under the workspace; that relative path is
//! returned for the message to mention.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::manager::Manager;

pub const UPLOAD_LIMIT: usize = 25 * 1024 * 1024;

pub fn routes() -> Router<Arc<Manager>> {
    Router::new().route(
        "/v1/uploads",
        // Room for the multipart framing around a file at the limit.
        post(upload).layer(DefaultBodyLimit::max(UPLOAD_LIMIT + 64 * 1024)),
    )
}

#[derive(Serialize, Deserialize)]
pub struct Uploaded {
    /// Relative to the workspace.
    pub path: String,
    pub size: u64,
}

struct UploadError(StatusCode, String);

impl IntoResponse for UploadError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> UploadError {
    UploadError(StatusCode::BAD_REQUEST, msg.into())
}

/// A file name safe to create: the last path component, without control
/// characters or a leading dot.
pub fn clean_name(raw: &str) -> Option<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let name: String = base
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .trim_start_matches('.')
        .chars()
        .take(200)
        .collect();
    (!name.is_empty()).then_some(name)
}

async fn upload(
    State(m): State<Arc<Manager>>,
    mut form: Multipart,
) -> Result<Json<Uploaded>, UploadError> {
    while let Some(mut field) = form.next_field().await.map_err(|e| bad(e.body_text()))? {
        let Some(raw) = field.file_name() else {
            continue;
        };
        let name = clean_name(raw).ok_or_else(|| bad("the file needs a name"))?;
        let rel = format!("uploads/{}/{name}", uuid::Uuid::now_v7());
        // The folder of the conversation the file is for, through the same
        // capability handle the file tools use: a symlinked `uploads` can't
        // redirect the write outside it.
        let scope = tern_core::workspace::WorkspaceScope::for_dir_guarded(
            m.rt.config.workspace_dir(),
            m.rt.deny.clone(),
        );
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    if bytes.len() + chunk.len() > UPLOAD_LIMIT {
                        return Err(UploadError(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!("the file is over {} MB", UPLOAD_LIMIT / (1024 * 1024)),
                        ));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => return Err(UploadError(e.status(), e.body_text())),
            }
        }
        let size = bytes.len() as u64;
        let (target, data) = (rel.clone(), bytes);
        let s2 = scope.clone();
        tokio::task::spawn_blocking(move || s2.write_file(&target, &data))
            .await
            .map_err(|e| UploadError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .map_err(|e| {
                let msg = scope.fail("upload", &rel, e);
                UploadError(StatusCode::INTERNAL_SERVER_ERROR, msg)
            })?;
        tracing::info!(path = %rel, size, "file uploaded");
        return Ok(Json(Uploaded { path: rel, size }));
    }
    Err(bad(
        "no file in the upload (send a multipart field with a filename)",
    ))
}

#[cfg(test)]
mod tests {
    use super::clean_name;

    #[test]
    fn names_lose_paths_and_dots() {
        assert_eq!(clean_name("report.pdf").as_deref(), Some("report.pdf"));
        assert_eq!(clean_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(clean_name("C:\\Users\\me\\a.txt").as_deref(), Some("a.txt"));
        assert_eq!(clean_name(".bashrc").as_deref(), Some("bashrc"));
        assert_eq!(clean_name("..").as_deref(), None);
        assert_eq!(clean_name("dir/").as_deref(), None);
        assert_eq!(clean_name("a\nb").as_deref(), Some("ab"));
    }
}
