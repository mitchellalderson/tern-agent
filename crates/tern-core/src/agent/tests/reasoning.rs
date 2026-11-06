//! Reasoning: streamed live, kept with the step, never in channel text.

use super::*;

fn reasoning_events(events: &mut broadcast::Receiver<AgentEvent>) -> String {
    let mut out = String::new();
    while let Ok(ev) = events.try_recv() {
        if let AgentEvent::ReasoningDelta { text, .. } = ev {
            out.push_str(&text);
        }
    }
    out
}

#[tokio::test]
async fn reasoning_streams_and_is_kept_with_the_step() {
    let h = Harness::new();
    let agent = h.agent_showing_reasoning(
        Arc::new(Script::new(vec![Reply::Reasoned(
            "weigh the options",
            "Done.",
        )])),
        true,
    );
    let mut events = agent.subscribe();
    let run = h.start(&agent, "think");
    let out = agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(out.status, RunStatus::Completed);
    // Channels deliver the final text only: no reasoning in it.
    assert_eq!(out.final_text, "Done.");
    assert_eq!(reasoning_events(&mut events), "weigh the options");

    // Persisted with the message, so it survives a reload.
    let rows = h.store.session_messages("s1").unwrap();
    let blocks: Vec<ContentBlock> =
        serde_json::from_str(&rows.last().unwrap().content_json).unwrap();
    match &blocks[0] {
        ContentBlock::Thinking {
            thinking,
            signature,
            duration_ms,
        } => {
            assert_eq!(thinking, "weigh the options");
            assert_eq!(signature.as_deref(), Some("sig"));
            assert!(duration_ms.is_some());
        }
        other => panic!("expected a thinking block first, got {other:?}"),
    }
    assert!(matches!(&blocks[1], ContentBlock::Text { text } if text == "Done."));
}

#[tokio::test]
async fn show_reasoning_off_streams_nothing_but_still_keeps_it() {
    let h = Harness::new();
    let agent = h.agent_showing_reasoning(
        Arc::new(Script::new(vec![Reply::Reasoned("quiet thoughts", "ok")])),
        false,
    );
    let mut events = agent.subscribe();
    let run = h.start(&agent, "think");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(reasoning_events(&mut events), "");
    // Kept for providers that need signed blocks echoed back.
    let rows = h.store.session_messages("s1").unwrap();
    assert!(rows.last().unwrap().content_json.contains("quiet thoughts"));
}

#[test]
fn assembler_orders_thinking_before_text_and_tool_calls() {
    let mut a = Assembler::default();
    for ev in [
        StreamEvent::ReasoningDelta("hm".into()),
        StreamEvent::ReasoningSignature("s".into()),
        StreamEvent::TextDelta("let me look".into()),
        StreamEvent::ToolUseStart {
            id: "t1".into(),
            name: "x".into(),
        },
        StreamEvent::MessageStop {
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
        },
    ] {
        a.push(ev);
    }
    let turn = a.finish().unwrap();
    let kinds: Vec<&str> = turn
        .message
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Thinking { .. } => "thinking",
            ContentBlock::Text { .. } => "text",
            ContentBlock::ToolUse { .. } => "tool_use",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["thinking", "text", "tool_use"]);
}

#[test]
fn redacted_reasoning_is_kept_opaque() {
    let mut a = Assembler::default();
    a.push(StreamEvent::ReasoningRedacted("enc".into()));
    a.push(StreamEvent::TextDelta("hi".into()));
    a.push(StreamEvent::MessageStop {
        usage: Usage::default(),
        stop_reason: StopReason::EndTurn,
    });
    let turn = a.finish().unwrap();
    assert!(matches!(
        &turn.message.content[0],
        ContentBlock::RedactedThinking { data } if data == "enc"
    ));
}
