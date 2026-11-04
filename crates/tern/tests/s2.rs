//! S2 end-to-end (docs/subagents-design.md §3, §7): the API's view of
//! sub-agents: child lists, event enrichment, labelled approvals, session
//! filtering, run trees and usage, and `tern run` progress.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use serde_json::{Value, json};
use subagents_env::{Env, Events, get, post, start};

const FINAL: &str = "Both reported.";

/// The parent spawns `a` (all tools; runs a shell command, which needs
/// approval) and `b` (read-only), then waits for both.
fn script() -> Value {
    json!({
        "turns": [
            { "text": "Delegating.", "tool_calls": [
                { "name": "agent_spawn", "input": { "label": "a", "task": "SUBTASK-a: run ls" } },
                { "name": "agent_spawn", "input": { "label": "b", "task": "SUBTASK-b: list", "tools": "read_only" } },
                { "name": "agent_wait", "input": {} }
            ] },
            { "text": FINAL }
        ],
        "conversations": [
            { "when_task_contains": "SUBTASK-a", "turns": [
                { "tool_calls": [{ "name": "shell", "input": { "command": "ls" } }] },
                { "text": "report from a" }
            ] },
            { "when_task_contains": "SUBTASK-b", "turns": [
                { "tool_calls": [{ "name": "list_dir", "input": { "path": "." } }] },
                { "text": "report from b" }
            ] }
        ]
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn the_api_shows_sub_agents_their_events_and_their_approvals() {
    let env = Env::new(&script());
    let _daemon = env.daemon_with(None);
    let sock = env.socket();

    let (_, health) = get(&sock, "/v1/health").await;
    assert!(
        health["features"]
            .as_array()
            .unwrap()
            .contains(&json!("subagents")),
        "{health}"
    );

    let mut events = Events::open(&sock).await;
    let started = start(&sock, "delegate two things").await;
    let parent = started["run_id"].as_str().unwrap().to_owned();
    let session = started["session_id"].as_str().unwrap().to_owned();

    // a's shell call waits for an API client, labelled with who asks.
    let approval = {
        let evs = events
            .until(20, |evs| {
                evs.iter().any(|e| e.get("approval_requested").is_some())
            })
            .await;
        evs.into_iter()
            .find_map(|e| e.get("approval_requested").cloned())
            .unwrap()
    };
    assert_eq!(approval["parent_run_id"], parent.as_str(), "{approval}");
    assert_eq!(approval["agent_label"], "a");
    assert_eq!(approval["root_session_id"], session.as_str());
    assert_ne!(
        approval["session_id"],
        session.as_str(),
        "the child's own session"
    );
    let (_, pending) = get(&sock, "/v1/approvals").await;
    assert_eq!(pending[0]["agent_label"], "a", "{pending}");
    assert_eq!(pending[0]["root_session_id"], session.as_str());

    // While a waits it is an active run, with its parent and label.
    let (_, active) = get(&sock, "/v1/runs/active").await;
    let a_active = active
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == "a")
        .unwrap_or_else(|| panic!("{active}"));
    assert_eq!(a_active["parent_run_id"], parent.as_str());
    let top = active
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run_id"] == parent.as_str())
        .unwrap();
    assert!(
        top.get("parent_run_id").is_none() && top.get("label").is_none(),
        "{top}"
    );

    let (_, kids) = get(&sock, &format!("/v1/runs/{parent}/children")).await;
    let a = kids
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["label"] == "a")
        .unwrap()
        .clone();
    assert_eq!(a["status"], "running", "{a}");
    assert_eq!(a["task"], "SUBTASK-a: run ls");

    let (status, _) = post(
        &sock,
        &format!("/v1/approvals/{}", approval["id"].as_str().unwrap()),
        json!({ "decision": "deny" }),
    )
    .await;
    assert_eq!(status, 200);

    let evs = events
        .until(30, |evs| {
            evs.iter()
                .any(|e| e["type"] == "run_finished" && e["run_id"] == parent.as_str())
        })
        .await;

    // Every event of a child carries its parent and the root session.
    let child_runs: Vec<String> = evs
        .iter()
        .filter(|e| e["type"] == "subagent_spawned")
        .map(|e| e["child_run_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(child_runs.len(), 2);
    let mut seen = 0;
    for e in evs.iter().filter(|e| {
        e["run_id"]
            .as_str()
            .is_some_and(|r| child_runs.iter().any(|c| c == r))
    }) {
        seen += 1;
        assert_eq!(e["parent_run_id"], parent.as_str(), "{e}");
        assert_eq!(e["root_session_id"], session.as_str(), "{e}");
        assert!(
            e["session_id"].is_string() && e["session_id"] != session.as_str(),
            "{e}"
        );
    }
    assert!(seen >= 8, "{evs:#?}");
    // The parent's events don't.
    for e in evs.iter().filter(|e| e["run_id"] == parent.as_str()) {
        assert!(e.get("root_session_id").is_none(), "{e}");
        assert_eq!(e["session_id"], session.as_str());
    }

    // Sessions: sub-agent sessions are hidden unless asked for.
    let (_, sessions) = get(&sock, "/v1/sessions").await;
    assert_eq!(sessions.as_array().unwrap().len(), 1, "{sessions}");
    let (_, subs) = get(&sock, "/v1/sessions?source=subagent").await;
    assert_eq!(subs.as_array().unwrap().len(), 2, "{subs}");
    assert!(
        subs.as_array()
            .unwrap()
            .iter()
            .all(|s| s["source"] == "subagent")
    );

    // Children, as the desktop resyncs them.
    let (_, kids) = get(&sock, &format!("/v1/sessions/{session}/subagents")).await;
    let kids = kids.as_array().unwrap().clone();
    assert_eq!(kids.len(), 2);
    let keys = [
        "run_id",
        "session_id",
        "parent_run_id",
        "call_id",
        "label",
        "task",
        "model",
        "depth",
        "status",
        "error",
        "started_at",
        "finished_at",
        "usage",
        "result_preview",
        "pending_question",
    ];
    for k in &kids {
        for key in keys {
            assert!(k.get(key).is_some(), "{key} missing: {k}");
        }
        assert_eq!(k["status"], "completed", "{k}");
        assert_eq!(k["parent_run_id"], parent.as_str());
        assert_eq!(k["depth"], 1);
        assert_eq!(k["model"], "mock:mock");
        assert!(k["call_id"].as_str().unwrap().starts_with("mock_"));
        // Unix seconds, not milliseconds.
        let t = k["started_at"].as_i64().unwrap();
        assert!(t > 1_600_000_000 && t < 10_000_000_000, "{t}");
        assert!(k["finished_at"].as_i64().unwrap() >= t);
        assert!(k["usage"]["input_tokens"].as_u64().unwrap() > 0);
        for u in ["output_tokens", "cache_read_tokens", "cache_write_tokens"] {
            assert!(k["usage"][u].is_u64(), "{k}");
        }
    }
    let by = |l: &str| kids.iter().find(|k| k["label"] == l).unwrap();
    assert_eq!(by("a")["result_preview"], "report from a");
    assert_eq!(by("b")["result_preview"], "report from b");
    assert_eq!(by("b")["task"], "SUBTASK-b: list");
    let (_, again) = get(&sock, &format!("/v1/runs/{parent}/children?recursive=1")).await;
    assert_eq!(again.as_array().unwrap().len(), 2);

    // The run tree for the inspector.
    let (_, runs) = get(&sock, &format!("/v1/sessions/{session}/runs")).await;
    assert_eq!(runs.as_array().unwrap().len(), 1, "{runs}");
    assert_eq!(runs[0]["depth"], 0);
    assert!(runs[0].get("parent_run_id").is_none());
    let (_, runs) = get(
        &sock,
        &format!("/v1/sessions/{session}/runs?include=subagents"),
    )
    .await;
    let runs = runs.as_array().unwrap();
    assert_eq!(runs.len(), 3, "{runs:?}");
    assert_eq!(runs[0]["id"], parent.as_str());
    for r in &runs[1..] {
        assert_eq!(r["parent_run_id"], parent.as_str());
        assert_eq!(r["depth"], 1);
        assert!(r["label"] == "a" || r["label"] == "b");
        assert_eq!(r["status"], "completed");
    }

    // Usage of the whole tree is more than the parent's own.
    let (_, own) = get(&sock, &format!("/v1/sessions/{session}/usage")).await;
    let (_, tree) = get(
        &sock,
        &format!("/v1/sessions/{session}/usage?include=subagents"),
    )
    .await;
    let n = |v: &Value| v["input_tokens"].as_u64().unwrap();
    assert!(n(&tree) > n(&own), "{own} {tree}");
    // The context meter: the parent's last call against its window.
    let ctx = own["context_tokens"].as_u64().unwrap();
    let window = own["context_window"].as_u64().unwrap();
    assert!(ctx > 0 && ctx <= n(&own), "{own}");
    assert!(own["compact_at"].as_u64().unwrap() < window, "{own}");

    // Only running sub-agents take messages.
    let (status, body) = post(
        &sock,
        &format!("/v1/runs/{parent}/message"),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body["error"].as_str().unwrap().contains("not a sub-agent"));
    let (status, _) = post(
        &sock,
        &format!("/v1/runs/{}/message", by("a")["run_id"].as_str().unwrap()),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(status, 400);
    let (status, _) = post(&sock, "/v1/runs/nope/message", json!({ "text": "hi" })).await;
    assert_eq!(status, 404);
}

/// `tern run` prints each child's progress on stderr with its label.
#[test]
fn tern_run_prints_child_progress() {
    let env = Env::new(&script());
    let o = env
        .cmd(&["run", "--yes", "delegate two things"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), FINAL);
    for line in [
        "[a] started",
        "[b] started",
        "[b] ✓ list_dir .",
        "[a] completed",
        "[b] completed",
    ] {
        assert!(stderr.contains(line), "{line} missing:\n{stderr}");
    }
    assert!(
        stderr.contains("[a] ✓ shell ls") || stderr.contains("[a] ✗ shell ls"),
        "{stderr}"
    );
}
