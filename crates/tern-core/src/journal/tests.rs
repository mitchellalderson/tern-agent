use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::json;
use tern_store::{NewMessage, NewRun, Store};

use super::*;
use crate::types::Message;

const RUN: &str = "r1";

fn setup() -> (Store, Journal) {
    let store = Store::open_in_memory().unwrap();
    store.create_session("s1", "cli", None).unwrap();
    store
        .create_run(
            &NewRun {
                id: RUN,
                session_id: "s1",
                trigger: "user",
                model: "mock:m",
                system_prompt: "sys",
            },
            &NewMessage {
                role: "user".into(),
                content_json: "[]".into(),
                text: "hi".into(),
                summary_of: None,
            },
        )
        .unwrap();
    let journal = Journal::new(store.clone());
    (store, journal)
}

/// A step body that counts executions and returns `value`.
fn counted(
    calls: &Arc<AtomicU32>,
    value: &str,
) -> impl FnOnce() -> std::future::Ready<Result<String, StepError>> {
    let calls = calls.clone();
    let value = value.to_owned();
    move || {
        calls.fetch_add(1, Ordering::SeqCst);
        std::future::ready(Ok(value))
    }
}

fn status(store: &Store, idx: u32) -> String {
    store.step(RUN, idx).unwrap().unwrap().status
}

#[tokio::test]
async fn not_found_executes_and_records() {
    let (store, j) = setup();
    let calls = Arc::new(AtomicU32::new(0));
    let out: String = j
        .step(
            RUN,
            0,
            StepKind::Tool,
            &json!({"a": 1}),
            counted(&calls, "done"),
        )
        .await
        .unwrap();
    assert_eq!(out, "done");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let row = store.step(RUN, 0).unwrap().unwrap();
    assert_eq!(row.status, "completed");
    assert_eq!(row.kind, "tool");
    assert_eq!(row.output_json.as_deref(), Some("\"done\""));
}

#[tokio::test]
async fn completed_with_same_input_is_memoized() {
    let (_store, j) = setup();
    let calls = Arc::new(AtomicU32::new(0));
    let input = json!({"cmd": "ls"});
    let first: String = j
        .step(RUN, 0, StepKind::Tool, &input, counted(&calls, "first"))
        .await
        .unwrap();
    let second = j
        .step_with(RUN, 0, StepKind::Tool, &input, InFlight::Retry, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(StepOutput::new("second".to_owned()))
        })
        .await
        .unwrap();
    assert_eq!(first, "first");
    assert_eq!(second.value, "first");
    assert!(second.replayed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn hash_is_independent_of_key_order() {
    let (_store, j) = setup();
    let calls = Arc::new(AtomicU32::new(0));
    let a: serde_json::Value = serde_json::from_str(r#"{"x":1,"y":2}"#).unwrap();
    let b: serde_json::Value = serde_json::from_str(r#"{"y":2,"x":1}"#).unwrap();
    let _: String = j
        .step(RUN, 0, StepKind::Tool, &a, counted(&calls, "v"))
        .await
        .unwrap();
    let _: String = j
        .step(RUN, 0, StepKind::Tool, &b, counted(&calls, "v"))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn kind_is_part_of_the_hash() {
    let (_store, j) = setup();
    let calls = Arc::new(AtomicU32::new(0));
    let input = json!("same");
    let _: String = j
        .step(RUN, 0, StepKind::Tool, &input, counted(&calls, "a"))
        .await
        .unwrap();
    let _: String = j
        .step(RUN, 0, StepKind::Llm, &input, counted(&calls, "b"))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn divergence_truncates_this_and_later_steps_and_their_messages() {
    let (store, j) = setup();
    let calls = Arc::new(AtomicU32::new(0));
    for idx in 0..3 {
        j.step_with(
            RUN,
            idx,
            StepKind::Tool,
            &json!(idx),
            InFlight::Retry,
            || async move {
                Ok(StepOutput::new(idx).with_messages(vec![Message::user_text(format!("m{idx}"))]))
            },
        )
        .await
        .unwrap();
    }
    let run = store.run(RUN).unwrap().unwrap();
    assert_eq!(store.run_context(&run, 99).unwrap().len(), 4);

    let out: String = j
        .step(
            RUN,
            1,
            StepKind::Tool,
            &json!("changed"),
            counted(&calls, "live"),
        )
        .await
        .unwrap();
    assert_eq!(out, "live");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(status(&store, 1), "completed");
    assert!(
        store.step(RUN, 2).unwrap().is_none(),
        "later steps are deleted"
    );
    let texts: Vec<_> = store
        .run_context(&run, 99)
        .unwrap()
        .into_iter()
        .map(|m| m.text)
        .collect();
    assert_eq!(
        texts,
        ["hi", "m0"],
        "messages from diverged steps are deleted"
    );
}

#[tokio::test]
async fn started_retry_policy_reexecutes() {
    // Crashed in flight: llm, compaction, approval and idempotent tools run again.
    for kind in [
        StepKind::Llm,
        StepKind::Compaction,
        StepKind::Approval,
        StepKind::Tool,
    ] {
        let (store, j) = setup();
        let input = json!({"k": kind.as_str()});
        store
            .start_step(
                RUN,
                0,
                kind.as_str(),
                &input_hash(kind, &input).unwrap(),
                "{}",
            )
            .unwrap();
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let out = j
            .step_with(RUN, 0, kind, &input, InFlight::Retry, || async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(StepOutput::new("again".to_owned()))
            })
            .await
            .unwrap();
        assert_eq!(out.value, "again");
        assert!(!out.replayed);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{kind:?}");
        assert_eq!(status(&store, 0), "completed");
    }
}

#[tokio::test]
async fn started_substitute_policy_does_not_reexecute() {
    let (store, j) = setup();
    let input = json!({"cmd": "rm -rf build"});
    store
        .start_step(
            RUN,
            0,
            "tool",
            &input_hash(StepKind::Tool, &input).unwrap(),
            "{}",
        )
        .unwrap();
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let out = j
        .step_with(
            RUN,
            0,
            StepKind::Tool,
            &input,
            InFlight::Substitute(StepOutput::new("interrupted".to_owned())),
            || async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(StepOutput::new("ran".to_owned()))
            },
        )
        .await
        .unwrap();
    assert_eq!(out.value, "interrupted");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(status(&store, 0), "completed");

    // The substituted result is now memoized like any other.
    let again: String = j
        .step(RUN, 0, StepKind::Tool, &input, counted(&calls, "ran"))
        .await
        .unwrap();
    assert_eq!(again, "interrupted");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn substitute_messages_are_recorded() {
    let (store, j) = setup();
    let input = json!(1);
    store
        .start_step(
            RUN,
            0,
            "tool",
            &input_hash(StepKind::Tool, &input).unwrap(),
            "{}",
        )
        .unwrap();
    let sub = StepOutput::new(0).with_messages(vec![Message::user_text("interrupted")]);
    j.step_with(
        RUN,
        0,
        StepKind::Tool,
        &input,
        InFlight::Substitute(sub),
        || async { Ok(StepOutput::new(1)) },
    )
    .await
    .unwrap();
    let run = store.run(RUN).unwrap().unwrap();
    assert_eq!(store.run_context(&run, 1).unwrap().len(), 2);
}

#[tokio::test]
async fn failure_is_recorded_then_retried_on_next_call() {
    let (store, j) = setup();
    let input = json!("x");
    let err = j
        .step::<_, String, _, _>(RUN, 0, StepKind::Llm, &input, || async {
            Err(StepError::Failed("HTTP 500".into()))
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("HTTP 500"));
    let row = store.step(RUN, 0).unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert_eq!(row.error.as_deref(), Some("HTTP 500"));

    let calls = Arc::new(AtomicU32::new(0));
    let out: String = j
        .step(RUN, 0, StepKind::Llm, &input, counted(&calls, "ok"))
        .await
        .unwrap();
    assert_eq!(out, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(status(&store, 0), "completed");
}

#[tokio::test]
async fn messages_are_appended_only_on_live_execution() {
    let (store, j) = setup();
    for _ in 0..2 {
        j.step_with(
            RUN,
            0,
            StepKind::Llm,
            &json!("req"),
            InFlight::Retry,
            || async { Ok(StepOutput::new(()).with_messages(vec![Message::user_text("reply")])) },
        )
        .await
        .unwrap();
    }
    let run = store.run(RUN).unwrap().unwrap();
    assert_eq!(store.run_context(&run, 1).unwrap().len(), 2);
}

#[tokio::test]
async fn usage_is_stored() {
    let (store, j) = setup();
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 5,
        ..Usage::default()
    };
    j.step_with(
        RUN,
        0,
        StepKind::Llm,
        &json!(0),
        InFlight::Retry,
        || async move { Ok(StepOutput::new(()).with_usage(usage)) },
    )
    .await
    .unwrap();
    let row = store.step(RUN, 0).unwrap().unwrap();
    let stored: Usage = serde_json::from_str(row.usage_json.as_deref().unwrap()).unwrap();
    assert_eq!(stored, usage);
}

#[tokio::test]
async fn secrets_are_redacted_from_stored_inputs_outputs_and_messages() {
    let (store, j) = setup();
    let j = j.with_redactions(["sk-supersecret".to_owned()]);
    let input = json!({"headers": {"auth": "Bearer sk-supersecret"}});
    j.step_with(RUN, 0, StepKind::Tool, &input, InFlight::Retry, || async {
        Ok(StepOutput::new("token=sk-supersecret".to_owned())
            .with_messages(vec![Message::user_text("got sk-supersecret")]))
    })
    .await
    .unwrap();
    let row = store.step(RUN, 0).unwrap().unwrap();
    assert!(
        !row.input_json.contains("sk-supersecret"),
        "{}",
        row.input_json
    );
    assert!(row.input_json.contains("[REDACTED]"));
    assert!(!row.output_json.unwrap().contains("sk-supersecret"));
    let run = store.run(RUN).unwrap().unwrap();
    for m in store.run_context(&run, 1).unwrap() {
        assert!(!m.text.contains("sk-supersecret"));
        assert!(!m.content_json.contains("sk-supersecret"));
    }

    // Replay still matches: the hash is over the unredacted input.
    let calls = Arc::new(AtomicU32::new(0));
    let _: String = j
        .step(RUN, 0, StepKind::Tool, &input, counted(&calls, "x"))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
