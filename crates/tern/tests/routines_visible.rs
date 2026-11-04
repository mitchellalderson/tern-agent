//! Routines made in conversation stay findable: one made in a topic channel
//! posts there, can run as another agent (within `delegates`), and clients
//! hear `schedules_changed` so every list refreshes live.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use serde_json::json;
use subagents_env::{Env, Events, get, post};

/// The result of `run`'s one tool call.
fn tool_output(env: &Env, run: &str) -> String {
    env.db()
        .query_row(
            "SELECT output_json FROM steps WHERE run_id = ?1 AND kind = 'tool' ORDER BY idx LIMIT 1",
            [run],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_routine_made_in_a_topic_runs_as_research_and_shows_everywhere() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Look up this week's AI news", "turns": [
            { "text": "Three launches this week." }
        ]},
        { "matches": "Every morning", "turns": [
            { "text": "", "tool_calls": [{ "name": "schedule_create", "input": {
                "cron": "0 9 * * *", "agent": "research", "initiative": "interrupt",
                "prompt": "Look up this week's AI news and summarise it." } }] },
            { "text": "Done: Research will do it at 9." }
        ]},
        { "matches": "Inbox, schedule", "turns": [
            { "text": "", "tool_calls": [{ "name": "schedule_create", "input": {
                "cron": "0 9 * * *", "agent": "research", "prompt": "Anything." } }] },
            { "text": "Couldn't." }
        ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, research) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Research", "template": "research", "model": "mock:research" }),
    )
    .await;
    let research = research["id"].as_str().unwrap().to_owned();
    let (code, inbox) = post(&s, "/v1/agents", json!({ "name": "Inbox" })).await;
    assert_eq!(code, 201, "{inbox}");
    let (code, topic) = post(
        &s,
        "/v1/channels",
        json!({ "slug": "news", "title": "News" }),
    )
    .await;
    assert_eq!(code, 201, "{topic}");
    let topic = topic["id"].as_str().unwrap().to_owned();
    let mut events = Events::open(&s).await;

    // tern, asked in #news, makes a routine that runs as Research.
    let (code, sent) = post(
        &s,
        "/v1/channels/news/posts",
        json!({ "text": "Every morning, have Research look up AI news" }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{run}/wait")).await;
    let out = tool_output(&env, &run);
    assert!(out.contains("runs as Research"), "{out}");
    assert!(out.contains("posted to #news"), "{out}");
    assert!(out.contains("(tern://routine/sch_"), "{out}");

    let (_, list) = get(&s, "/v1/schedules").await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    let r = &list[0];
    let id = r["id"].as_str().unwrap().to_owned();
    // Research's (its side panel), posting to #news (the channel's list).
    assert_eq!(r["agent_id"], research.as_str(), "{r}");
    assert_eq!(r["channel_id"], topic.as_str(), "{r}");
    assert!(out.contains(&format!("tern://routine/{id}")), "{out}");
    // Research joined #news to post there.
    let (_, ch) = get(&s, "/v1/channels/news/agents").await;
    assert!(ch.to_string().contains(&research), "{ch}");

    // Clients heard about it.
    events
        .until(10, |evs| {
            evs.iter()
                .any(|e| e["schedules_changed"]["id"] == id.as_str())
        })
        .await;

    // It runs as Research, on Research's model, and posts to #news.
    let (code, started) = post(&s, &format!("/v1/schedules/{id}/run"), json!({})).await;
    assert_eq!(code, 200, "{started}");
    let routine_run = started["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{routine_run}/wait")).await;
    let (agent, model): (Option<String>, String) = env
        .db()
        .query_row(
            "SELECT s.agent_id, r.model FROM runs r JOIN sessions s ON s.id = r.session_id
             WHERE r.id = ?1",
            [&routine_run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(agent.as_deref(), Some(research.as_str()));
    assert!(model.ends_with("research"), "{model}");
    env.wait_for("the result in #news", 10, |env| {
        env.db()
            .query_row(
                "SELECT count(*) FROM posts WHERE channel_id = ?1 AND text LIKE '%Three launches%'",
                [&topic],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    });

    // Inbox delegates to no one (its `delegates` is unset), so it can't
    // hand Research a routine.
    let (code, sent) = post(
        &s,
        "/v1/channels/dm-inbox/posts",
        json!({ "text": "Inbox, schedule something" }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{run}/wait")).await;
    let out = tool_output(&env, &run);
    assert!(out.contains("isn't in your delegates"), "{out}");
    let (_, list) = get(&s, "/v1/schedules").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}
