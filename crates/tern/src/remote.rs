//! The TUI's view of a running daemon, over its Unix socket API, or over
//! HTTPS with a device token (`tern --remote <url>`).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tern_core::SecretStatus;
use tern_core::agent::RunOutcome;
use tern_core::approval::{ApprovalRequest, Decision};
use tern_core::events::AgentEvent;
use tern_core::types::{Role, ToolCall, Usage};
use tern_store::{SessionRow, SessionSummary};
use tern_tui::{Backend, BackendResult, HistoryMessage, PendingApproval, SessionInfo, Started};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::daemon::api::{self, ApiEvent};

const BASE: &str = "http://tern";

pub struct Remote {
    http: reqwest::Client,
    base: String,
    events: broadcast::Sender<AgentEvent>,
}

type Attached = (Arc<Remote>, mpsc::Receiver<PendingApproval>);

/// An error and its sources: reqwest's own message hides TLS failures such
/// as a pin mismatch.
fn causes(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        out.push_str(&format!(": {s}"));
        source = s.source();
    }
    out
}

/// Connect if a daemon answers on `socket`. Returns the backend and the
/// channel its approval requests arrive on.
pub async fn connect(socket: &Path) -> Option<Attached> {
    if !socket.exists() {
        return None;
    }
    let http = reqwest::Client::builder()
        .unix_socket(socket)
        .build()
        .ok()?;
    attach(http, BASE.into()).await.ok()
}

/// Connect to a core over HTTPS with a device token, pinning its
/// certificate when `pin` is given (deployment spec §7.2).
pub async fn connect_url(url: &str, token: &str, pin: Option<&str>) -> anyhow::Result<Attached> {
    let mut headers = reqwest::header::HeaderMap::new();
    let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?;
    auth.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, auth);
    let http = crate::daemon::tls::client(pin)?
        .default_headers(headers)
        .build()?;
    attach(http, url.trim_end_matches('/').to_owned())
        .await
        .map_err(anyhow::Error::msg)
}

async fn attach(http: reqwest::Client, base: String) -> BackendResult<Attached> {
    let (events, _) = broadcast::channel(1024);
    let (approvals_tx, approvals_rx) = mpsc::channel(8);
    let remote = Arc::new(Remote { http, base, events });
    remote
        .request::<Value>(
            remote
                .http
                .get(remote.url("/v1/health"))
                .timeout(Duration::from_secs(5)),
        )
        .await?;
    // Subscribe before returning, so no event of the first run is missed.
    let resp = remote
        .http
        .get(remote.url("/v1/events"))
        .send()
        .await
        .map_err(|e| format!("daemon: {}", causes(&e)))?;
    if !resp.status().is_success() {
        return Err(format!("events: HTTP {}", resp.status()));
    }
    tokio::spawn(pump(remote.clone(), resp, approvals_tx));
    Ok((remote, approvals_rx))
}

/// Read the SSE stream: agent events go to subscribers, approval requests
/// to the TUI, whose answers are posted back.
async fn pump(
    remote: Arc<Remote>,
    resp: reqwest::Response,
    approvals: mpsc::Sender<PendingApproval>,
) {
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    while let Some(Ok(chunk)) = stream.next().await {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buf.find("\n\n") {
            let frame: String = buf.drain(..end + 2).collect();
            let data: String = frame
                .lines()
                .filter_map(|l| {
                    l.strip_prefix("data:")
                        .map(|d| d.strip_prefix(' ').unwrap_or(d))
                })
                .collect::<Vec<_>>()
                .join("\n");
            match serde_json::from_str::<ApiEvent>(&data) {
                Ok(ApiEvent::Agent(ev)) => {
                    let _ = remote.events.send(ev);
                }
                // The TUI re-reads providers and models when it needs them.
                Ok(ApiEvent::Change(_)) => {}
                Ok(ApiEvent::Approval {
                    approval_requested: a,
                }) => {
                    let (reply, rx) = oneshot::channel();
                    let request = ApprovalRequest {
                        id: a.id.clone(),
                        run_id: a.run_id,
                        session_id: a.session_id,
                        call: ToolCall {
                            id: String::new(),
                            name: a.tool,
                            input: a.input,
                        },
                        summary: a.summary,
                        pattern: a.pattern,
                    };
                    if approvals
                        .send(PendingApproval { request, reply })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let http = remote.http.clone();
                    let url = remote.url(&format!("/v1/approvals/{}", a.id));
                    tokio::spawn(async move {
                        let decision = match rx.await.unwrap_or(Decision::Deny) {
                            Decision::AllowOnce => "once",
                            Decision::AlwaysAllow => "always",
                            Decision::Deny => "deny",
                        };
                        let _ = http
                            .post(url)
                            .json(&json!({ "decision": decision }))
                            .send()
                            .await;
                    });
                }
                Err(_) => {}
            }
        }
    }
    tracing::warn!("lost the daemon event stream");
}

impl Remote {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn request<T: DeserializeOwned>(&self, req: reqwest::RequestBuilder) -> BackendResult<T> {
        let resp = req
            .send()
            .await
            .map_err(|e| format!("daemon: {}", causes(&e)))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // No retry: the token was revoked or never valid.
            return Err(format!(
                "signed out of {}: this device's token was revoked or is unknown; pair it again",
                self.base
            ));
        }
        let body: Value = resp.json().await.map_err(|e| format!("daemon: {e}"))?;
        if !status.is_success() {
            return Err(body["error"]
                .as_str()
                .unwrap_or("daemon request failed")
                .to_owned());
        }
        serde_json::from_value(body).map_err(|e| format!("daemon: {e}"))
    }

    /// The TUI calls most backend methods synchronously from async code.
    fn blocking<T>(
        &self,
        fut: impl std::future::Future<Output = BackendResult<T>>,
    ) -> BackendResult<T> {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> BackendResult<T> {
        self.blocking(self.request(self.http.get(self.url(path))))
    }

    fn send_json<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Value,
    ) -> BackendResult<T> {
        self.blocking(self.request(self.http.request(method, self.url(path)).json(&body)))
    }
}

#[async_trait]
impl Backend for Remote {
    fn default_model(&self) -> Option<String> {
        self.get::<api::Info>("/v1/info")
            .ok()
            .and_then(|i| i.default_model)
    }

    fn check_model(&self, model: &str) -> BackendResult<()> {
        self.send_json::<Value>(
            reqwest::Method::POST,
            "/v1/models/check",
            json!({ "model": model }),
        )
        .map(|_| ())
    }

    fn start_run(
        &self,
        session: Option<&str>,
        model: Option<&str>,
        text: &str,
    ) -> BackendResult<Started> {
        let s: api::Started = self.send_json(
            reqwest::Method::POST,
            "/v1/runs",
            json!({ "text": text, "session_id": session, "model": model }),
        )?;
        Ok(Started {
            session_id: s.session_id,
            run_id: s.run_id,
            model: s.model,
        })
    }

    /// The daemon drives the run; this waits for it, relaying cancellation.
    async fn drive(
        &self,
        run_id: &str,
        resume: bool,
        cancel: CancellationToken,
    ) -> BackendResult<RunOutcome> {
        if resume {
            self.request::<Value>(
                self.http
                    .post(self.url(&format!("/v1/runs/{run_id}/resume"))),
            )
            .await?;
        }
        let wait =
            self.request::<RunOutcome>(self.http.get(self.url(&format!("/v1/runs/{run_id}/wait"))));
        tokio::pin!(wait);
        tokio::select! {
            r = &mut wait => return r,
            () = cancel.cancelled() => {}
        }
        let _ = self
            .http
            .post(self.url(&format!("/v1/runs/{run_id}/cancel")))
            .send()
            .await;
        wait.await
    }

    fn set_session_model(&self, session: &str, model: &str) -> BackendResult<()> {
        self.send_json::<Value>(
            reqwest::Method::PUT,
            &format!("/v1/sessions/{session}/model"),
            json!({ "model": model }),
        )
        .map(|_| ())
    }

    fn sessions(&self, limit: usize) -> BackendResult<Vec<SessionSummary>> {
        let items: Vec<api::SessionItem> = self.get(&format!("/v1/sessions?limit={limit}"))?;
        Ok(items
            .into_iter()
            .map(|i| SessionSummary {
                session: SessionRow {
                    id: i.id,
                    title: i.title,
                    source: i.source,
                    external_ref: None,
                    model: i.model,
                    created_at: i.updated_at,
                    updated_at: i.updated_at,
                },
                messages: i.messages,
                runs: i.runs,
            })
            .collect())
    }

    fn session(&self, id: &str) -> BackendResult<Option<(SessionInfo, Vec<HistoryMessage>)>> {
        match self.get::<api::SessionDetail>(&format!("/v1/sessions/{id}")) {
            Ok(d) => Ok(Some((
                SessionInfo {
                    id: d.id,
                    title: d.title,
                    model: d.model,
                },
                d.messages
                    .into_iter()
                    .map(|m| HistoryMessage {
                        role: if m.role == "assistant" {
                            Role::Assistant
                        } else {
                            Role::User
                        },
                        content: m.content,
                        summary: m.summary,
                    })
                    .collect(),
            ))),
            Err(e) if e.contains("not found") => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn run_session(&self, run_id: &str) -> BackendResult<Option<String>> {
        match self.get::<Value>(&format!("/v1/runs/{run_id}/session")) {
            Ok(v) => Ok(v["session_id"].as_str().map(str::to_owned)),
            Err(e) if e.contains("not found") => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn tools(&self) -> Vec<(String, String)> {
        self.get::<api::Info>("/v1/info")
            .map(|i| i.tools)
            .unwrap_or_default()
    }

    fn usage(&self, session: &str) -> BackendResult<Usage> {
        self.get(&format!("/v1/sessions/{session}/usage"))
    }

    fn reload(&self) -> BackendResult<String> {
        self.send_json::<Value>(reqwest::Method::POST, "/v1/config/reload", json!({}))?;
        Ok("reloaded providers and default model; restart `tern daemon` for other settings".into())
    }

    fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    fn secrets(&self) -> BackendResult<Vec<SecretStatus>> {
        self.get("/v1/secrets")
    }

    fn set_secret(&self, name: &str, value: &str) -> BackendResult<()> {
        self.send_json::<Value>(
            reqwest::Method::PUT,
            &format!("/v1/secrets/{name}"),
            json!({ "value": value }),
        )
        .map(|_| ())
    }

    fn remove_secret(&self, name: &str) -> BackendResult<bool> {
        match self
            .blocking(self.request::<Value>(self.http.delete(format!("{BASE}/v1/secrets/{name}"))))
        {
            Ok(_) => Ok(true),
            Err(e) if e.contains("not found") => Ok(false),
            Err(e) => Err(e),
        }
    }
}
