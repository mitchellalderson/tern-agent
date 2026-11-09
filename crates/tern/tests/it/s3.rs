//! S3 end-to-end (docs/subagents-design.md §2.2–§2.4, §7): two-way
//! messaging between a parent and its sub-agents, and from the user to a
//! sub-agent.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use serde_json::{Value, json};
use subagents_env::{Env, Events, post, start};

const FINAL: &str = "Scan done: public companies only.";
const QUESTION: &str = "Include private companies?";
const ANSWER: &str = "No, public only.";

/// The parent spawns `scan` and waits; `scan` asks a question; the parent
/// answers and waits again. With `parent_lists`, the parent lists the
/// directory twice before spawning, so its turn after the first wait (step
/// 5) comes after the child's question (child step 1) and no other step has
/// that index, for TERN_CRASH_AFTER_STEP.
fn ask_script(parent_lists: bool) -> Value {
    let list = json!({ "name": "list_dir", "input": { "path": "." } });
    let mut first = vec![];
    if parent_lists {
        first = vec![list.clone(), list.clone()];
    }
    first.push(json!({ "name": "agent_spawn", "input": { "label": "scan", "task": "SUBTASK-scan: find companies" } }));
    first.push(json!({ "name": "agent_wait", "input": {} }));
    let ask = json!({ "tool_calls": [{ "name": "agent_ask_parent", "input": {
        "question": QUESTION, "recommendation": "no" } }] });
    json!({
        "turns": [
            { "text": "Delegating.", "tool_calls": first },
            { "tool_calls": [
                { "name": "agent_send", "input": { "agent": "scan", "message": ANSWER } },
                { "name": "agent_wait", "input": {} }
            ] },
            { "text": FINAL }
        ],
        "conversations": [
            { "when_task_contains": "SUBTASK-scan", "turns": [
                ask,
                { "text": "report: 3 public companies" }
            ] }
        ]
    })
}

fn assert_asked_once_and_answered(env: &Env) {
    let questions: i64 = env.query("SELECT count(*) FROM agent_mail WHERE kind = 'question'");
    assert_eq!(questions, 1, "asked exactly once");
    let answers: i64 = env.query("SELECT count(*) FROM agent_mail WHERE kind = 'answer'");
    assert_eq!(answers, 1);
    let (last, parent) = env.transcript("r.parent_run_id IS NULL");
    assert_eq!(last, FINAL);
    // The parent's first wait returned the question, the second the report.
    assert!(parent.contains("\"status\":\"question\""), "{parent}");
    assert!(parent.contains(QUESTION), "{parent}");
    assert!(parent.contains("report: 3 public companies"), "{parent}");
    // The child got the answer as agent_ask_parent's result.
    let (child_last, child) = env.transcript("r.parent_run_id IS NOT NULL");
    assert_eq!(child_last, "report: 3 public companies");
    assert!(
        child.contains(&format!("\"answer\":\"{ANSWER}\"")),
        "{child}"
    );
    assert_eq!(env.children()[0].1, "completed");
}

#[test]
fn a_child_asks_and_the_parent_answers() {
    let env = Env::new(&ask_script(false));
    let o = env
        .cmd(&["run", "--yes", "scan companies"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), FINAL);
    assert_asked_once_and_answered(&env);
    assert!(
        stderr.contains(&format!("[scan] asks: {QUESTION}")),
        "{stderr}"
    );
    assert!(stderr.contains(&format!("[scan] ← {ANSWER}")), "{stderr}");
    // The question was delivered by agent_wait, so it never reached the
    // parent's inbox as well.
    let inbox: i64 = env.query(
        "SELECT count(*) FROM steps s JOIN runs r ON r.id = s.run_id
         WHERE r.parent_run_id IS NULL AND s.kind = 'inbox'",
    );
    assert_eq!(inbox, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn questions_and_answers_are_events() {
    let env = Env::new(&ask_script(false));
    let _daemon = env.daemon_with(None);
    let sock = env.socket();
    let mut events = Events::open(&sock).await;
    let started = start(&sock, "scan companies").await;
    let parent = started["run_id"].as_str().unwrap().to_owned();
    let evs = events
        .until(30, |evs| {
            evs.iter()
                .any(|e| e["type"] == "run_finished" && e["run_id"] == parent.as_str())
        })
        .await;
    let child = evs
        .iter()
        .find(|e| e["type"] == "subagent_spawned")
        .unwrap()["child_run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let msgs: Vec<&Value> = evs
        .iter()
        .filter(|e| e["type"] == "subagent_message")
        .collect();
    assert_eq!(msgs.len(), 2, "{msgs:#?}");
    assert_eq!(msgs[0]["kind"], "question");
    assert_eq!(msgs[0]["run_id"], child.as_str());
    assert_eq!(msgs[0]["to_run_id"], parent.as_str());
    assert_eq!(msgs[0]["text"], QUESTION);
    // The sender is a sub-agent, so the event is enriched like its others.
    assert_eq!(msgs[0]["parent_run_id"], parent.as_str());
    assert_eq!(msgs[1]["kind"], "answer");
    assert_eq!(msgs[1]["run_id"], parent.as_str());
    assert_eq!(msgs[1]["to_run_id"], child.as_str());
    assert_eq!(msgs[1]["text"], ANSWER);
    assert_asked_once_and_answered(&env);
}

/// The daemon dies after the child asked (its ask step started, the
/// question posted) and before the parent answered. After a restart the
/// child's ask step re-executes without asking again, and the run ends as
/// an uncrashed one does.
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_between_ask_and_answer_does_not_ask_twice() {
    let env = Env::new(&ask_script(true));
    // Parent: 0 llm, 1-2 list_dir, 3 agent_spawn, 4 agent_wait, 5 llm.
    // Child: 0 llm, 1 agent_ask_parent (blocks until answered).
    let mut daemon = env.daemon_with(Some(5));
    start(&env.socket(), "scan companies").await;
    env.wait_abort(&mut daemon);
    drop(daemon);
    let questions: i64 = env.query("SELECT count(*) FROM agent_mail WHERE kind = 'question'");
    assert_eq!(questions, 1);
    let asking: String = env.query(
        "SELECT s.status FROM steps s JOIN runs r ON r.id = s.run_id
         WHERE r.parent_run_id IS NOT NULL AND s.idx = 1",
    );
    assert_eq!(asking, "started");
    let answers: i64 = env.query("SELECT count(*) FROM agent_mail WHERE kind = 'answer'");
    assert_eq!(answers, 0);

    let _daemon = env.daemon_with(None);
    env.wait_for("the parent to complete", 30, |e| {
        e.parent_status() == "completed"
    });
    assert_asked_once_and_answered(&env);
}

/// Unanswered, the question times out and the child carries on with its
/// recommendation; the parent ended its turn, so the report reaches it
/// through the inbox.
#[test]
fn an_unanswered_question_times_out() {
    let script = json!({
        "turns": [
            { "tool_calls": [
                { "name": "agent_spawn", "input": { "label": "scan", "task": "SUBTASK-scan" } },
                { "name": "agent_wait", "input": {} }
            ] },
            { "text": "Not answering that." },
            { "text": FINAL }
        ],
        "conversations": [
            { "when_task_contains": "SUBTASK-scan", "turns": [
                { "tool_calls": [{ "name": "agent_ask_parent", "input": { "question": QUESTION } }] },
                { "text": "assumed public only" }
            ] }
        ]
    });
    let env = Env::with_config(
        &script,
        "[subagents]\nenabled = true\nask_parent_timeout_secs = 1\n",
    );
    let o = env.cmd(&["run", "--yes", "scan"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), FINAL);
    let (_, child) = env.transcript("r.parent_run_id IS NOT NULL");
    assert!(child.contains("No answer within 1 second"), "{child}");
    let (_, parent) = env.transcript("r.parent_run_id IS NULL");
    assert!(
        parent.contains("[sub-agent scan completed]\nassumed public only"),
        "{parent}"
    );
}

/// The user steers a running sub-agent; it gets the message before its
/// next model call.
#[tokio::test(flavor = "multi_thread")]
async fn the_user_can_message_a_running_sub_agent() {
    let script = json!({
        "turns": [
            { "tool_calls": [
                { "name": "agent_spawn", "input": { "label": "slow", "task": "SUBTASK-slow" } },
                { "name": "agent_wait", "input": {} }
            ] },
            { "text": FINAL }
        ],
        "conversations": [
            { "when_task_contains": "SUBTASK-slow", "turns": [
                { "tool_calls": [{ "name": "list_dir", "input": { "path": "." } }], "delay_ms": 1500 },
                { "text": "done, focusing on docs" }
            ] }
        ]
    });
    let env = Env::new(&script);
    let _daemon = env.daemon_with(None);
    let sock = env.socket();
    start(&sock, "start a slow one").await;
    env.wait_for("the child to start", 10, |e| {
        e.children().first().is_some_and(|c| c.1 == "running")
    });
    let child: String = env.query("SELECT id FROM runs WHERE parent_run_id IS NOT NULL");
    let (status, body) = post(
        &sock,
        &format!("/v1/runs/{child}/message"),
        json!({ "text": "focus on the docs" }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({ "ok": true }));
    let (status, _) = post(
        &sock,
        &format!("/v1/runs/{child}/message"),
        json!({ "text": "  " }),
    )
    .await;
    assert_eq!(status, 400);
    env.wait_for("the parent to complete", 30, |e| {
        e.parent_status() == "completed"
    });
    let (last, all) = env.transcript("r.parent_run_id IS NOT NULL");
    assert_eq!(last, "done, focusing on docs");
    assert!(
        all.contains("[message from the user to this agent]\nfocus on the docs"),
        "{all}"
    );
    let kinds: String = env.query(
        "SELECT group_concat(kind, ',') FROM (SELECT s.kind FROM steps s JOIN runs r ON r.id = s.run_id
         WHERE r.parent_run_id IS NOT NULL ORDER BY s.idx)",
    );
    assert_eq!(kinds, "llm,tool,inbox,llm");
}
