//! Terminal chat front end (spec §12.2).
//!
//! The TUI talks to a [`Backend`], not to the runtime directly, so the same
//! UI can drive an embedded runtime now and the daemon API later.

mod app;
mod history;
mod input;
mod mcp;
mod models;
pub mod outbox;
mod secret_prompt;
mod secrets;
mod settings;
mod setup;
mod term;
mod ui;
mod ui_mcp;
mod ui_settings;
mod ui_setup;
mod wrap;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use tern_core::SecretStatus;
use tern_core::agent::RunOutcome;
use tern_core::approval::{
    ApprovalHandler, ApprovalRequest, AuthPrompt, Decision, SecretRequest, SecretValues,
};
use tern_core::config::McpServerConfig;
use tern_core::events::AgentEvent;
use tern_core::mcp::{ImportedServer, McpPatch, McpStatus, McpVerified, McpVerifyError};
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
const NO_SETTINGS: &str = "this backend can't edit settings";
const NO_MCP: &str = "this backend can't manage MCP servers";
const NO_CHANNELS: &str =
    "channels need the background agent: start it with `tern start`, then reopen tern";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub session_id: String,
    pub run_id: String,
    pub model: String,
}

/// An agent profile (docs/agent-profiles-design.md), as `/agents` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    pub id: String,
    pub slug: String,
    pub name: String,
    /// Approvals and secret requests from its runs.
    pub needs_you: usize,
    pub active_runs: usize,
    pub is_default: bool,
}

/// A channel, for `/channels` and `/channel` (channels-design C5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub id: String,
    pub slug: String,
    pub title: String,
    /// `main` (Home, a DM) or `topic`.
    pub kind: String,
    /// Open needs-you posts; Home's counts everything waiting on you.
    pub needs_you: usize,
    pub active_runs: usize,
}

/// What `/agents` and `/channel` can rename and delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Managed {
    Agent,
    Channel,
}

/// What to do with a deleted agent's routines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteChoice {
    /// Hand its routines (and led channels) to this agent.
    pub reassign_to: Option<String>,
    /// Delete its routines instead.
    pub delete_routines: bool,
}

/// A post, as the TUI prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostLine {
    pub id: i64,
    pub author: String,
    pub author_kind: String,
    pub kind: String,
    pub text: String,
    pub run_id: Option<String>,
    pub needs_you: bool,
    /// A `channel_suggest` card: `(suggestion id, slug, pending)`.
    pub suggestion: Option<(i64, String, bool)>,
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
    /// Accounts of connectors and MCP servers, one line each (`/connections`).
    fn connections(&self) -> BackendResult<Vec<String>> {
        Err("this backend doesn't list connections".into())
    }

    /// `/skills`: skills, drafts and skill-capture offers. `args` is what
    /// follows the command (`offers`, `save 3`, `publish name`, ...); the
    /// answer is shown as a notice.
    fn skills(&self, _args: &str) -> BackendResult<String> {
        Err("skill capture needs the background agent; start it with `tern start`".into())
    }

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
    /// Outbox items waiting for the user, newest first.
    fn outbox(&self) -> BackendResult<Vec<tern_core::outbox::OutboxItem>> {
        Err("this backend has no outbox".into())
    }
    /// Approve (with edited arguments) or reject outbox item `id`. Only
    /// the background agent runs items.
    fn decide_outbox(
        &self,
        _id: i64,
        _decide: outbox::Decide,
    ) -> BackendResult<tern_core::outbox::OutboxItem> {
        Err("outbox items run in the background agent; start it with `tern start`".into())
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
    /// Every platform setting in `config.toml`, as the core describes it
    /// (`/settings`).
    fn config_view(&self) -> BackendResult<tern_core::config::settings::ConfigView> {
        Err(NO_SETTINGS.into())
    }
    /// Set `key` to `value`, or back to its default with `None`. The core
    /// checks it and keeps the file's comments; the error says what is wrong.
    fn change_setting(
        &self,
        _key: &str,
        _value: Option<serde_json::Value>,
    ) -> BackendResult<tern_core::config::settings::Setting> {
        Err(NO_SETTINGS.into())
    }
    /// Configured MCP servers with their state.
    fn mcp_servers(&self) -> BackendResult<Vec<McpStatus>> {
        Err(NO_MCP.into())
    }
    /// Start `cfg` as a throwaway server and list its tools; saves nothing.
    /// `secrets` are values typed but not saved yet. The outer error is the
    /// backend failing, the inner one the server.
    async fn verify_mcp(
        &self,
        _name: String,
        _cfg: McpServerConfig,
        _secrets: BTreeMap<String, String>,
    ) -> BackendResult<Result<McpVerified, McpVerifyError>> {
        Err(NO_MCP.into())
    }
    /// Read a pasted `mcpServers` JSON snippet into drafts.
    fn import_mcp(&self, _text: &str) -> BackendResult<Vec<ImportedServer>> {
        Err(NO_MCP.into())
    }
    /// Store `secrets`, write the config and start the server. `old` is the
    /// server being replaced (renamed when it differs from `name`).
    async fn save_mcp(
        &self,
        _old: Option<String>,
        _name: String,
        _cfg: McpServerConfig,
        _secrets: BTreeMap<String, String>,
    ) -> BackendResult<McpStatus> {
        Err(NO_MCP.into())
    }
    /// The quick toggles: enabled, hidden tools, approval, retry.
    fn patch_mcp(&self, _name: &str, _patch: &McpPatch) -> BackendResult<McpStatus> {
        Err(NO_MCP.into())
    }
    fn remove_mcp(&self, _name: &str, _remove_secrets: bool) -> BackendResult<()> {
        Err(NO_MCP.into())
    }
    /// Reconnect now, ignoring backoff.
    async fn restart_mcp(&self, _name: String) -> BackendResult<McpStatus> {
        Err(NO_MCP.into())
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
    /// Agent profiles, the default first. Empty when the core has none.
    fn agents(&self) -> BackendResult<Vec<AgentInfo>> {
        Ok(Vec::new())
    }
    /// Channels, Home first.
    fn channels(&self) -> BackendResult<Vec<ChannelInfo>> {
        Err(NO_CHANNELS.into())
    }
    /// Create topic channel `slug`.
    fn create_channel(&self, _slug: &str, _title: Option<&str>) -> BackendResult<ChannelInfo> {
        Err(NO_CHANNELS.into())
    }
    /// Rename agent or topic channel `id`: a new @name / #name (`slug`)
    /// and/or display name (`title`). What changed, in words.
    fn rename(
        &self,
        _what: Managed,
        _id: &str,
        _slug: Option<&str>,
        _title: Option<&str>,
    ) -> BackendResult<String> {
        Err("this backend can't rename".into())
    }
    /// What deleting `id` would touch, in words; an error when it can't
    /// be deleted now (and why).
    fn delete_preview(&self, _what: Managed, _id: &str) -> BackendResult<String> {
        Err("this backend can't delete".into())
    }
    /// Delete `id` (the user confirmed). What happened, in words.
    fn delete(&self, _what: Managed, _id: &str, _choice: &DeleteChoice) -> BackendResult<String> {
        Err("this backend can't delete".into())
    }
    /// Post `text` in topic channel `id`: the lead (or whoever it
    /// @mentions) answers; the started run is the first's.
    fn send_to_channel(
        &self,
        _id: &str,
        _text: &str,
        _model: Option<&str>,
    ) -> BackendResult<Started> {
        Err(NO_CHANNELS.into())
    }
    /// A channel's newest `limit` posts, oldest first.
    fn channel_posts(&self, _id: &str, _limit: usize) -> BackendResult<Vec<PostLine>> {
        Err(NO_CHANNELS.into())
    }
    /// Accept or decline a channel suggestion; what happened, in words.
    fn decide_suggestion(&self, _id: i64, _accept: bool) -> BackendResult<String> {
        Err(NO_CHANNELS.into())
    }
    /// Send `text` into agent `id`'s DM: its current thread, or a new one
    /// with `new_topic`.
    fn send_to_agent(
        &self,
        _id: &str,
        _text: &str,
        _model: Option<&str>,
        _new_topic: bool,
    ) -> BackendResult<Started> {
        Err("this backend doesn't support agent profiles".into())
    }
}

/// An approval the agent is waiting on, answered through `reply`.
pub struct PendingApproval {
    pub request: ApprovalRequest,
    pub reply: oneshot::Sender<Decision>,
}

/// Secrets the agent asked for (`secret_request`), answered through
/// `reply` with the values entered. Dropping `reply` answers nothing, for
/// prompts closed because someone else answered.
pub struct PendingSecret {
    pub request: SecretRequest,
    pub reply: oneshot::Sender<SecretValues>,
}

/// A sign-in card (`auth_propose`, `connection_add`), answered through
/// `reply` with an option's id, `device` or `cancel`. Dropping `reply`
/// answers nothing (the sign-in finished in the browser).
pub struct PendingAuth {
    pub request: AuthPrompt,
    pub reply: oneshot::Sender<String>,
}

/// Something the agent is waiting on the user for.
pub enum UserPrompt {
    Approval(PendingApproval),
    Secret(PendingSecret),
    Auth(PendingAuth),
    /// Close sign-in card `id`: it was answered elsewhere or is over.
    AuthClosed(String),
}

/// `ApprovalHandler` that forwards requests to the TUI's event loop.
pub struct TuiApprover {
    tx: mpsc::Sender<UserPrompt>,
}

impl TuiApprover {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<UserPrompt>) {
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
        if self.tx.send(UserPrompt::Approval(pending)).await.is_err() {
            return Decision::Deny;
        }
        rx.await.unwrap_or(Decision::Deny)
    }

    async fn ask_secrets(&self, request: &SecretRequest) -> Option<SecretValues> {
        let (reply, rx) = oneshot::channel();
        let pending = PendingSecret {
            request: request.clone(),
            reply,
        };
        self.tx.send(UserPrompt::Secret(pending)).await.ok()?;
        // A prompt closed without an answer counts as skipped.
        Some(rx.await.unwrap_or_default())
    }

    async fn ask_auth(&self, request: &AuthPrompt) -> Option<String> {
        let (reply, rx) = oneshot::channel();
        let pending = PendingAuth {
            request: request.clone(),
            reply,
        };
        self.tx.send(UserPrompt::Auth(pending)).await.ok()?;
        // Closed without an answer: cancelled.
        Some(rx.await.unwrap_or_else(|_| "cancel".into()))
    }
}
