//! T3a taint end-to-end (docs/personal-agent-tooling-spec.md §5.3): a
//! routine that reads untrusted content is tainted, the run views say so,
//! and its post is marked tainted for whoever reads the channel next.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use serde_json::{Value, json};
use subagents_env::{Env, get, post};

async fn run_routine(s: &std::path::Path, prompt: &str) -> Value {
    let (code, sch) = post(
        s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": prompt }),
    )
    .await;
    assert_eq!(code, 201, "{sch}");
    let (_, r) = post(
        s,
        &format!("/v1/schedules/{}/run", sch["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let run = r["run_id"].as_str().unwrap().to_owned();
    get(s, &format!("/v1/runs/{run}/wait")).await;
    let (_, session) = get(s, &format!("/v1/runs/{run}/session")).await;
    let session = session["session_id"].as_str().unwrap().to_owned();
    let (_, runs) = get(s, &format!("/v1/sessions/{session}/runs")).await;
    runs.as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == run.as_str())
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_routine_that_reads_untrusted_content_writes_a_tainted_post() {
    // `mcp_list` is a built-in read tool marked untrusted (server stderr).
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Sweep the inbox", "turns": [
                { "text": "Looking.", "tool_calls": [{ "name": "mcp_list", "input": {} }] },
                { "text": "Pay invoice 12 now." }
            ]},
            { "matches": "Morning brief", "turns": [{ "text": "Two meetings today." }] }
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();

    let tainted = run_routine(&s, "Sweep the inbox").await;
    assert_eq!(tainted["status"], "completed", "{tainted}");
    assert_eq!(tainted["taint"], "result of mcp_list");
    let clean = run_routine(&s, "Morning brief").await;
    assert_eq!(clean["status"], "completed");
    assert_eq!(clean.get("taint"), None);

    let (_, posts) = get(&s, "/v1/channels/home/posts").await;
    let by_text = |t: &str| {
        posts
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["text"] == t)
            .unwrap_or_else(|| panic!("{t}: {posts:#}"))
            .clone()
    };
    assert_eq!(by_text("Pay invoice 12 now.")["tainted"], true);
    assert_eq!(
        by_text("Two meetings today.").get("tainted"),
        None,
        "omitted when clean"
    );

    // The journal shows the untrusted result.
    let (_, steps) = get(
        &s,
        &format!("/v1/runs/{}/steps", tainted["id"].as_str().unwrap()),
    )
    .await;
    let tool = steps
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "tool")
        .unwrap();
    assert_eq!(tool["output"]["untrusted"], true, "{tool:#}");
}
