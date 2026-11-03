//! The TUI's view of an embedded runtime. `/reload` swaps in a new one.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use tern_core::agent::RunOutcome;
use tern_core::approval::ApprovalHandler;
use tern_core::events::AgentEvent;
use tern_core::types::{ContentBlock, Role, Usage};
use tern_core::{SecretStatus, TernHome};
use tern_store::SessionSummary;
use tern_tui::{Backend, BackendResult, HistoryMessage, SessionInfo, Started};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::runtime::Runtime;

pub struct TuiBackend {
    home: TernHome,
    approvals: Arc<dyn ApprovalHandler>,
    rt: RwLock<Arc<Runtime>>,
}

impl TuiBackend {
    pub fn open(home: &TernHome, approvals: Arc<dyn ApprovalHandler>) -> anyhow::Result<Self> {
        let rt = Runtime::open(home, approvals.clone())?;
        Ok(Self {
            home: home.clone(),
            approvals,
            rt: RwLock::new(Arc::new(rt)),
        })
    }

    fn rt(&self) -> Arc<Runtime> {
        match self.rt.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// `anyhow` errors with their causes on one line.
fn chain(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[async_trait]
impl Backend for TuiBackend {
    fn default_model(&self) -> Option<String> {
        self.rt().pick_model(None, None).ok().map(|m| m.to_string())
    }

    fn check_model(&self, model: &str) -> BackendResult<()> {
        self.rt().check_model(model).map(|_| ()).map_err(chain)
    }

    fn start_run(
        &self,
        session: Option<&str>,
        model: Option<&str>,
        text: &str,
    ) -> BackendResult<Started> {
        let run = self
            .rt()
            .start_run(session, model, text, "tui", "user")
            .map_err(chain)?;
        Ok(Started {
            session_id: run.session_id,
            run_id: run.run_id,
            model: run.model.to_string(),
        })
    }

    async fn drive(
        &self,
        run_id: &str,
        resume: bool,
        cancel: CancellationToken,
    ) -> BackendResult<RunOutcome> {
        let rt = self.rt();
        let result = if resume {
            rt.agent.resume(run_id, cancel).await
        } else {
            rt.agent.drive(run_id, cancel).await
        };
        result.map_err(err)
    }

    fn set_session_model(&self, session: &str, model: &str) -> BackendResult<()> {
        self.rt()
            .store
            .set_session_model(session, model)
            .map_err(err)
    }

    fn sessions(&self, limit: usize) -> BackendResult<Vec<SessionSummary>> {
        self.rt().store.list_sessions(limit).map_err(err)
    }

    fn session(&self, id: &str) -> BackendResult<Option<(SessionInfo, Vec<HistoryMessage>)>> {
        let rt = self.rt();
        let Some(s) = rt.store.session(id).map_err(err)? else {
            return Ok(None);
        };
        let messages = rt
            .store
            .session_messages(id)
            .map_err(err)?
            .into_iter()
            .map(|m| HistoryMessage {
                role: if m.role == "assistant" {
                    Role::Assistant
                } else {
                    Role::User
                },
                content: serde_json::from_str::<Vec<ContentBlock>>(&m.content_json)
                    .unwrap_or_default(),
                summary: m.summary_of.is_some(),
            })
            .collect();
        Ok(Some((
            SessionInfo {
                id: s.id,
                title: s.title,
                model: s.model,
            },
            messages,
        )))
    }

    fn run_session(&self, run_id: &str) -> BackendResult<Option<String>> {
        Ok(self
            .rt()
            .store
            .run(run_id)
            .map_err(err)?
            .map(|r| r.session_id))
    }

    fn tools(&self) -> Vec<(String, String)> {
        self.rt()
            .agent
            .tools()
            .specs()
            .into_iter()
            .map(|t| (t.name, t.description))
            .collect()
    }

    fn usage(&self, session: &str) -> BackendResult<Usage> {
        let (input_tokens, output_tokens, cache_read_tokens, cache_write_tokens) =
            self.rt().store.session_usage(session).map_err(err)?;
        Ok(Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
        })
    }

    fn reload(&self) -> BackendResult<String> {
        let fresh = Runtime::open(&self.home, self.approvals.clone()).map_err(chain)?;
        let summary = format!(
            "reloaded: {} tools, {} providers configured",
            fresh.agent.tools().names().len(),
            fresh.config.providers.len()
        );
        match self.rt.write() {
            Ok(mut g) => *g = Arc::new(fresh),
            Err(poisoned) => *poisoned.into_inner() = Arc::new(fresh),
        }
        Ok(summary)
    }

    fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.rt().agent.subscribe()
    }

    fn secrets(&self) -> BackendResult<Vec<SecretStatus>> {
        Ok(self.rt().secret_statuses())
    }

    /// Applies to the next model call or tool use; no reload needed.
    fn set_secret(&self, name: &str, value: &str) -> BackendResult<()> {
        self.rt().secrets.set(name, value).map_err(err)
    }

    fn remove_secret(&self, name: &str) -> BackendResult<bool> {
        self.rt().secrets.remove(name).map_err(err)
    }
}

/// A core to attach to over the network (`tern --remote`).
pub struct RemoteTarget {
    pub url: String,
    pub token: String,
    pub pin: Option<String>,
}

/// `tern` with no subcommand: attach to `remote` if given, else to a
/// running daemon, else run embedded.
pub fn run(
    home: &TernHome,
    remote: Option<RemoteTarget>,
) -> anyhow::Result<std::process::ExitCode> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!("the chat UI needs a terminal; use `tern run \"<prompt>\"` for scripts");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        if let Some(t) = remote {
            let (remote, approvals) =
                crate::remote::connect_url(&t.url, &t.token, t.pin.as_deref()).await?;
            tracing::info!(url = %t.url, "attached to a remote core");
            return tern_tui::run(remote, approvals, None)
                .await
                .map_err(anyhow::Error::from);
        }
        let socket = home
            .socket_connect_path()
            .inspect_err(|e| tracing::warn!("not attaching to the daemon: {e}"));
        if let Some((remote, approvals)) = match &socket {
            Ok(p) => crate::remote::connect(p).await,
            Err(_) => None,
        } {
            tracing::info!("attached to the running daemon");
            return tern_tui::run(remote, approvals, None)
                .await
                .map_err(anyhow::Error::from);
        }
        let (approver, approvals) = tern_tui::TuiApprover::new();
        let backend = Arc::new(TuiBackend::open(home, approver)?);
        tern_tui::run(backend, approvals, None)
            .await
            .map_err(anyhow::Error::from)
    })?;
    Ok(std::process::ExitCode::SUCCESS)
}
