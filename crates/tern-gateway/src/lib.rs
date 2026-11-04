//! Messaging gateways (spec §12.4). A [`Gateway`] connects a platform to the
//! daemon's [`ChatHost`]; Telegram is the first implementation.

pub mod builtin;
pub mod lua;
pub mod telegram;

/// Which agent profile a chat talks to (agent profiles §6.4): `/agent` in
/// a chat (or a forum topic) sets it. Kept in the store, so the daemon
/// finds the chat's session and the gateway its reply prefix.
pub mod chat_agents {
    use tern_store::{DEFAULT_AGENT, Store};

    const NS: &str = "chat_agent";

    /// The agent `chat` (on `source`) talks to: the default agent unless
    /// `/agent` chose an active other one.
    pub fn get(store: &Store, source: &str, chat: &str) -> String {
        store
            .kv_get(NS, &format!("{source}:{chat}"))
            .ok()
            .flatten()
            .filter(|id| {
                store
                    .agent(id)
                    .ok()
                    .flatten()
                    .is_some_and(|a| a.archived_at.is_none())
            })
            .unwrap_or_else(|| DEFAULT_AGENT.into())
    }

    pub fn set(store: &Store, source: &str, chat: &str, agent: &str) -> tern_store::Result<()> {
        let key = format!("{source}:{chat}");
        if agent == DEFAULT_AGENT {
            store.kv_delete(NS, &key)
        } else {
            store.kv_set(NS, &key, agent)
        }
    }

    /// The `external_ref` of the chat's sessions with `agent`: the chat
    /// itself for the default agent (as before profiles), else
    /// `<chat>|<agent id>`, so each agent has its own stream in the chat.
    pub fn session_ref(chat: &str, agent: &str) -> String {
        if agent == DEFAULT_AGENT {
            chat.to_owned()
        } else {
            format!("{chat}|{agent}")
        }
    }
}

use std::sync::Arc;

use async_trait::async_trait;
use tern_core::approval::{ApprovalRequest, AuthPrompt, Decision, SecretRequest};
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
    /// A message from someone who isn't the paired owner, in a chat that
    /// lets strangers in (tooling spec §5.3, §6.4): it starts a run that is
    /// tainted for `taint` ("message from Ann on slack"), in a session of
    /// its own so it never joins the owner's run. Returns the run id.
    async fn send_untrusted(
        &self,
        _source: &str,
        _chat: &str,
        _text: &str,
        _taint: &str,
    ) -> Result<String, String> {
        Err("this tern doesn't take messages from strangers".into())
    }
    /// Approve (run it, exactly once) or reject a pending outbox item from
    /// a chat button. Returns how it ended, in a few words.
    async fn decide_outbox(&self, _id: i64, _approve: bool) -> Result<String, String> {
        Err("this tern can't decide outbox items".into())
    }
    /// Items waiting for the user (`/outbox`), oldest first.
    async fn pending_outbox(&self) -> Vec<OutboxItem> {
        Vec::new()
    }
}

#[async_trait]
pub trait Gateway: Send + Sync {
    /// Target prefix this gateway owns, e.g. `telegram` for `telegram:<chat>`.
    fn name(&self) -> &str;
    /// What platform it is (`telegram`, `ntfy`, `slack`), for front ends.
    fn kind(&self) -> &str {
        self.name()
    }
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
    /// Tell the user in `target` that the agent wants them to choose how to
    /// connect a service or to sign in, which happens in the app or TUI
    /// (a chat can't open a browser on this machine). Same handle as
    /// [`Gateway::secret_request_notice`]; ended with `secret_request_done`.
    async fn auth_prompt_notice(&self, _target: &str, _req: &AuthPrompt) -> Option<String> {
        None
    }
    /// Show a new outbox item (a call nobody approved in time) in `target`,
    /// with Approve and Reject buttons unless only this host may decide it.
    async fn outbox_notice(&self, _target: &str, _item: &OutboxItem) {}
    /// An item was decided or expired: update any card showing it.
    async fn outbox_resolved(&self, _item: &OutboxItem) {}
}
