//! Agent profiles AP3 end-to-end (docs/agent-profiles-design.md §8): the
//! per-agent autonomy ceiling, agent templates as bundles, "Let agents
//! decide".
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use std::path::Path;

use serde_json::{Value, json};
use subagents_env::{Env, api, get, post};

async fn patch(socket: &Path, path: &str, body: Value) -> (u16, Value) {
    let r = api(socket)
        .request(reqwest::Method::PATCH, format!("http://tern{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

async fn say(socket: &Path, channel: &str, text: &str) -> String {
    let (code, sent) = post(
        socket,
        &format!("/v1/channels/{channel}/posts"),
        json!({ "text": text, "new_topic": true }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(socket, &format!("/v1/runs/{run}/wait")).await;
    run
}

fn autonomy_of(env: &Env, run: &str) -> String {
    env.query(&format!("SELECT autonomy FROM runs WHERE id = '{run}'"))
}

// ---- 1. the autonomy ceiling ----

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_autonomy_ceiling_caps_its_runs_routines_and_delegates() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Hand it over", "turns": [
                { "text": "", "tool_calls": [{ "name": "agent_spawn", "input": {
                    "label": "c", "agent": "careful", "task": "Look this up." } }] },
                { "text": "Done." }
            ]},
            { "matches": "Look this up", "turns": [{ "text": "Found it." }] },
            { "matches": "hi", "turns": [{ "text": "hello" }] },
            { "matches": "Sweep", "turns": [{ "text": "Swept." }] }
        ]}),
        "autonomy = \"autonomous\"\n",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();

    let (code, bad) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Bad", "autonomy": "reckless" }),
    )
    .await;
    assert!(
        code == 400 || code == 422,
        "an unknown level is refused: {code} {bad}"
    );

    let (code, careful) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Careful", "autonomy": "readonly" }),
    )
    .await;
    assert_eq!(code, 201, "{careful}");
    assert_eq!(careful["autonomy"], "readonly");
    let (_, bold) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Bold", "autonomy": "yolo" }),
    )
    .await;
    let (_, plain) = post(&s, "/v1/agents", json!({ "name": "Plain" })).await;
    assert_eq!(plain.get("autonomy"), None);

    // The lower of the ceiling and the configured autonomy, frozen on the run.
    let run = say(&s, careful["main_channel_id"].as_str().unwrap(), "hi").await;
    assert_eq!(autonomy_of(&env, &run), "readonly");
    // A ceiling never raises it.
    let run = say(&s, bold["main_channel_id"].as_str().unwrap(), "hi").await;
    assert_eq!(autonomy_of(&env, &run), "autonomous");
    let run = say(&s, plain["main_channel_id"].as_str().unwrap(), "hi").await;
    assert_eq!(autonomy_of(&env, &run), "autonomous");

    // A routine's own profile can't pass its agent's ceiling either.
    let (_, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Sweep", "agent": "careful",
                "autonomy": "yolo" }),
    )
    .await;
    let (_, r) = post(
        &s,
        &format!("/v1/schedules/{}/run", sch["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let run = r["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{run}/wait")).await;
    assert_eq!(autonomy_of(&env, &run), "readonly");

    // Delegating to a capped agent runs the child under the cap.
    patch(&s, "/v1/agents/tern", json!({ "delegates": ["careful"] })).await;
    let run = say(&s, "home", "Hand it over").await;
    let child: String = env.query(&format!(
        "SELECT id FROM runs WHERE parent_run_id = '{run}' AND label = 'c'"
    ));
    assert_eq!(autonomy_of(&env, &child), "readonly");
    assert_eq!(autonomy_of(&env, &run), "autonomous");

    // Editable, removable, and recorded in the ledger.
    let (_, a) = patch(&s, "/v1/agents/careful", json!({ "autonomy": null })).await;
    assert_eq!(a.get("autonomy"), None, "{a}");
    let (_, a) = patch(
        &s,
        "/v1/agents/careful",
        json!({ "autonomy": "supervised" }),
    )
    .await;
    assert_eq!(a["autonomy"], "supervised");
    let n: i64 = env.query("SELECT COUNT(*) FROM ledger WHERE kind = 'autonomy.agent'");
    assert!(n >= 2, "ledger entries: {n}");
}

// ---- 3. let agents decide ----

async fn agent(s: &Path, body: Value) -> String {
    let (code, a) = post(s, "/v1/agents", body).await;
    assert_eq!(code, 201, "{a}");
    a["id"].as_str().unwrap().to_owned()
}

async fn posts_until(s: &Path, channel: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let start = std::time::Instant::now();
    loop {
        let (_, posts) = get(s, &format!("/v1/channels/{channel}/posts?limit=100")).await;
        let mut posts = posts.as_array().cloned().unwrap_or_default();
        posts.reverse();
        if done(&posts) {
            return posts;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(20),
            "timed out; posts: {posts:#?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn send(s: &Path, channel: &str, text: &str) -> Value {
    let (code, sent) = post(
        s,
        &format!("/v1/channels/{channel}/posts"),
        json!({ "text": text }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    sent
}

#[tokio::test(flavor = "multi_thread")]
async fn let_agents_decide_routes_a_post_to_the_right_members() {
    let env = Env::with_config(
        &json!({ "conversations": [
            // The routing call (it carries "New message:"); a real
            // run's first message never does.
            { "matches": "New message:\nWhat does Acme charge", "turns": [
                { "text": "{\"answers\": {\"who\": {\"research\": 0.9, \"ops\": 0.1}}}" }] },
            { "matches": "New message:\nBoth please", "turns": [
                { "text": "Sure: {\"answers\": {\"who\": {\"ops\": 0.6, \"ghost\": 0.9, \"research\": 0.4}}}" }] },
            { "matches": "New message:\nGarbled", "turns": [
                { "text": "I think research." }] },
            { "matches": "New message:\nOutsider", "turns": [
                { "text": "{\"answers\": {\"who\": {\"outsider\": 1}}}" }] },
            { "matches": "What does Acme charge", "turns": [{ "text": "Research: $15." }] },
            { "matches": "Both please", "turns": [{ "text": "On it." }] },
            { "matches": "Garbled", "turns": [{ "text": "Lead here." }] },
            { "matches": "Outsider", "turns": [{ "text": "Lead again." }] },
            { "matches": "Unrouted", "turns": [{ "text": "Lead, as always." }] }
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let research = agent(
        &s,
        json!({ "name": "Research", "persona": "You find things out on the web." }),
    )
    .await;
    let ops = agent(
        &s,
        json!({ "name": "Ops", "persona": "You run the calendar and logistics." }),
    )
    .await;
    agent(&s, json!({ "name": "Outsider" })).await;

    for slug in ["ask", "plain"] {
        let (code, c) = post(&s, "/v1/channels", json!({ "slug": slug })).await;
        assert_eq!(code, 201, "{c}");
        for a in ["research", "ops"] {
            post(
                &s,
                &format!("/v1/channels/{slug}/agents"),
                json!({ "agent": a }),
            )
            .await;
        }
    }
    // Off by default, and only for topic channels.
    let (_, m) = get(&s, "/v1/channels/ask/agents").await;
    assert_eq!(m["let_agents_decide"], false);
    let (code, _) = patch(
        &s,
        "/v1/channels/home",
        json!({ "let_agents_decide": true }),
    )
    .await;
    assert_eq!(code, 400);
    let (code, c) = patch(&s, "/v1/channels/ask", json!({ "let_agents_decide": true })).await;
    assert_eq!(code, 200, "{c}");
    assert_eq!(c["agents"]["let_agents_decide"], true);

    // One agent picked: it answers, the lead doesn't, and a note says so.
    let sent = send(&s, "ask", "What does Acme charge?").await;
    let woke = sent["woke"].as_array().unwrap();
    assert_eq!(woke.len(), 1, "{sent:#}");
    assert_eq!(woke[0]["agent_id"], research.as_str());
    let posts = posts_until(&s, "ask", |p| {
        p.iter().any(|p| p["author_id"] == research.as_str())
    })
    .await;
    assert!(
        posts
            .iter()
            .any(|p| p["kind"] == "note"
                && p["text"].as_str().unwrap().starts_with("Sent to @research")),
        "{posts:#?}"
    );

    // The most likely member answers; names that aren't members are ignored.
    let sent = send(&s, "ask", "Both please").await;
    let agents: Vec<_> = sent["woke"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["agent_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(agents, vec![ops.clone()], "{sent:#}");

    // An answer that can't be read, or names someone who isn't a member:
    // the lead answers, as without the setting.
    for text in ["Garbled", "Outsider"] {
        let sent = send(&s, "ask", text).await;
        assert_eq!(sent["woke"][0]["agent_id"], "tern", "{text}: {sent:#}");
    }
    // A mention beats routing, and the setting off means the lead.
    let sent = send(&s, "ask", "@ops Unrouted").await;
    assert_eq!(sent["woke"][0]["agent_id"], ops.as_str());
    let sent = send(&s, "plain", "What does Acme charge?").await;
    assert_eq!(sent["woke"][0]["agent_id"], "tern", "{sent:#}");
}

#[tokio::test(flavor = "multi_thread")]
async fn routing_modes_the_reply_rule_the_per_agent_switch_and_recent_posts() {
    let who = |p: Value| json!({ "answers": { "who": p } }).to_string();
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "New message:\nBoth topics", "turns": [
                { "text": who(json!({ "research": 0.5, "ops": 0.45, "tern": 0.05 })) }] },
            // A follow-up: the router sees the agent's last post and picks it.
            { "matches": "Recent posts:\n[you] Both topics", "turns": [
                { "text": who(json!({ "ops": 0.9, "research": 0.1 })) }] },
            { "matches": "New message:\nthanks", "turns": [
                { "text": json!({ "answers": { "who": { "research": 1 }, "rule": 0.1 } }).to_string() }] },
            { "matches": "Both topics", "turns": [{ "text": "Answered." }] },
            { "matches": "thanks", "turns": [{ "text": "Welcome." }] }
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let research = agent(
        &s,
        json!({ "name": "Research", "persona": "You research." }),
    )
    .await;
    let ops = agent(&s, json!({ "name": "Ops", "persona": "You run ops." })).await;
    let (code, _) = post(&s, "/v1/channels", json!({ "slug": "ask" })).await;
    assert_eq!(code, 201);
    for a in ["research", "ops"] {
        post(&s, "/v1/channels/ask/agents", json!({ "agent": a })).await;
    }
    let (code, c) = patch(
        &s,
        "/v1/channels/ask",
        json!({ "let_agents_decide": true, "route_mode": "all" }),
    )
    .await;
    assert_eq!(code, 200, "{c}");
    assert_eq!(c["agents"]["route_mode"], "all");
    let (code, _) = patch(&s, "/v1/channels/ask", json!({ "route_mode": "some" })).await;
    assert_eq!(code, 400);

    // Every relevant agent, most likely first (tern is under 0.3).
    let sent = send(&s, "ask", "Both topics").await;
    let ids: Vec<_> = sent["woke"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["agent_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, vec![research.clone(), ops.clone()], "{sent:#}");
    posts_until(&s, "ask", |p| {
        p.iter().any(|p| p["author_id"] == ops.as_str())
            && p.iter().any(|p| p["author_id"] == research.as_str())
    })
    .await;

    // One agent mode: the follow-up sees the recent posts; the switch off
    // takes an agent out of routing.
    patch(&s, "/v1/channels/ask", json!({ "route_mode": "one" })).await;
    let sent = send(&s, "ask", "And the second part?").await;
    assert_eq!(sent["woke"][0]["agent_id"], ops.as_str(), "{sent:#}");
    let (code, a) = patch(
        &s,
        "/v1/agents/ops",
        json!({ "reply_when_relevant": false }),
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(a["reply_when_relevant"], false);
    let sent = send(&s, "ask", "And the second part?").await;
    assert_ne!(
        sent["woke"][0]["agent_id"],
        ops.as_str(),
        "ops answers only when mentioned"
    );
    let sent = send(&s, "ask", "@ops Both topics").await;
    assert_eq!(sent["woke"][0]["agent_id"], ops.as_str());

    // The reply rule: judged no, nobody replies and a note says why.
    patch(
        &s,
        "/v1/channels/ask",
        json!({ "reply_rule": "Only reply to questions" }),
    )
    .await;
    let sent = send(&s, "ask", "thanks").await;
    assert_eq!(sent["woke"].as_array().map_or(0, Vec::len), 0, "{sent:#}");
    let posts = posts_until(&s, "ask", |p| {
        p.iter()
            .any(|p| p["kind"] == "note" && p["text"].as_str().unwrap().contains("reply rule"))
    })
    .await;
    assert!(!posts.is_empty());
    // A one-line description can be set, and is capped.
    let (code, a) = patch(
        &s,
        "/v1/agents/ops",
        json!({ "description": "Runs the calendar" }),
    )
    .await;
    assert_eq!(
        (code, a["description"].as_str()),
        (200, Some("Runs the calendar"))
    );
    let (code, _) = patch(
        &s,
        "/v1/agents/ops",
        json!({ "description": "x".repeat(200) }),
    )
    .await;
    assert_eq!(code, 400);
}
