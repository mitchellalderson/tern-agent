//! T2 channels end-to-end (docs/channels-design.md §6): channels and posts
//! over the API, sends into a channel's threads, the posts finished runs
//! and routines write, and (C1) Home's stream.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use subagents_env::{Env, Events, api, get, post};

async fn patch(socket: &Path, path: &str, body: Value) -> (u16, Value) {
    let r = api(socket)
        .patch(format!("http://tern{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

/// Wait until `GET path` returns something `ok` accepts.
async fn wait_get(socket: &Path, path: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, v) = get(socket, path).await;
        if ok(&v) {
            return v;
        }
        assert!(Instant::now() < end, "{path}: {v:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_run(socket: &Path, run: &str) -> Value {
    get(socket, &format!("/v1/runs/{run}/wait")).await.1
}

fn texts(posts: &Value) -> Vec<String> {
    posts
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["text"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn channels_are_created_listed_changed_and_archived() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let _d = env.daemon_with(None);
    let s = env.socket();

    let (_, list) = get(&s, "/v1/channels").await;
    assert_eq!(list[0]["slug"], "home");
    assert_eq!(list[0]["kind"], "main");

    let (code, c) = post(
        &s,
        "/v1/channels",
        json!({ "slug": "#finances", "title": "Finances" }),
    )
    .await;
    assert_eq!(code, 201, "{c}");
    assert_eq!(
        (c["slug"].as_str(), c["kind"].as_str()),
        (Some("finances"), Some("topic"))
    );
    for (body, want) in [
        (json!({ "slug": "Bad Name" }), 400),
        (json!({ "slug": "finances" }), 409),
        (json!({ "slug": "x", "kind": "main" }), 400),
    ] {
        assert_eq!(post(&s, "/v1/channels", body).await.0, want);
    }

    let (code, c) = patch(
        &s,
        "/v1/channels/finances",
        json!({ "title": "Money", "instructions": "Amounts in EUR." }),
    )
    .await;
    assert_eq!(code, 200, "{c}");
    assert_eq!(
        (c["title"].as_str(), c["instructions"].as_str()),
        (Some("Money"), Some("Amounts in EUR."))
    );
    let (_, c) = patch(&s, "/v1/channels/finances", json!({ "instructions": null })).await;
    assert_eq!(c["instructions"], Value::Null);

    assert_eq!(
        patch(&s, "/v1/channels/home", json!({ "archived": true }))
            .await
            .0,
        400
    );
    patch(&s, "/v1/channels/finances", json!({ "archived": true })).await;
    let (_, list) = get(&s, "/v1/channels").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, list) = get(&s, "/v1/channels?archived=1").await;
    assert_eq!(list[1]["archived"], true);
    let (code, _) = post(&s, "/v1/channels/finances/posts", json!({ "text": "hi" })).await;
    assert_eq!(code, 400, "an archived channel takes no sends");
    assert_eq!(get(&s, "/v1/channels/nope/posts").await.0, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_into_a_topic_channel_posts_and_the_answer_replies_to_it() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "How much did we spend", "turns": [
                { "text": "About 1,240 EUR." },
                { "text": "Northwind is 310 EUR." }
            ]},
            { "matches": "Why so much?", "turns": [{ "text": "Mostly card fees." }] }
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    post(
        &s,
        "/v1/channels",
        json!({ "slug": "finances", "instructions": "Amounts in EUR." }),
    )
    .await;
    let mut events = Events::open(&s).await;

    let (code, sent) = post(
        &s,
        "/v1/channels/finances/posts",
        json!({ "text": "How much did we spend?" }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let mine = sent["post"].clone();
    assert_eq!(
        (
            mine["author"].as_str(),
            mine["author_id"].as_str(),
            mine["kind"].as_str()
        ),
        (Some("you"), Some("owner"), Some("message"))
    );
    assert_eq!(sent["queued"], false);
    let main = sent["session_id"].as_str().unwrap().to_owned();
    wait_run(&s, sent["run_id"].as_str().unwrap()).await;

    let posts = wait_get(&s, "/v1/channels/finances/posts", |v| {
        v.as_array().is_some_and(|a| a.len() == 2)
    })
    .await;
    assert_eq!(
        texts(&posts),
        ["About 1,240 EUR.", "How much did we spend?"],
        "newest first"
    );
    let answer = &posts[0];
    assert_eq!(
        (
            answer["author"].as_str(),
            answer["author_id"].as_str(),
            answer["kind"].as_str()
        ),
        (Some("tern"), Some("tern"), Some("answer"))
    );
    assert_eq!(answer["reply_to"], mine["id"]);
    assert_eq!(answer["session_id"], main.as_str());

    // The channel's instructions are in the run's system prompt.
    let system: String = env.query("SELECT system_prompt FROM runs LIMIT 1");
    assert!(system.contains("## Channel #finances") && system.contains("Amounts in EUR."));

    // Run events carry the channel; posts are announced.
    let channel_id = posts[0]["channel_id"].as_str().unwrap().to_owned();
    let evs = events
        .until(10, |evs| {
            evs.iter()
                .filter(|e| e.get("post_created").is_some())
                .count()
                >= 2
        })
        .await;
    let started = evs.iter().find(|e| e["type"] == "run_started").unwrap();
    assert_eq!(started["channel_id"], channel_id.as_str());

    // A second send continues the main thread.
    let (_, again) = post(
        &s,
        "/v1/channels/finances/posts",
        json!({ "text": "And Northwind?" }),
    )
    .await;
    assert_eq!(again["session_id"], main.as_str());
    wait_run(&s, again["run_id"].as_str().unwrap()).await;

    // A reply starts a side thread branched from that post, and its answer
    // replies to the reply.
    let (_, side) = post(
        &s,
        "/v1/channels/finances/posts",
        json!({ "text": "Why so much?", "reply_to": answer["id"] }),
    )
    .await;
    assert_ne!(side["session_id"], main.as_str());
    assert_eq!(side["post"]["reply_to"], answer["id"]);
    wait_run(&s, side["run_id"].as_str().unwrap()).await;
    let posts = wait_get(&s, "/v1/channels/finances/posts", |v| {
        v.as_array().is_some_and(|a| a.len() == 6)
    })
    .await;
    assert_eq!(posts[0]["text"], "Mostly card fees.");
    assert_eq!(posts[0]["reply_to"], side["post"]["id"]);
    let replied = posts
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == answer["id"])
        .unwrap();
    assert_eq!(replied["replies"], 1);

    let (_, threads) = get(&s, "/v1/sessions?channel=finances").await;
    let side_thread = threads
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == side["session_id"])
        .unwrap();
    assert_eq!(
        side_thread["branched_from"],
        format!("post:{}", answer["id"])
    );

    // A new thread over /v1/runs.
    let (_, run) = post(
        &s,
        "/v1/runs",
        json!({ "text": "Why so much?", "channel": "finances" }),
    )
    .await;
    let (_, threads) = get(&s, "/v1/sessions?channel=finances").await;
    assert!(
        threads
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == run["session_id"])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn home_sends_write_no_posts_and_routines_post_where_they_are_pointed() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "hello", "turns": [{ "text": "Hi there." }] },
            { "matches": "Sweep the inbox", "turns": [{ "text": "2 invoices need a decision." }] },
            { "matches": "Anything new?", "turns": [{ "text": "NO_REPLY" }] },
            { "matches": "Morning brief", "turns": [{ "text": "Two meetings today." }] }
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();

    let (_, sent) = post(&s, "/v1/channels/home/posts", json!({ "text": "hello" })).await;
    assert_eq!(sent["post"], Value::Null);
    wait_run(&s, sent["run_id"].as_str().unwrap()).await;
    let (_, home) = get(&s, "/v1/channels").await;
    assert_eq!(home[0]["main_session_id"], sent["session_id"]);
    assert_eq!(get(&s, "/v1/channels/home/posts").await.1, json!([]));

    post(&s, "/v1/channels", json!({ "slug": "email" })).await;
    let (code, sweep) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Sweep the inbox", "channel": "email" }),
    )
    .await;
    assert_eq!(code, 201, "{sweep}");
    let email_id = get(&s, "/v1/channels").await.1[1]["id"].clone();
    assert_eq!(sweep["channel_id"], email_id);
    let (_, quiet) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Anything new?", "channel": "email" }),
    )
    .await;
    let (_, brief) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Morning brief" }),
    )
    .await;
    assert_eq!(brief["channel_id"], "home");
    assert_eq!(
        post(
            &s,
            "/v1/schedules",
            json!({ "cron": "0 8 * * *", "prompt": "x", "channel": "nope" })
        )
        .await
        .0,
        404
    );

    for sch in [&sweep, &quiet, &brief] {
        let (_, r) = post(
            &s,
            &format!("/v1/schedules/{}/run", sch["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
        wait_run(&s, r["run_id"].as_str().unwrap()).await;
    }
    let posts = wait_get(&s, "/v1/channels/email/posts", |v| {
        v.as_array().is_some_and(|a| !a.is_empty())
    })
    .await;
    assert_eq!(
        texts(&posts),
        ["2 invoices need a decision."],
        "NO_REPLY posts nothing"
    );
    assert_eq!(posts[0]["author_kind"], "routine");
    assert_eq!(
        posts[0]["author_id"],
        format!("routine:{}", sweep["id"].as_str().unwrap())
    );
    assert_eq!(posts[0]["author"], "Sweep the inbox");
    let home_posts = wait_get(&s, "/v1/channels/home/posts", |v| {
        v.as_array().is_some_and(|a| !a.is_empty())
    })
    .await;
    assert_eq!(texts(&home_posts), ["Two meetings today."]);

    // Moving a routine to another channel.
    let (_, moved) = patch(
        &s,
        &format!("/v1/schedules/{}", brief["id"].as_str().unwrap()),
        json!({ "channel": "email" }),
    )
    .await;
    assert_eq!(moved["channel_id"], email_id);

    // A restart doesn't post anything twice.
    drop(_d);
    let _d = env.daemon_with(None);
    let n: i64 = env.query("SELECT count(*) FROM posts");
    assert_eq!(n, 2);
}
