//! Events broadcast to front ends while a run executes.

use serde::{Deserialize, Serialize};

use crate::approval::ApprovalOutcome;
use crate::types::Usage;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    RunStarted {
        run_id: String,
        session_id: String,
        /// Set for sub-agent runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_run_id: Option<String>,
    },
    /// Emitted on the parent's run when `agent_spawn` creates (not finds
    /// again after a crash) a child run.
    SubagentSpawned {
        /// The parent run.
        run_id: String,
        /// The `agent_spawn` tool call, matching its `tool_started`.
        call_id: String,
        child_run_id: String,
        child_session_id: String,
        label: String,
        /// The first 500 characters of the brief.
        task: String,
        model: String,
        /// `running` or `queued`.
        status: String,
    },
    /// A question, answer or steering message between runs of one tree
    /// (`agent_ask_parent`, `agent_send`).
    SubagentMessage {
        /// The run that sent it.
        run_id: String,
        to_run_id: String,
        /// `question`, `answer` or `message`.
        kind: String,
        text: String,
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
    /// Output produced by a running tool, before it finishes. Live only:
    /// never journaled or replayed (the full output arrives in
    /// `ToolFinished`).
    ToolOutput {
        run_id: String,
        /// The tool call id, matching `ToolStarted`.
        id: String,
        chunk: String,
    },
    ToolFinished {
        run_id: String,
        id: String,
        name: String,
        is_error: bool,
        preview: String,
        /// Full (already truncated to 32 KB) tool output.
        output: String,
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
    /// Old messages (ids <= `cover_through`) were summarized.
    Compacted {
        run_id: String,
        cover_through: i64,
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

impl AgentEvent {
    /// The run the event belongs to (for `subagent_spawned`, the parent;
    /// for `subagent_message`, the sender).
    pub fn run_id(&self) -> &str {
        match self {
            Self::RunStarted { run_id, .. }
            | Self::SubagentSpawned { run_id, .. }
            | Self::SubagentMessage { run_id, .. }
            | Self::TextDelta { run_id, .. }
            | Self::AssistantMessage { run_id, .. }
            | Self::ToolStarted { run_id, .. }
            | Self::ToolOutput { run_id, .. }
            | Self::ToolFinished { run_id, .. }
            | Self::Approval { run_id, .. }
            | Self::Retrying { run_id, .. }
            | Self::Compacted { run_id, .. }
            | Self::Usage { run_id, .. }
            | Self::RunFinished { run_id, .. } => run_id,
        }
    }
}
