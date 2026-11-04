//! The outbox (tooling spec §5.2): a `write` or `destructive` call nobody
//! approved in time is queued here instead of denied, and the run carries
//! on. Approving it later executes the call exactly once, through an
//! `outbox_exec` step on a small system run in the same session.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tern_store::OutboxRow;

/// `runs.trigger` of the system run that executes an approved item.
pub const TRIGGER: &str = "outbox";

/// `agent_mail.kind` of tern's notes to a run still working when one of
/// its items is decided.
pub const MAIL_KIND: &str = "outbox";

/// What the model is told when its call goes to the outbox.
pub fn deferred_message(id: i64) -> String {
    format!(
        "queued for approval as outbox #{id}: nobody was there to approve it, so it hasn't run. \
         Continue with anything that doesn't depend on it, and say in your answer that it is waiting \
         for the user. If you are still running when the user decides, you'll be told."
    )
}

/// The note a running run gets when an item is decided.
pub fn decided_note(item: &OutboxRow, result: Option<&str>) -> String {
    match item.state.as_str() {
        "done" | "failed" => format!(
            "[outbox #{} was approved and ran: {}]\n{}",
            item.id,
            item.summary,
            result.unwrap_or_default()
        ),
        "rejected" => match item.note.as_deref().filter(|n| !n.trim().is_empty()) {
            Some(note) => format!(
                "[outbox #{} was rejected: {}]\nThe user said: {note}",
                item.id, item.summary
            ),
            None => format!("[outbox #{} was rejected: {}]", item.id, item.summary),
        },
        "expired" => format!(
            "[outbox #{} expired without an answer: {}]",
            item.id, item.summary
        ),
        _ => format!("[outbox #{}: {}]", item.id, item.summary),
    }
}

/// An outbox item as the API, Telegram and clients see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub id: i64,
    pub run_id: String,
    pub session_id: String,
    pub tool: String,
    /// The arguments it would run with (the user's edit, if any).
    pub input: Value,
    pub summary: String,
    pub effect: crate::tool::Effect,
    /// Why the run that queued it was tainted, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taint: Option<String>,
    /// Only a client on this host may approve it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local_only: bool,
    /// `pending`, `approved`, `done`, `failed`, `rejected` or `expired`.
    pub state: String,
    pub created_at: i64,
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The user changed the arguments before approving.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub edited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_run_id: Option<String>,
    /// The tool's result once it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<crate::tool::ToolResult>,
    /// The channel and agent of the session at the top of the run's tree
    /// (agent profiles §3): an agent's DM shows its own items as well as
    /// Home. Filled in by the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

impl From<OutboxRow> for OutboxItem {
    fn from(r: OutboxRow) -> Self {
        Self {
            id: r.id,
            input: serde_json::from_str(r.effective_input()).unwrap_or(Value::Null),
            edited: r.edited_input_json.is_some(),
            result: r
                .result_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok()),
            run_id: r.run_id,
            session_id: r.session_id,
            tool: r.tool,
            summary: r.summary,
            effect: crate::tool::Effect::parse(&r.effect).unwrap_or_default(),
            taint: r.taint,
            local_only: r.local_only,
            state: r.state,
            created_at: r.created_at,
            expires_at: r.expires_at,
            decided_at: r.decided_at,
            note: r.note,
            exec_run_id: r.exec_run_id,
            channel_id: None,
            agent_id: None,
        }
    }
}

impl OutboxItem {
    /// `write · gmail_send to bob@…`, as approval prompts show a call.
    pub fn headline(&self) -> String {
        format!("{} · {}", self.effect, self.summary)
    }
}
