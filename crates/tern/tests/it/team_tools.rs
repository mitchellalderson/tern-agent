//! Agents working as a team: `delegate_team` (one card, reports together),
//! the "Message from X" card, `brief_team` (read at the next run, or wake),
//! `inspect_work`, and what a hand-off that didn't finish tells its caller.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use subagents_env::{Env, get, post};

async fn say(socket: &Path, channel: &str, text: &str, new_topic: bool) -> String {
    let (code, sent) = post(
        socket,
        &format!("/v1/channels/{channel}/posts"),
        json!({ "text": text, "new_topic": new_topic }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(socket, &format!("/v1/runs/{run}/wait")).await;
    run
}

async fn agent(socket: &Path, name: &str) -> Value {
    let (code, a) = post(socket, "/v1/agents", json!({ "name": name })).await;
    assert_eq!(code, 201, "{a}");
    a
}

/// Posts of `channel` of one kind, once there is one (bounded wait).
async fn posts_of(socket: &Path, channel: &str, kind: &str, at_least: usize) -> Vec<Value> {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, posts) = get(socket, &format!("/v1/channels/{channel}/posts")).await;
        let v: Vec<Value> = posts
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["kind"] == kind)
            .cloned()
            .collect();
        if v.len() >= at_least || Instant::now() > end {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The result of the `n`th tool step of `run`.
fn tool_result(env: &Env, run: &str, n: usize) -> String {
    env.db()
        .query_row(
            "SELECT output_json FROM steps WHERE run_id = ?1 AND kind = 'tool' ORDER BY idx LIMIT 1 OFFSET ?2",
            rusqlite::params![run, n as i64],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_question_split_across_lanes_comes_back_together_under_one_card() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Scout: find Alpha", "turns": [
            { "text": "Alpha charges $10 a seat.", "delay_ms": 300 } ]},
        { "matches": "Research: size Beta", "turns": [
            { "text": "Beta is a $2M market." } ]},
        { "matches": "Nobody home", "turns": [
            { "text": "", "tool_calls": [{ "name": "delegate_team", "input": { "lanes": [
                { "agent": "scout", "task": "Fine." },
                { "agent": "ghost", "task": "Not here." } ] } }] },
            { "text": "Refused." } ]},
        { "matches": "One lane only", "turns": [
            { "text": "", "tool_calls": [{ "name": "delegate_team", "input": { "lanes": [
                { "agent": "scout", "task": "Alone." } ] } }] },
            { "text": "Refused." } ]},
        { "matches": "Compare Alpha and Beta", "turns": [
            { "text": "", "tool_calls": [{ "name": "delegate_team", "input": { "lanes": [
                { "agent": "scout", "task": "Scout: find Alpha's pricing." },
                { "agent": "research", "task": "Research: size Beta's market." } ] } }] },
            { "text": "Scout found Alpha at $10; Research sized Beta at $2M." } ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    agent(&s, "Scout").await;
    agent(&s, "Research").await;

    let run = say(&s, "home", "Compare Alpha and Beta", true).await;
    let (_, out) = get(&s, &format!("/v1/runs/{run}/wait")).await;
    assert_eq!(out["status"], "completed", "{out}");

    // Both lanes ran as the teammates, as this run's sub-agents.
    let kids = env.children();
    let labels: Vec<&str> = kids.iter().map(|k| k.0.as_str()).collect();
    assert_eq!(labels, ["team-research", "team-scout"], "{kids:?}");
    assert!(kids.iter().all(|k| k.1 == "completed"), "{kids:?}");

    // One combined card in the caller's chat, and no per-teammate cards.
    let cards = posts_of(&s, "home", "handed", 1).await;
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0]["text"], "Handed to Scout and Research");
    assert!(posts_of(&s, "home", "message_from", 0).await.is_empty());

    // The reports came back together, each under its teammate's name.
    let result = tool_result(&env, &run, 0);
    assert!(result.contains("Alpha charges $10 a seat."), "{result}");
    assert!(result.contains("Beta is a $2M market."), "{result}");
    assert!(
        result.contains("\\\"teammate\\\":\\\"Scout\\\""),
        "{result}"
    );
    assert!(result.contains("crediting each teammate"), "{result}");

    // One lane isn't a team; an unknown teammate starts nobody.
    let before = env.children().len();
    let run = say(&s, "home", "One lane only", true).await;
    let r = tool_result(&env, &run, 0);
    assert!(r.contains("takes 2 to 3 lanes"), "{r}");
    let run = say(&s, "home", "Nobody home", true).await;
    let r = tool_result(&env, &run, 0);
    assert!(r.contains("no agent \\\"ghost\\\""), "{r}");
    assert_eq!(env.children().len(), before, "nothing started");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_from_a_delegate_is_a_card_in_the_askers_chat() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Look up Gamma", "turns": [
            { "text": "Gamma launched in March." } ]},
        { "matches": "Ask Scout about Gamma", "turns": [
            { "text": "", "tool_calls": [{ "name": "agent_spawn", "input": {
                "label": "gamma", "agent": "scout", "task": "Look up Gamma." } },
                { "name": "agent_wait", "input": {} }] },
            { "text": "Scout says Gamma launched in March." } ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let scout = agent(&s, "Scout").await;
    say(&s, "home", "Ask Scout about Gamma", true).await;

    let cards = posts_of(&s, "home", "message_from", 1).await;
    assert_eq!(cards.len(), 1, "{cards:?}");
    let text = cards[0]["text"].as_str().unwrap();
    assert!(text.starts_with("Message from Scout"), "{text}");
    assert!(text.contains("Gamma launched in March."), "{text}");
    assert_eq!(cards[0]["author_id"], "tern:scout");
    // Nothing of it lands in the delegate's DM but its own work pointer.
    let dm = scout["main_channel_id"].as_str().unwrap();
    assert!(posts_of(&s, dm, "message_from", 0).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_brief_is_read_at_the_next_run_and_wakes_only_when_asked() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Brief a ghost", "turns": [
            { "text": "", "tool_calls": [{ "name": "brief_team", "input": {
                "agents": ["ghost"], "message": "Hello." } }] },
            { "text": "Refused." } ]},
        { "matches": "Plan changed urgently", "turns": [
            { "text": "", "tool_calls": [{ "name": "brief_team", "input": {
                "agents": ["scout"], "message": "Launch moves to May.", "now": true } }] },
            { "text": "Woke Scout." } ]},
        { "matches": "What is the plan", "turns": [
            { "text": "The plan is May." } ]},
        { "matches": "Plan changed quietly", "turns": [
            { "text": "", "tool_calls": [{ "name": "brief_team", "input": {
                "agents": ["scout"], "message": "Launch moves to May." } }] },
            { "text": "Told Scout." } ]},
        { "matches": "Launch moves to May", "turns": [
            { "text": "Noted: May.", "delay_ms": 100 } ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let scout = agent(&s, "Scout").await;
    let dm = scout["main_channel_id"].as_str().unwrap().to_owned();

    // Quiet: a post in Scout's DM, and nobody woken.
    say(&s, "home", "Plan changed quietly", true).await;
    let briefs = posts_of(&s, &dm, "brief", 1).await;
    assert_eq!(briefs.len(), 1, "{briefs:?}");
    assert_eq!(briefs[0]["text"], "Launch moves to May.");
    assert_eq!(briefs[0]["author"], "tern");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let n: i64 = env.query(
        "SELECT count(*) FROM runs r JOIN sessions s ON s.id = r.session_id WHERE s.agent_id = 'tern:scout'",
    );
    assert_eq!(n, 0, "a plain brief wakes nobody");

    // Scout's next run is shown it with the other new posts.
    let run = say(&s, &dm, "What is the plan?", false).await;
    let (_, all) = env.transcript(&format!("r.id = '{run}'"));
    assert!(all.contains("Launch moves to May."), "{all}");

    // `now` wakes Scout, who is told who briefed it.
    say(&s, "home", "Plan changed urgently", true).await;
    env.wait_for("a wake", 10, |e| {
        e.query::<i64>(
            "SELECT count(*) FROM runs r JOIN sessions s ON s.id = r.session_id
             WHERE s.agent_id = 'tern:scout' AND r.trigger = 'wake'",
        ) == 1
    });
    let woke: String = env.query(
        "SELECT m.text FROM runs r JOIN messages m ON m.id = r.input_message_id WHERE r.trigger = 'wake'",
    );
    assert!(woke.starts_with("tern briefed you:"), "{woke}");
    assert!(woke.contains("no reply is needed"), "{woke}");

    // An unknown teammate is refused, naming the real ones.
    let run = say(&s, "home", "Brief a ghost", true).await;
    let r = tool_result(&env, &run, 0);
    assert!(r.contains("no teammate"), "{r}");
    assert!(r.contains("scout"), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_can_look_at_its_own_runs_and_only_its_own() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "What did I do", "turns": [
            { "text": "", "tool_calls": [{ "name": "inspect_work", "input": {} }] },
            { "text": "Looked." } ]},
        { "matches": "Scout works", "turns": [ { "text": "Scouting." } ]},
        { "matches": "Say hello", "turns": [ { "text": "Hello." } ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let scout = agent(&s, "Scout").await;
    let first = say(&s, "home", "Say hello", true).await;
    let theirs = say(
        &s,
        scout["main_channel_id"].as_str().unwrap(),
        "Scout works",
        true,
    )
    .await;
    let run = say(&s, "home", "What did I do", true).await;

    let r = tool_result(&env, &run, 0);
    assert!(r.contains(&first), "its earlier run is listed: {r}");
    assert!(!r.contains(&theirs), "another agent's run is not: {r}");
    assert!(r.contains("\\\"status\\\":\\\"completed\\\""), "{r}");
    assert!(r.contains("\\\"tokens\\\""), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hand_off_that_ran_out_says_what_got_done_and_what_to_do() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Dig forever", "turns": [
            { "text": "", "tool_calls": [{ "name": "list_dir", "input": { "path": "." } }] },
            { "text": "Found one folder so far." } ]},
        { "matches": "Run a deep job", "turns": [
            { "text": "", "tool_calls": [{ "name": "agent_spawn", "input": {
                "label": "deep", "task": "Dig forever.", "max_iterations": 1 } },
                { "name": "agent_wait", "input": {} }] },
            { "text": "It stopped." } ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let run = say(&s, "home", "Run a deep job", true).await;
    let r = tool_result(&env, &run, 1);
    assert!(r.contains("\\\"status\\\":\\\"stopped\\\""), "{r}");
    assert!(r.contains("ran out of steps or tokens"), "{r}");
    assert!(r.contains("What got done"), "{r}");
    assert!(r.contains("ask the user"), "{r}");
}
