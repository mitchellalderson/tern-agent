//! Terminal chat front end (spec §12.2).
//!
//! The TUI talks to a [`Backend`], not to the runtime directly, so the same
//! UI can drive an embedded runtime now and the daemon API later.

mod app;
mod history;
mod input;
mod models;
mod secrets;
mod setup;
mod term;
mod ui;
mod ui_setup;
mod wrap;

use std::sync::Arc;

use async_trait::async_trait;
use tern_core::SecretStatus;
use tern_core::agent::RunOutcome;
use tern_core::approval::{ApprovalHandler, ApprovalRequest, Decision};
use tern_core::events::AgentEvent;
use tern_core::models::{ProviderDraft, ProviderModels, ProviderStatus, Resolved, Verified};
use tern_core::subagent::ChildRun;
use tern_core::types::{ContentBlock, Role, Usage};
use tern_store::SessionSummary;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub use app::App;
pub use term::run;

pub type BackendResult<T> = Result<T, String>;

const NO_SETUP: &str = "this backend can't manage providers";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub session_id: String,
    pub run_id: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub title: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    /// A compaction summary rather than a real turn.
    pub summary: bool,
}

#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// Model used for new sessions when none was chosen.
    fn default_model(&self) -> Option<String>;
    /// Validate a `provider:model` reference against the configuration.
    fn check_model(&self, model: &str) -> BackendResult<()>;
    /// Record a run for `text`, creating a session if `session` is `None`.
    fn start_run(
        &self,
        session: Option<&str>,
        model: Option<&str>,
        text: &str,
    ) -> BackendResult<Started>;
    /// Drive (or resume) a run to completion.
    async fn drive(
        &self,
        run_id: &str,
        resume: bool,
        cancel: CancellationToken,
    ) -> BackendResult<RunOutcome>;
    fn set_session_model(&self, session: &str, model: &str) -> BackendResult<()>;
    fn sessions(&self, limit: usize) -> BackendResult<Vec<SessionSummary>>;
    fn session(&self, id: &str) -> BackendResult<Option<(SessionInfo, Vec<HistoryMessage>)>>;
    /// The session a run belongs to, if `id` is a run id.
    fn run_session(&self, run_id: &str) -> BackendResult<Option<String>>;
    fn tools(&self) -> Vec<(String, String)>;
    fn usage(&self, session: &str) -> BackendResult<Usage>;
    /// Re-read configuration and rebuild tools and providers.
    fn reload(&self) -> BackendResult<String>;
    fn subscribe(&self) -> broadcast::Receiver<AgentEvent>;
    /// Every known secret, without values.
    fn secrets(&self) -> BackendResult<Vec<SecretStatus>> {
        Err("this backend can't manage secrets".into())
    }
    /// Store a secret in `secrets.toml`.
    fn set_secret(&self, _name: &str, _value: &str) -> BackendResult<()> {
        Err("this backend can't manage secrets".into())
    }
    /// Remove a secret from `secrets.toml`; false if it wasn't there.
    fn remove_secret(&self, _name: &str) -> BackendResult<bool> {
        Err("this backend can't manage secrets".into())
    }
    /// Resolve a model reference, filling in the provider when only one
    /// lists that model. The default only checks it.
    fn resolve_model(&self, model: &str) -> BackendResult<Resolved> {
        self.check_model(model)?;
        Ok(Resolved {
            model: model.to_owned(),
            warning: None,
        })
    }
    /// No usable model yet: the TUI opens provider setup.
    fn setup_needed(&self) -> bool {
        false
    }
    /// Configured providers, without keys.
    fn providers(&self) -> BackendResult<Vec<ProviderStatus>> {
        Err(NO_SETUP.into())
    }
    /// Model lists for every provider, or one. May call the providers.
    async fn models(
        &self,
        _provider: Option<String>,
        _refresh: bool,
    ) -> BackendResult<Vec<ProviderModels>> {
        Err(NO_SETUP.into())
    }
    /// Check a new provider (or a change to `existing`) without saving.
    async fn verify_provider(
        &self,
        _draft: ProviderDraft,
        _existing: Option<String>,
    ) -> BackendResult<Verified> {
        Err(NO_SETUP.into())
    }
    /// Verify, store the key, write the config and reload providers.
    async fn save_provider(
        &self,
        _draft: ProviderDraft,
        _existing: Option<String>,
        _default_model: Option<String>,
    ) -> BackendResult<ProviderStatus> {
        Err(NO_SETUP.into())
    }
    fn remove_provider(&self, _name: &str, _remove_secret: bool) -> BackendResult<()> {
        Err(NO_SETUP.into())
    }
    /// Set `default_model`; returns the resolved `provider:model`.
    fn set_default_model(&self, _model: &str) -> BackendResult<Resolved> {
        Err(NO_SETUP.into())
    }
    /// The sub-agents `run_id` started, oldest first.
    fn children(&self, _run_id: &str) -> BackendResult<Vec<ChildRun>> {
        Err("this backend doesn't support sub-agents".into())
    }
    /// Send the user's `text` to a running sub-agent, delivered at its next
    /// step.
    fn message_run(&self, _run_id: &str, _text: &str) -> BackendResult<()> {
        Err("this backend doesn't support sub-agents".into())
    }
}

/// An approval the agent is waiting on, answered through `reply`.
pub struct PendingApproval {
    pub request: ApprovalRequest,
    pub reply: oneshot::Sender<Decision>,
}

/// `ApprovalHandler` that forwards requests to the TUI's event loop.
pub struct TuiApprover {
    tx: mpsc::Sender<PendingApproval>,
}

impl TuiApprover {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<PendingApproval>) {
        let (tx, rx) = mpsc::channel(8);
        (Arc::new(Self { tx }), rx)
    }
}

#[async_trait]
impl ApprovalHandler for TuiApprover {
    async fn ask(&self, request: &ApprovalRequest) -> Decision {
        let (reply, rx) = oneshot::channel();
        let pending = PendingApproval {
            request: request.clone(),
            reply,
        };
        if self.tx.send(pending).await.is_err() {
            return Decision::Deny;
        }
        rx.await.unwrap_or(Decision::Deny)
    }
}
