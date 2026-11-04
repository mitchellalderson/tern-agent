//! Agent profiles AP3 end-to-end (docs/agent-profiles-design.md §8): the
//! per-agent autonomy ceiling, agent templates as bundles, "Let agents
//! decide" and a Telegram bot per agent.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

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
    assert!(code == 400 || code == 422, "an unknown level is refused: {code} {bad}");

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
    let (_, a) = patch(&s, "/v1/agents/careful", json!({ "autonomy": "supervised" })).await;
    assert_eq!(a["autonomy"], "supervised");
    let n: i64 = env.query("SELECT COUNT(*) FROM ledger WHERE kind = 'autonomy.agent'");
    assert!(n >= 2, "ledger entries: {n}");
}
