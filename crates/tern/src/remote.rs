//! The TUI's view of a running daemon, over its Unix socket API, or over
//! HTTPS with a device token (`tern --remote <url>`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tern_core::SecretStatus;
use tern_core::agent::RunOutcome;
use tern_core::approval::{ApprovalRequest, Decision, SecretRequest};
use tern_core::config::McpServerConfig;
use tern_core::events::AgentEvent;
use tern_core::mcp::{ImportedServer, McpPatch, McpStatus, McpVerified, McpVerifyError};
use tern_core::models::{ProviderDraft, ProviderModels, ProviderStatus, Resolved, Verified};
use tern_core::subagent::ChildRun;
use tern_core::types::{Role, ToolCall, Usage};
use tern_store::{SessionRow, SessionSummary};
use tern_tui::{
    AgentInfo, Backend, BackendResult, HistoryMessage, PendingApproval, PendingSecret, SessionInfo,
    Started, UserPrompt,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::daemon::api::{self, ApiEvent};
use crate::daemon::manager::ApiSecretRequest;

const BASE: &str = "http://tern";

pub struct Remote {
    http: reqwest::Client,
    base: String,
    events: broadcast::Sender<AgentEvent>,
    /// The connection may answer secret requests (owner scope).
    owner: bool,
}

type Attached = (Arc<Remote>, mpsc::Receiver<UserPrompt>);

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
/// channel its approval and secret requests arrive on.
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
    let (prompts_tx, prompts_rx) = mpsc::channel(8);
    let probe = Remote {
        http,
        base,
        events,
        owner: false,
    };
    let health: Value = probe
        .request(
            probe
                .http
                .get(probe.url("/v1/health"))
                .timeout(Duration::from_secs(5)),
        )
        .await?;
    let remote = Arc::new(Remote {
        owner: health["auth"]["scope"] == "owner",
        ..probe
    });
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
    // Secret requests wait for minutes, so pick up ones opened before we
    // attached (the TUI ignores any that also arrive live).
    let pending: Vec<ApiSecretRequest> = remote
        .request(remote.http.get(remote.url("/v1/secret-requests")))
        .await
        .unwrap_or_default();
    {
        let (remote, prompts) = (remote.clone(), prompts_tx.clone());
        tokio::spawn(async move {
            for r in pending {
                if !forward_secret(&remote, &prompts, r).await {
                    return;
                }
            }
        });
    }
    tokio::spawn(pump(remote.clone(), resp, prompts_tx));
    Ok((remote, prompts_rx))
}

/// Read the SSE stream: agent events go to subscribers, approval and
/// secret requests to the TUI, whose answers are posted back.
async fn pump(remote: Arc<Remote>, resp: reqwest::Response, prompts: mpsc::Sender<UserPrompt>) {
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
                        effect: a.effect,
                        pattern: a.pattern,
                        parent_run_id: a.parent_run_id,
                        agent_label: a.agent_label,
                        taint: a.taint,
                        local_only: a.local_only,
                    };
                    if prompts
                        .send(UserPrompt::Approval(PendingApproval { request, reply }))
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
                Ok(ApiEvent::SecretRequest {
                    secret_requested: r,
                }) => {
                    if !forward_secret(&remote, &prompts, r).await {
                        return;
                    }
                }
                Err(_) => {}
            }
        }
    }
    tracing::warn!("lost the daemon event stream");
}

/// Show a secret request in the TUI and post its answer. Devices without
/// the owner scope can't answer (the daemon would refuse), so they don't get
/// the prompt. Returns false once the TUI has gone.
async fn forward_secret(
    remote: &Arc<Remote>,
    prompts: &mpsc::Sender<UserPrompt>,
    r: ApiSecretRequest,
) -> bool {
    if !remote.owner {
        return true;
    }
    let (reply, rx) = oneshot::channel();
    let request = SecretRequest {
        id: r.id.clone(),
        run_id: r.run_id,
        session_id: r.session_id,
        call_id: r.call_id,
        asks: r.secrets,
        reason: r.reason,
        replace: r.replace,
        parent_run_id: r.parent_run_id,
        agent_label: r.agent_label,
    };
    if prompts
        .send(UserPrompt::Secret(PendingSecret { request, reply }))
        .await
        .is_err()
    {
        return false;
    }
    let http = remote.http.clone();
    let url = remote.url(&format!("/v1/secret-requests/{}", r.id));
    tokio::spawn(async move {
        // Dropped unanswered: answered elsewhere, or over.
        let Ok(values) = rx.await else {
            return;
        };
        match http
            .post(url)
            .json(&json!({ "values": values }))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                tracing::warn!(status = %resp.status(), "answering a secret request failed")
            }
            Err(e) => tracing::warn!(error = %e, "answering a secret request failed"),
        }
    });
    true
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
                    channel_id: i.channel_id,
                    ..SessionRow::default()
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

    fn resolve_model(&self, model: &str) -> BackendResult<Resolved> {
        let v: Value = self.send_json(
            reqwest::Method::POST,
            "/v1/models/check",
            json!({ "model": model }),
        )?;
        Ok(Resolved {
            model: v["resolved"].as_str().unwrap_or(model).to_owned(),
            warning: v["warning"].as_str().map(str::to_owned),
        })
    }

    fn setup_needed(&self) -> bool {
        self.get::<api::Info>("/v1/info")
            .is_ok_and(|i| i.setup_needed)
    }

    fn providers(&self) -> BackendResult<Vec<ProviderStatus>> {
        self.get("/v1/providers")
    }

    async fn models(
        &self,
        provider: Option<String>,
        refresh: bool,
    ) -> BackendResult<Vec<ProviderModels>> {
        // Provider names are `[a-z0-9_-]`, so they need no escaping.
        let mut path = format!("/v1/models?refresh={refresh}");
        if let Some(p) = provider {
            path.push_str(&format!("&provider={p}"));
        }
        self.request(self.http.get(self.url(&path))).await
    }

    async fn verify_provider(
        &self,
        draft: ProviderDraft,
        existing: Option<String>,
    ) -> BackendResult<Verified> {
        let path = match existing {
            Some(name) => format!("/v1/providers/{name}/verify"),
            None => "/v1/providers/verify".into(),
        };
        self.request(self.http.post(self.url(&path)).json(&draft))
            .await
    }

    async fn save_provider(
        &self,
        draft: ProviderDraft,
        existing: Option<String>,
        default_model: Option<String>,
    ) -> BackendResult<ProviderStatus> {
        let mut body = serde_json::to_value(&draft).map_err(|e| e.to_string())?;
        body["default_model"] = json!(default_model);
        let req = match existing {
            Some(name) => self.http.patch(self.url(&format!("/v1/providers/{name}"))),
            None => self.http.post(self.url("/v1/providers")),
        };
        let v: Value = self.request(req.json(&body)).await?;
        serde_json::from_value(v["provider"].clone()).map_err(|e| format!("daemon: {e}"))
    }

    fn remove_provider(&self, name: &str, remove_secret: bool) -> BackendResult<()> {
        self.send_json::<Value>(
            reqwest::Method::DELETE,
            &format!("/v1/providers/{name}?remove_secret={remove_secret}"),
            json!({}),
        )
        .map(|_| ())
    }

    fn set_default_model(&self, model: &str) -> BackendResult<Resolved> {
        self.send_json(
            reqwest::Method::PUT,
            "/v1/config/default_model",
            json!({ "model": model }),
        )
    }

    fn set_secret(&self, name: &str, value: &str) -> BackendResult<()> {
        self.send_json::<Value>(
            reqwest::Method::PUT,
            &format!("/v1/secrets/{name}"),
            json!({ "value": value }),
        )
        .map(|_| ())
    }

    fn mcp_servers(&self) -> BackendResult<Vec<McpStatus>> {
        self.get("/v1/mcp")
    }

    async fn verify_mcp(
        &self,
        name: String,
        cfg: McpServerConfig,
        secrets: BTreeMap<String, String>,
    ) -> BackendResult<Result<McpVerified, McpVerifyError>> {
        let body = json!({ "name": name, "config": cfg, "secrets": secrets });
        let v: Value = self
            .request(self.http.post(self.url("/v1/mcp/verify")).json(&body))
            .await?;
        let bad = |e: serde_json::Error| format!("daemon: {e}");
        Ok(if v["ok"] == true {
            Ok(serde_json::from_value(v["verified"].clone()).map_err(bad)?)
        } else {
            Err(serde_json::from_value(v["error"].clone()).map_err(bad)?)
        })
    }

    fn import_mcp(&self, text: &str) -> BackendResult<Vec<ImportedServer>> {
        self.send_json(
            reqwest::Method::POST,
            "/v1/mcp/import",
            json!({ "text": text }),
        )
    }

    async fn save_mcp(
        &self,
        old: Option<String>,
        name: String,
        cfg: McpServerConfig,
        secrets: BTreeMap<String, String>,
    ) -> BackendResult<McpStatus> {
        let body = json!({ "name": name, "config": cfg, "secrets": secrets });
        let req = match old {
            Some(old) => self.http.put(self.url(&format!("/v1/mcp/{old}"))),
            None => self.http.post(self.url("/v1/mcp")),
        };
        self.request(req.json(&body)).await
    }

    fn patch_mcp(&self, name: &str, patch: &McpPatch) -> BackendResult<McpStatus> {
        let body = serde_json::to_value(patch).map_err(|e| e.to_string())?;
        self.send_json(reqwest::Method::PATCH, &format!("/v1/mcp/{name}"), body)
    }

    fn remove_mcp(&self, name: &str, remove_secrets: bool) -> BackendResult<()> {
        self.send_json::<Value>(
            reqwest::Method::DELETE,
            &format!("/v1/mcp/{name}?remove_secrets={remove_secrets}"),
            json!({}),
        )
        .map(|_| ())
    }

    async fn restart_mcp(&self, name: String) -> BackendResult<McpStatus> {
        self.request(self.http.post(self.url(&format!("/v1/mcp/{name}/restart"))))
            .await
    }

    fn children(&self, run_id: &str) -> BackendResult<Vec<ChildRun>> {
        self.get(&format!("/v1/runs/{run_id}/children"))
    }

    fn message_run(&self, run_id: &str, text: &str) -> BackendResult<()> {
        self.send_json::<Value>(
            reqwest::Method::POST,
            &format!("/v1/runs/{run_id}/message"),
            json!({ "text": text }),
        )
        .map(|_| ())
    }

    /// A core without the `agents` feature answers 404: no profiles.
    fn agents(&self) -> BackendResult<Vec<AgentInfo>> {
        let list: Vec<crate::daemon::api_agents::Agent> = match self.get("/v1/agents") {
            Ok(l) => l,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(list
            .into_iter()
            .map(|a| AgentInfo {
                id: a.id,
                slug: a.slug,
                name: a.name,
                needs_you: a.needs_you,
                active_runs: a.active_runs,
                is_default: a.is_default,
            })
            .collect())
    }

    /// Sent like the desktop sends into a DM, so cuts and queueing apply.
    fn send_to_agent(
        &self,
        id: &str,
        text: &str,
        model: Option<&str>,
        new_topic: bool,
    ) -> BackendResult<Started> {
        let list: Vec<crate::daemon::api_agents::Agent> = self.get("/v1/agents")?;
        let agent = list
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| format!("no agent {id}"))?;
        let channel = agent
            .main_channel_id
            .ok_or_else(|| format!("{} has no DM", agent.name))?;
        let sent: crate::daemon::channels::Sent = self.send_json(
            reqwest::Method::POST,
            &format!("/v1/channels/{channel}/posts"),
            json!({ "text": text, "new_topic": new_topic, "model": model }),
        )?;
        let model = model
            .map(str::to_owned)
            .or_else(|| agent.model.map(|c| c.primary().to_owned()))
            .or_else(|| self.default_model())
            .unwrap_or_default();
        Ok(Started {
            session_id: sent.session_id,
            run_id: sent.run_id,
            model,
        })
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
