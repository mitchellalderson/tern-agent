//! T5b skill capture end-to-end (docs/personal-agent-tooling-spec.md §7): a
//! long, clean run is offered once; accepting writes a draft that stays out
//! of the prompt until it is published.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use std::path::Path;

use serde_json::{Value, json};
use subagents_env::{Env, get, post};

fn search() -> Value {
    json!({ "text": "", "tool_calls": [{ "name": "memory_search", "input": { "query": "x" } }] })
}

/// A script whose conversation `matches` makes `calls` tool calls, then answers.
fn script(calls: usize) -> Value {
    let mut turns: Vec<Value> = (0..calls).map(|_| search()).collect();
    turns.push(json!({ "text": "Done." }));
    json!({
        "skill_draft": "```\n---\nname: Check The Archive\ndescription: Search memory for the thing\n---\n# Steps\n1. Search memory.\n```",
        "turns": turns,
        "conversations": [{ "matches": "hello again", "turns": [{ "text": "Hi." }] }]
    })
}

async fn run(socket: &Path, text: &str) -> String {
    let (code, started) = post(socket, "/v1/runs", json!({ "text": text })).await;
    assert_eq!(code, 200, "{started}");
    let run = started["run_id"].as_str().unwrap().to_owned();
    let (_, done) = get(socket, &format!("/v1/runs/{run}/wait")).await;
    assert_eq!(done["status"], "completed", "{done}");
    run
}

const LOW: &str = "[skills]\ncapture_min_tool_calls = 3\n";

#[tokio::test(flavor = "multi_thread")]
async fn a_long_clean_run_is_offered_and_accepting_writes_a_draft() {
    let env = Env::with_config(&script(3), LOW);
    let _d = env.daemon_with(None);
    let s = env.socket();
    let run = run(&s, "dig through the archive").await;

    let (_, offers) = get(&s, "/v1/skill-offers").await;
    let offers = offers.as_array().unwrap();
    assert_eq!(offers.len(), 1, "{offers:?}");
    assert_eq!(offers[0]["run_id"], run.as_str());
    assert_eq!(offers[0]["tool_calls"], 3);
    assert_eq!(offers[0]["state"], "pending");
    let id = offers[0]["id"].as_i64().unwrap();

    // Pending offers are in Tasks, "needs you".
    let (_, tasks) = get(&s, "/v1/tasks").await;
    let t = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["kind"] == "skill_offer")
        .unwrap_or_else(|| panic!("{tasks:#}"));
    assert_eq!(
        (t["section"].as_str(), t["id"].as_str()),
        (Some("needs_you"), Some(id.to_string().as_str()))
    );

    // Nothing is written until the user accepts.
    assert!(!env.tern_home.join("skills/check-the-archive").exists());
    let (code, a) = post(&s, &format!("/v1/skill-offers/{id}/accept"), json!({})).await;
    assert_eq!(code, 200, "{a}");
    assert_eq!(a["skill"]["name"], "check-the-archive");
    assert_eq!(a["skill"]["draft"], true);
    assert_eq!(a["skill"]["captured_from"], run.as_str());
    assert_eq!(a["offer"]["state"], "accepted");
    let file = env.tern_home.join("skills/check-the-archive/SKILL.md");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("draft: true") && text.contains("# Steps"),
        "{text}"
    );

    // Decided once; no pending offer or task is left.
    let (code, _) = post(&s, &format!("/v1/skill-offers/{id}/accept"), json!({})).await;
    assert_eq!(code, 409);
    let (code, _) = post(&s, &format!("/v1/skill-offers/{id}/dismiss"), json!({})).await;
    assert_eq!(code, 409);
    let (_, offers) = get(&s, "/v1/skill-offers").await;
    assert!(offers.as_array().unwrap().is_empty());
    let (_, all) = get(&s, "/v1/skill-offers?state=all").await;
    assert_eq!(all[0]["skill"], "check-the-archive");
    let (_, tasks) = get(&s, "/v1/tasks").await;
    assert!(
        tasks["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["kind"] != "skill_offer")
    );

    // A draft is listed as one, readable, and not usable by the agent.
    let (_, list) = get(&s, "/v1/skills").await;
    let draft = list
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "check-the-archive")
        .unwrap();
    assert_eq!(draft["draft"], true);
    let (_, one) = get(&s, "/v1/skills/check-the-archive").await;
    assert!(one["content"].as_str().unwrap().contains("# Steps"));
    let (code, _) = post(&s, "/v1/skills/connect-a-service/discard", json!({})).await;
    assert_eq!(code, 404, "published skills are never discarded");

    // The next run's prompt doesn't list the draft ...
    let next = run_prompt(&env, &s).await;
    assert!(!next.contains("check-the-archive"), "{next}");
    // ... until it is published.
    let (code, p) = post(&s, "/v1/skills/check-the-archive/publish", json!({})).await;
    assert_eq!(code, 200, "{p}");
    assert_eq!(p["draft"], false);
    assert!(!std::fs::read_to_string(&file).unwrap().contains("draft:"));
    let next = run_prompt(&env, &s).await;
    assert!(
        next.contains("- check-the-archive: Search memory for the thing"),
        "{next}"
    );

    // `tern skills` shows it as published.
    let out = env.cmd(&["skills"]).output().unwrap();
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.contains("check-the-archive") && !out.contains("[draft]"),
        "{out}"
    );
}

/// The system prompt of a fresh run (a short one, so it isn't offered).
async fn run_prompt(env: &Env, s: &Path) -> String {
    let (code, started) = post(s, "/v1/runs", json!({ "text": "hello again" })).await;
    assert_eq!(code, 200, "{started}");
    let id = started["run_id"].as_str().unwrap();
    get(s, &format!("/v1/runs/{id}/wait")).await;
    env.query(&format!("SELECT system_prompt FROM runs WHERE id = '{id}'"))
}

#[tokio::test(flavor = "multi_thread")]
async fn dismissing_closes_the_offer_for_good() {
    let env = Env::with_config(&script(3), LOW);
    let _d = env.daemon_with(None);
    let s = env.socket();
    run(&s, "dig through the archive").await;
    let (_, offers) = get(&s, "/v1/skill-offers").await;
    let id = offers[0]["id"].as_i64().unwrap();
    let (code, _) = post(&s, &format!("/v1/skill-offers/{id}/dismiss"), json!({})).await;
    assert_eq!(code, 200);
    let (code, _) = post(&s, &format!("/v1/skill-offers/{id}/accept"), json!({})).await;
    assert_eq!(code, 409);
    assert_eq!(
        post(&s, "/v1/skill-offers/999/accept", json!({})).await.0,
        404
    );
    let (_, all) = get(&s, "/v1/skill-offers?state=all").await;
    assert_eq!(all[0]["state"], "dismissed");
    assert!(
        !env.tern_home.join("skills/check-the-archive").exists(),
        "dismissing writes nothing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn short_runs_and_a_disabled_setting_make_no_offers() {
    // Two tool calls, under the minimum of three.
    let env = Env::with_config(&script(2), LOW);
    let _d = env.daemon_with(None);
    let s = env.socket();
    run(&s, "a short one").await;
    let (_, offers) = get(&s, "/v1/skill-offers?state=all").await;
    assert!(offers.as_array().unwrap().is_empty(), "{offers}");
    drop(_d);

    let env = Env::with_config(
        &script(3),
        "[skills]\ncapture = false\ncapture_min_tool_calls = 3\n",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    run(&s, "dig through the archive").await;
    let (_, offers) = get(&s, "/v1/skill-offers?state=all").await;
    assert!(offers.as_array().unwrap().is_empty(), "{offers}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_default_minimum_is_eight_tool_calls() {
    let env = Env::with_config(&script(7), "");
    let _d = env.daemon_with(None);
    let s = env.socket();
    run(&s, "seven calls").await;
    let (_, offers) = get(&s, "/v1/skill-offers?state=all").await;
    assert!(offers.as_array().unwrap().is_empty());
    drop(_d);

    let env = Env::with_config(&script(8), "");
    let _d = env.daemon_with(None);
    let s = env.socket();
    run(&s, "eight calls").await;
    let (_, offers) = get(&s, "/v1/skill-offers").await;
    assert_eq!(offers.as_array().unwrap().len(), 1, "{offers}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conversation_with_an_offer_waiting_isnt_offered_again() {
    // Two long runs in one conversation: one offer.
    // Turns continue across the conversation: three searches, an answer,
    // three more, another answer.
    let mut turns: Vec<Value> = (0..3).map(|_| search()).collect();
    turns.push(json!({ "text": "Done." }));
    turns.extend((0..3).map(|_| search()));
    turns.push(json!({ "text": "Done again." }));
    let env = Env::with_config(&json!({ "turns": turns }), LOW);
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, started) = post(&s, "/v1/runs", json!({ "text": "first" })).await;
    let run1 = started["run_id"].as_str().unwrap().to_owned();
    let session = started["session_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{run1}/wait")).await;
    let (_, offers) = get(&s, "/v1/skill-offers").await;
    assert_eq!(offers.as_array().unwrap().len(), 1);
    let (_, started) = post(
        &s,
        "/v1/runs",
        json!({ "text": "second", "session_id": session }),
    )
    .await;
    let run2 = started["run_id"].as_str().unwrap();
    let (_, done) = get(&s, &format!("/v1/runs/{run2}/wait")).await;
    assert_eq!(done["status"], "completed", "{done}");
    let (_, offers) = get(&s, "/v1/skill-offers?state=all").await;
    assert_eq!(offers.as_array().unwrap().len(), 1, "{offers}");
    // The second run did make its tool calls (the offer was held back, not
    // missed): its messages carry them.
    let n: i64 = env.query(&format!(
        "SELECT count(*) FROM messages WHERE run_id = '{run2}' AND content_json LIKE '%memory_search%'"
    ));
    assert_eq!(n, 3);
}
