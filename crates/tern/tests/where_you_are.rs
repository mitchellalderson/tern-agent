//! Every conversation run's frozen prompt says where it is and where its
//! reply goes (DECISIONS.md "Where you are"): a topic channel without
//! instructions, Home, an agent's DM, a sub-agent and a routine.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use serde_json::json;
use subagents_env::{Env, get, post};

async fn say(s: &std::path::Path, channel: &str, text: &str) -> String {
    let (code, sent) = post(
        s,
        &format!("/v1/channels/{channel}/posts"),
        json!({ "text": text }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(s, &format!("/v1/runs/{run}/wait")).await;
    run
}

fn prompt(env: &Env, run: &str) -> String {
    env.db()
        .query_row("SELECT system_prompt FROM runs WHERE id = ?1", [run], |r| {
            r.get(0)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_run_is_told_where_it_is_and_where_its_reply_goes() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Look into Grok", "turns": [{ "text": "Grok has bots." }] },
        { "matches": "Spawn a helper", "turns": [
            { "text": "", "tool_calls": [{ "name": "agent_spawn", "input": {
                "label": "scout", "task": "Scout the docs." } },
                { "name": "agent_wait", "input": {} }] },
            { "text": "Done." }
        ]},
        { "matches": "Scout the docs", "turns": [{ "text": "Scouted." }] },
        { "matches": "Morning check", "turns": [{ "text": "All quiet." }] },
        { "matches": "hello", "turns": [{ "text": "Hi." }] }
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, research) = post(&s, "/v1/agents", json!({ "name": "Research" })).await;
    let dm = research["main_channel_id"].as_str().unwrap().to_owned();
    post(&s, "/v1/channels", json!({ "slug": "grokbot-research" })).await;
    post(
        &s,
        "/v1/channels/grokbot-research/agents",
        json!({ "agent": "research" }),
    )
    .await;

    // A topic channel with no instructions still says where it is, and
    // that the answer is posted there by itself.
    let run = say(&s, "grokbot-research", "Look into Grok").await;
    let p = prompt(&env, &run);
    assert!(p.contains("## Where you are"), "{p}");
    assert!(
        p.contains("You're in the #grokbot-research channel."),
        "{p}"
    );
    assert!(p.contains(
        "Your reply is posted to #grokbot-research automatically: don't offer to post it there."
    ));
    assert!(
        p.contains("Agents in #grokbot-research: @tern (tern, lead, you), @research (Research)."),
        "{p}"
    );
    assert!(p.contains("everyone's posts here"));
    assert!(!p.contains("instructions for"), "none given");

    // Home and a DM.
    let run = say(&s, "home", "hello").await;
    let p = prompt(&env, &run);
    assert!(
        p.contains("You're in Home, your direct line with the user."),
        "{p}"
    );
    assert!(!p.contains("channel_post` only"));
    let run = say(&s, &dm, "hello").await;
    assert!(prompt(&env, &run).contains("You're in Research's DM"));

    // A sub-agent reports to whoever spawned it (a fresh channel, so the
    // run's conversation starts with this script).
    post(&s, "/v1/channels", json!({ "slug": "scouting" })).await;
    let run = say(&s, "scouting", "Spawn a helper").await;
    let child: String = env.query(&format!(
        "SELECT id FROM runs WHERE parent_run_id = '{run}'"
    ));
    let p = prompt(&env, &child);
    assert!(
        p.contains("You're working for tern in #scouting."),
        "{p}"
    );
    assert!(p.contains("it goes back to tern, not to the user or the channel. Don't post"));

    // A routine's result goes where the routine posts.
    let (_, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Morning check", "channel": "grokbot-research" }),
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
    let p = prompt(&env, &run);
    assert!(p.contains("This is a run of the routine"), "{p}");
    assert!(
        p.contains("Your result is posted to #grokbot-research by itself"),
        "{p}"
    );
}
