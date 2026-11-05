//! Scoped approval grants end to end: testing a draft connector that
//! reaches the network asks for approval, and one "this run" or "always"
//! answer covers the repeat tests of that same draft tool instead of
//! asking again for each (and asks again for anything different).
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use serde_json::{Value, json};
use subagents_env::{Env, Events, get, post, start};

const DRAFT: &str = r#"return { name = "net", description = "test", capabilities = { http = { "example.invalid" } }, tools = {
  a = { description = "first", run = function(ctx, args) return "a ok" end },
  b = { description = "second", run = function(ctx, args) return "b ok" end } } }"#;

fn test(tool: &str, n: u32) -> Value {
    json!({ "name": "connector_test", "input": { "name": "net", "tool": tool, "args": { "n": n } } })
}

fn script() -> Value {
    json!({ "turns": [
        { "tool_calls": [{ "name": "connector_write", "input": { "name": "net", "source": DRAFT } }] },
        { "tool_calls": [test("a", 1), test("a", 2), test("a", 3), test("b", 4)] },
        { "text": "All tested." }
    ] })
}

fn approvals(evs: &[Value]) -> Vec<Value> {
    evs.iter()
        .filter_map(|e| e.get("approval_requested").cloned())
        .collect()
}

async fn next_approval(events: &mut Events, already: usize) -> Value {
    let evs = events
        .until(30, |evs| approvals(evs).len() > already)
        .await;
    approvals(&evs)[already].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_grant_stops_the_repeat_prompts_for_the_same_draft_tool_only() {
    let env = Env::new(&script());
    let _daemon = env.daemon_with(None);
    let sock = env.socket();
    let mut events = Events::open(&sock).await;
    let started = start(&sock, "test the draft").await;
    let run = started["run_id"].as_str().unwrap().to_owned();

    // The first test of `a` asks, offering scoped grants for exactly it.
    let first = next_approval(&mut events, 0).await;
    assert_eq!(first["tool"], "connector_test");
    assert_eq!(first["grant"]["label"], "draft net.a", "{first}");
    assert!(
        first["grant"]["key"]
            .as_str()
            .unwrap()
            .starts_with("draft net.a#"),
        "{first}"
    );
    let (status, _) = post(
        &sock,
        &format!("/v1/approvals/{}", first["id"].as_str().unwrap()),
        json!({ "decision": "run" }),
    )
    .await;
    assert_eq!(status, 200);

    // `a` twice more run without asking; `b` is a different tool: it asks.
    let second = next_approval(&mut events, 1).await;
    assert_eq!(second["input"]["tool"], "b", "{second}");
    assert_eq!(second["grant"]["label"], "draft net.b");
    assert_ne!(second["grant"]["key"], first["grant"]["key"]);
    post(
        &sock,
        &format!("/v1/approvals/{}", second["id"].as_str().unwrap()),
        json!({ "decision": "once" }),
    )
    .await;
    let evs = events
        .until(30, |evs| {
            evs.iter()
                .any(|e| e["type"] == "run_finished" && e["run_id"] == run.as_str())
        })
        .await;
    assert_eq!(approvals(&evs).len(), 2, "only a and b asked");
    // Nothing standing was stored by a run grant.
    let grants: i64 = env.query("SELECT COUNT(*) FROM approval_allowlist");
    assert_eq!(grants, 0);
    let (_, pending) = get(&sock, "/v1/approvals").await;
    assert_eq!(pending, json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_decision_is_refused_and_session_is_accepted() {
    let env = Env::new(&script());
    let _daemon = env.daemon_with(None);
    let sock = env.socket();
    let mut events = Events::open(&sock).await;
    start(&sock, "test the draft").await;
    let first = next_approval(&mut events, 0).await;
    let url = format!("/v1/approvals/{}", first["id"].as_str().unwrap());
    let (status, body) = post(&sock, &url, json!({ "decision": "forever" })).await;
    assert_eq!(status, 400);
    assert!(body.to_string().contains("session"), "{body}");
    let (status, _) = post(&sock, &url, json!({ "decision": "session" })).await;
    assert_eq!(status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn always_stores_one_key_grant() {
    let env = Env::new(&script());
    let _daemon = env.daemon_with(None);
    let sock = env.socket();
    let mut events = Events::open(&sock).await;
    start(&sock, "test the draft").await;
    let first = next_approval(&mut events, 0).await;
    post(
        &sock,
        &format!("/v1/approvals/{}", first["id"].as_str().unwrap()),
        json!({ "decision": "always" }),
    )
    .await;
    let second = next_approval(&mut events, 1).await;
    assert_eq!(second["input"]["tool"], "b");
    let pattern: String =
        env.query("SELECT pattern FROM approval_allowlist WHERE tool = 'connector_test'");
    assert_eq!(
        pattern,
        format!("key:{}", first["grant"]["key"].as_str().unwrap())
    );
}
