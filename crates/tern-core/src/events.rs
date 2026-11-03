//! Events broadcast to front ends while a run executes.

use serde::Serialize;

use crate::approval::ApprovalOutcome;
use crate::types::Usage;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    RunStarted {
        run_id: String,
        session_id: String,
    },
    /// Streaming text from a live model call.
    TextDelta {
        run_id: String,
        text: String,
    },
    /// A completed assistant message. `replayed` is true when it came from
    /// the journal (already shown before a crash), so it wasn't streamed.
    AssistantMessage {
        run_id: String,
        text: String,
        replayed: bool,
    },
    ToolStarted {
        run_id: String,
        id: String,
        name: String,
        summary: String,
    },
    ToolFinished {
        run_id: String,
        id: String,
        name: String,
        is_error: bool,
        preview: String,
        replayed: bool,
    },
    Approval {
        run_id: String,
        id: String,
        name: String,
        outcome: ApprovalOutcome,
    },
    Retrying {
        run_id: String,
        attempt: u32,
        delay_ms: u64,
        error: String,
    },
    Usage {
        run_id: String,
        usage: Usage,
    },
    RunFinished {
        run_id: String,
        status: String,
        error: Option<String>,
    },
}
