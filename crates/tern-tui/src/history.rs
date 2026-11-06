//! Rebuild transcript entries from a stored session.

use tern_core::types::{ContentBlock, Role, ToolCall};

use crate::HistoryMessage;
use crate::app::{Entry, ToolState};

pub fn entries(messages: &[HistoryMessage]) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for m in messages {
        if m.summary {
            let text: String = m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            out.push(Entry::Summary(text));
            continue;
        }
        for b in &m.content {
            match (m.role, b) {
                (_, ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }) => {}
                (_, ContentBlock::Text { text }) if text.trim().is_empty() => {}
                (Role::User, ContentBlock::Text { text }) => out.push(Entry::User(text.clone())),
                (Role::Assistant, ContentBlock::Text { text }) => {
                    out.push(Entry::Assistant {
                        text: text.clone(),
                        streaming: false,
                    });
                }
                (_, ContentBlock::ToolUse { id, name, input }) => {
                    let call = ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    };
                    out.push(Entry::Tool {
                        id: id.clone(),
                        summary: tern_core::agent::summarize(&call),
                        state: ToolState::Interrupted,
                        output: String::new(),
                    });
                }
                (
                    _,
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    },
                ) => {
                    let found = out
                        .iter_mut()
                        .rev()
                        .find(|e| matches!(e, Entry::Tool { id, .. } if id == tool_use_id));
                    if let Some(Entry::Tool { state, output, .. }) = found {
                        *state = match (*is_error, content.starts_with("denied")) {
                            (true, true) => ToolState::Denied,
                            (true, false) => ToolState::Failed,
                            (false, _) => ToolState::Ok,
                        };
                        *output = content.clone();
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pairs_tool_calls_with_results() {
        let msgs = vec![
            HistoryMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
                summary: false,
            },
            HistoryMessage {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "checking".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "shell".into(),
                        input: json!({"command": "ls"}),
                    },
                    ContentBlock::ToolUse {
                        id: "t2".into(),
                        name: "shell".into(),
                        input: json!({"command": "rm x"}),
                    },
                ],
                summary: false,
            },
            HistoryMessage {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "t1".into(),
                        content: "a\nb".into(),
                        is_error: false,
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "t2".into(),
                        content: "denied: no".into(),
                        is_error: true,
                    },
                ],
                summary: false,
            },
        ];
        let e = entries(&msgs);
        assert_eq!(e.len(), 4);
        assert_eq!(e[0], Entry::User("hi".into()));
        assert!(
            matches!(&e[2], Entry::Tool { summary, state: ToolState::Ok, output, .. } if summary == "shell ls" && output == "a\nb")
        );
        assert!(matches!(
            &e[3],
            Entry::Tool {
                state: ToolState::Denied,
                ..
            }
        ));
    }
}
