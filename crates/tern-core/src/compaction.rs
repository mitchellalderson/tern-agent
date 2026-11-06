//! Context compaction (spec §6): when the conversation nears the model's
//! context window, the oldest messages are summarized into one message.
//!
//! A summary is stored as a user message with `summary_of = N`. When building
//! context, the latest summary goes first and every non-summary message with
//! `id <= N` is dropped. Each new summary folds in the previous one.

use serde::Serialize;
use tern_store::MessageRow;

use crate::types::{ContentBlock, Message, Role, ToolSpec};

/// Messages kept verbatim after compaction (spec: the last 6).
pub const KEEP_RECENT: usize = 6;

/// Compact when the estimate exceeds this fraction of the context window.
pub const THRESHOLD: f64 = 0.75;

pub const SUMMARY_PREFIX: &str = "[Summary of the earlier conversation]";

/// System prompt for summarization calls. Exported so test providers can
/// recognize these requests.
pub const SYSTEM_PROMPT: &str = "You summarize conversations between a user and an AI agent so the agent can continue the work without the full history. Keep: the user's goals and preferences, decisions made, facts learned, files and commands involved and their results, open questions, and what remains to do. Be concise and factual. Write in English prose and bullet points; no preamble.";

pub const SUMMARY_MAX_TOKENS: u32 = 2048;

/// Rough token count: characters / 4.
pub fn estimate_tokens(system: &str, messages: &[Message], tools: &[ToolSpec]) -> u64 {
    let msg_chars: usize = messages.iter().map(|m| m.search_text().len()).sum();
    let tool_chars: usize = tools
        .iter()
        .map(|t| t.name.len() + t.description.len() + t.input_schema.to_string().len())
        .sum();
    ((system.len() + msg_chars + tool_chars) / 4) as u64
}

/// Which rows to summarize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// `rows[..split]` get summarized; `rows[split..]` are kept.
    pub split: usize,
    /// Highest non-summary message id being summarized.
    pub cover_through: i64,
}

/// Journal input for a compaction step.
#[derive(Debug, Serialize)]
pub struct StepInput {
    pub cover_through: i64,
    pub transcript_digest: String,
}

/// Plan a compaction of effective context `rows`, or `None` if there is
/// nothing worth summarizing. Never separates a tool result from the
/// assistant message that requested it.
pub fn plan(rows: &[MessageRow]) -> Option<Plan> {
    if rows.len() <= KEEP_RECENT + 1 {
        return None;
    }
    let mut split = rows.len() - KEEP_RECENT;
    while split > 0 && is_tool_result(&rows[split]) {
        split -= 1;
    }
    let cover_through = rows[..split]
        .iter()
        .filter(|r| r.summary_of.is_none())
        .map(|r| r.id)
        .max()?;
    Some(Plan {
        split,
        cover_through,
    })
}

fn is_tool_result(row: &MessageRow) -> bool {
    row.role == "user"
        && serde_json::from_str::<Vec<ContentBlock>>(&row.content_json).is_ok_and(|c| {
            c.iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
        })
}

/// The latest summary first, then every message it doesn't cover.
pub fn effective(rows: Vec<MessageRow>) -> Vec<MessageRow> {
    let latest = rows
        .iter()
        .filter(|r| r.summary_of.is_some())
        .max_by_key(|r| r.id)
        .cloned();
    let Some(summary) = latest else {
        return rows;
    };
    let cover = summary.summary_of.unwrap_or(i64::MIN);
    let mut out = vec![summary];
    out.extend(
        rows.into_iter()
            .filter(|r| r.summary_of.is_none() && r.id > cover),
    );
    out
}

/// Render messages as a plain transcript for the summarizer.
pub fn transcript(messages: &[Message]) -> String {
    const MAX_BLOCK: usize = 4000;
    let clip = |s: &str| -> String {
        if s.len() <= MAX_BLOCK {
            s.to_owned()
        } else {
            crate::tool::truncate_middle(s, MAX_BLOCK)
        }
    };
    let mut out = String::new();
    for m in messages {
        out.push_str(match m.role {
            Role::User => "### user\n",
            Role::Assistant => "### assistant\n",
        });
        for b in &m.content {
            match b {
                ContentBlock::Text { text } => out.push_str(&clip(text)),
                ContentBlock::ToolUse { name, input, .. } => {
                    out.push_str(&format!(
                        "[called {name} with {}]",
                        clip(&input.to_string())
                    ));
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let tag = if *is_error {
                        "tool error"
                    } else {
                        "tool result"
                    };
                    out.push_str(&format!("[{tag}: {}]", clip(content)));
                }
                // Reasoning isn't part of what happened.
                ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => continue,
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

pub fn summary_request(transcript: &str) -> Message {
    Message::user_text(format!(
        "Summarize this conversation so far. It will replace these messages in the agent's context.\n\n{transcript}"
    ))
}

pub fn summary_message(summary: &str) -> Message {
    Message::user_text(format!("{SUMMARY_PREFIX}\n\n{}", summary.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: i64, role: &str, content: serde_json::Value, summary_of: Option<i64>) -> MessageRow {
        MessageRow {
            id,
            session_id: "s".into(),
            run_id: None,
            step_idx: None,
            role: role.into(),
            content_json: content.to_string(),
            text: String::new(),
            created_at: 0,
            summary_of,
        }
    }
    fn text(id: i64, role: &str) -> MessageRow {
        row(id, role, json!([{"type": "text", "text": "x"}]), None)
    }
    fn result(id: i64) -> MessageRow {
        row(
            id,
            "user",
            json!([{"type": "tool_result", "tool_use_id": "t", "content": "ok"}]),
            None,
        )
    }

    #[test]
    fn nothing_to_do_for_short_conversations() {
        let rows: Vec<_> = (1..=7).map(|i| text(i, "user")).collect();
        assert_eq!(plan(&rows), None);
    }

    #[test]
    fn keeps_last_six() {
        let rows: Vec<_> = (1..=10)
            .map(|i| text(i, if i % 2 == 0 { "assistant" } else { "user" }))
            .collect();
        assert_eq!(
            plan(&rows),
            Some(Plan {
                split: 4,
                cover_through: 4
            })
        );
    }

    #[test]
    fn never_splits_a_tool_call_from_its_result() {
        // ids: 1 u, 2 a, 3 u, 4 a(tool_use), 5 result, 6 a, 7 u, 8 a, 9 u, 10 a
        let mut rows: Vec<_> = (1..=10)
            .map(|i| text(i, if i % 2 == 0 { "assistant" } else { "user" }))
            .collect();
        rows[4] = result(5);
        // Naive split at index 4 would keep the result (id 5) but summarize its call (id 4).
        assert_eq!(
            plan(&rows),
            Some(Plan {
                split: 3,
                cover_through: 3
            })
        );
    }

    #[test]
    fn effective_puts_latest_summary_first_and_drops_covered() {
        let rows = vec![
            text(1, "user"),
            text(2, "assistant"),
            text(3, "user"),
            row(4, "user", json!([{"type": "text", "text": "S1"}]), Some(1)),
            text(5, "assistant"),
            row(6, "user", json!([{"type": "text", "text": "S2"}]), Some(2)),
        ];
        let ids: Vec<i64> = effective(rows).iter().map(|r| r.id).collect();
        assert_eq!(ids, [6, 3, 5]);
    }

    #[test]
    fn second_compaction_keeps_messages_older_than_first_summary() {
        // After compaction 1 (summary id 16 covering ..=9), effective rows are
        // [S1, 10..=15, 17..=20]. Compaction 2 must not drop 14 and 15.
        let mut rows = vec![row(
            16,
            "user",
            json!([{"type": "text", "text": "S1"}]),
            Some(9),
        )];
        rows.extend(
            (10..=15)
                .chain(17..=20)
                .map(|i| text(i, if i % 2 == 0 { "assistant" } else { "user" })),
        );
        let p = plan(&rows).unwrap();
        assert_eq!(p.cover_through, 13);
        let mut all = rows.clone();
        all.push(row(
            21,
            "user",
            json!([{"type": "text", "text": "S2"}]),
            Some(p.cover_through),
        ));
        let ids: Vec<i64> = effective(all).iter().map(|r| r.id).collect();
        assert_eq!(ids, [21, 14, 15, 17, 18, 19, 20]);
    }

    #[test]
    fn transcript_marks_roles_and_tools() {
        let t = transcript(&[
            Message::user_text("hi"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "1".into(),
                    name: "ls".into(),
                    input: json!({}),
                }],
            },
        ]);
        assert!(t.contains("### user\nhi"));
        assert!(t.contains("### assistant\n[called ls with {}]"));
    }
}
