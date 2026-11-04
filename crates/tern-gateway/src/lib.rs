//! Messaging gateways (spec §12.4). A [`Gateway`] connects a platform to the
//! daemon's [`ChatHost`]; Telegram is the first implementation.

pub mod telegram;

use std::sync::Arc;

use async_trait::async_trait;
use tern_core::approval::{ApprovalRequest, Decision, SecretRequest};
use tern_core::events::AgentEvent;
use tern_core::outbox::OutboxItem;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// What gateways need from the daemon. Chats are identified by the
/// gateway's source name (`"telegram"`) and a platform chat id.
#[async_trait]
pub trait ChatHost: Send + Sync {
    /// A user message: goes to the run in progress in this chat, if any
    /// (it reads it at its next model call), else starts a run for `text`
    /// in the chat's current session. Returns the run id either way.
    async fn send(&self, source: &str, chat: &str, text: &str) -> Result<String, String>;
    /// Start a new topic for the chat (`/new`): the next message begins a
    /// new session that carries a summary of this one over.
    async fn new_session(&self, source: &str, chat: &str) -> Result<(), String>;
    /// Show (`None`) or set the chat session's model. Returns the model now in use.
    async fn model(&self, source: &str, chat: &str, set: Option<&str>) -> Result<String, String>;
    /// Cancel the chat's run in progress. Returns false if nothing was running.
    async fn stop(&self, source: &str, chat: &str) -> bool;
    async fn status(&self, source: &str, chat: &str) -> String;
    fn events(&self) -> broadcast::Receiver<AgentEvent>;
    /// Approve (run it, exactly once) or reject a pending outbox item from
    /// a chat button. Returns how it ended, in a few words.
    async fn decide_outbox(&self, _id: i64, _approve: bool) -> Result<String, String> {
        Err("this tern can't decide outbox items".into())
    }
}

#[async_trait]
pub trait Gateway: Send + Sync {
    /// Target prefix this gateway owns, e.g. `telegram` for `telegram:<chat>`.
    fn name(&self) -> &str;
    /// Poll the platform and handle messages until cancelled.
    async fn run(self: Arc<Self>, host: Arc<dyn ChatHost>, cancel: CancellationToken);
    /// Send a run's final answer to `target` (the part after `name:`). If the
    /// gateway is streaming that run, the streamed message is finalized instead.
    async fn deliver(&self, target: &str, run_id: Option<&str>, text: &str) -> Result<(), String>;
    /// Ask the user in `target` to approve a tool call.
    async fn ask_approval(&self, target: &str, req: &ApprovalRequest) -> Decision;
    /// Tell the user in `target` that the agent is waiting for secrets they
    /// must enter in the app or TUI: a chat must never collect the value.
    /// Returns a handle for [`Gateway::secret_request_done`].
    async fn secret_request_notice(&self, _target: &str, _req: &SecretRequest) -> Option<String> {
        None
    }
    /// Replace the notice with how the request ended.
    async fn secret_request_done(&self, _target: &str, _handle: &str, _text: &str) {}
    /// Show a new outbox item (a call nobody approved in time) in `target`,
    /// with Approve and Reject buttons unless only this host may decide it.
    async fn outbox_notice(&self, _target: &str, _item: &OutboxItem) {}
}
