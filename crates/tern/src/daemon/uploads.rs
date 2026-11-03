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
use tokio::io::AsyncWriteExt;

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
        let path = m.rt.config.workspace_dir().join(&rel);
        let internal =
            |e: std::io::Error| UploadError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await.map_err(internal)?;
        }
        let mut file = tokio::fs::File::create(&path).await.map_err(internal)?;
        let mut size = 0u64;
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    size += chunk.len() as u64;
                    file.write_all(&chunk).await.map_err(internal)?;
                }
                Ok(None) => break,
                Err(e) => {
                    drop(file);
                    let _ = tokio::fs::remove_file(&path).await;
                    if let Some(dir) = path.parent() {
                        let _ = tokio::fs::remove_dir(dir).await;
                    }
                    return Err(UploadError(e.status(), e.body_text()));
                }
            }
        }
        file.flush().await.map_err(internal)?;
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
