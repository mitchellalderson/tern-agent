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
            // First: a side thread's first message carries the channel's digest.
            { "matches": "Why so much?", "turns": [{ "text": "Mostly card fees." }] },
            { "matches": "How much did we spend", "turns": [
                { "text": "About 1,240 EUR." },
                { "text": "Northwind is 310 EUR." }
            ]}
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

// ---- C1: Home's stream ----

fn stream_types(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| match i["type"].as_str().unwrap() {
            "message" => format!(
                "{}:{}",
                i["role"].as_str().unwrap(),
                i["content"][0]["text"].as_str().unwrap_or("…")
            ),
            "cut" => format!("cut:{}", i["reason"].as_str().unwrap_or("")),
            "post" => format!("post:{}", i["post"]["text"].as_str().unwrap()),
            other => other.to_owned(),
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn new_topic_cuts_home_and_carries_the_last_segment_over() {
    let env = Env::with_config(
        &json!({ "conversations": [
            // First: the new segment's message also quotes the old one.
            { "matches": "Plan the launch", "turns": [
                { "text": "", "tool_calls": [{ "name": "history_search", "input": { "query": "Northwind" } }] },
                { "text": "Launch plan drafted." }
            ]},
            { "matches": "Northwind is 310", "turns": [{ "text": "Noted: Northwind 310 EUR." }] },
            { "matches": "Morning brief", "turns": [{ "text": "Two meetings today." }] }
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();

    let (_, first) = post(
        &s,
        "/v1/channels/home/posts",
        json!({ "text": "Northwind is 310 EUR" }),
    )
    .await;
    assert_eq!(first["new_segment"], false);
    wait_run(&s, first["run_id"].as_str().unwrap()).await;
    let (_, brief) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Morning brief" }),
    )
    .await;
    let (_, r) = post(
        &s,
        &format!("/v1/schedules/{}/run", brief["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    wait_run(&s, r["run_id"].as_str().unwrap()).await;

    // The stream orders by millisecond; keep the brief and the cut apart.
    tokio::time::sleep(Duration::from_millis(5)).await;
    let (_, next) = post(
        &s,
        "/v1/channels/home/posts",
        json!({ "text": "Plan the launch", "new_topic": true }),
    )
    .await;
    assert_eq!(next["new_segment"], true);
    assert_ne!(next["session_id"], first["session_id"]);
    let done = wait_run(&s, next["run_id"].as_str().unwrap()).await;
    assert_eq!(done["final_text"], "Launch plan drafted.");

    // The new segment's first run carried the old one over, and found the
    // detail with history_search.
    let kinds: String = env.query(&format!(
        "SELECT group_concat(kind, ',') FROM (SELECT kind FROM steps WHERE run_id = '{}' ORDER BY idx)",
        next["run_id"].as_str().unwrap()
    ));
    // The routine's brief came in after the last segment looked (C3).
    assert_eq!(kinds, "carry,channel,llm,tool,llm");
    let found: String = env.query(&format!(
        "SELECT output_json FROM steps WHERE run_id = '{}' AND kind = 'tool'",
        next["run_id"].as_str().unwrap()
    ));
    assert!(
        found.contains(first["session_id"].as_str().unwrap()) && found.contains("Northwind"),
        "{found}"
    );
    let (_, home) = get(&s, "/v1/channels").await;
    assert_eq!(home[0]["main_session_id"], next["session_id"]);

    let (_, page) = get(&s, "/v1/channels/home/stream").await;
    assert_eq!(
        stream_types(&page)[..4],
        [
            "assistant:Launch plan drafted.".to_string(),
            "user:…".into(),
            "assistant:…".into(),
            "user:Plan the launch".into()
        ]
    );
    let types = stream_types(&page);
    let cut = types.iter().position(|t| t == "cut:new_topic").unwrap();
    let brief_at = types
        .iter()
        .position(|t| t == "post:Two meetings today.")
        .unwrap();
    let hello = types
        .iter()
        .position(|t| t == "user:Northwind is 310 EUR")
        .unwrap();
    assert!(
        cut < brief_at && brief_at < hello,
        "{types:#?}\n{:#}",
        page["items"]
    );
    let carry = page["items"][cut]["carry"].as_str().unwrap();
    assert!(
        carry.contains("Summary: mock summary") && carry.contains("you: Northwind is 310 EUR"),
        "{carry}"
    );
    assert_eq!(page["next"], Value::Null);

    // Paging: two at a time, then the rest.
    let (_, p1) = get(&s, "/v1/channels/home/stream?limit=2").await;
    assert_eq!(p1["items"].as_array().unwrap().len(), 2);
    let cursor = p1["next"].as_str().unwrap();
    let (_, p2) = get(&s, &format!("/v1/channels/home/stream?before={cursor}")).await;
    assert_eq!(stream_types(&p2), types[2..]);
    assert_eq!(get(&s, "/v1/channels/home/stream?before=junk").await.0, 400);

    // A side chat is a card in the stream, not part of it.
    let (_, side) = post(
        &s,
        "/v1/runs",
        json!({ "text": "> Northwind\nWhy 310?", "channel": "home", "branched_from": "message:1" }),
    )
    .await;
    wait_run(&s, side["run_id"].as_str().unwrap()).await;
    assert_eq!(
        post(
            &s,
            "/v1/runs",
            json!({ "text": "x", "channel": "home", "branched_from": "nope" })
        )
        .await
        .0,
        400
    );
    let (_, page) = get(&s, "/v1/channels/home/stream").await;
    assert_eq!(page["items"][0]["type"], "side_chat");
    assert_eq!(page["items"][0]["session_id"], side["session_id"]);
    assert_eq!(page["items"][0]["branched_from"], "message:1");
    let (_, home) = get(&s, "/v1/channels").await;
    assert_eq!(
        home[0]["main_session_id"], next["session_id"],
        "a side chat doesn't move the stream"
    );

    // History lists both segments.
    let (_, history) = get(&s, "/v1/sessions?channel=home").await;
    let ids: Vec<&str> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&first["session_id"].as_str().unwrap())
            && ids.contains(&next["session_id"].as_str().unwrap())
    );
    let seg = history
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"] == next["session_id"])
        .unwrap();
    assert_eq!(seg["prev_session_id"], first["session_id"]);
    assert_eq!(seg["segment_reason"], "new_topic");
    assert!(
        history
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["branched_from"] == "message:1")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn home_is_cut_after_idle_hours() {
    let env = Env::with_config(
        &json!({ "turns": [{ "text": "ok" }] }),
        "[home]\nidle_hours = 0.0002\n",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, a) = post(&s, "/v1/channels/home/posts", json!({ "text": "one" })).await;
    wait_run(&s, a["run_id"].as_str().unwrap()).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let (_, b) = post(&s, "/v1/channels/home/posts", json!({ "text": "two" })).await;
    assert_eq!(b["new_segment"], true, "{b}");
    wait_run(&s, b["run_id"].as_str().unwrap()).await;
    let reason: String = env.query(&format!(
        "SELECT segment_reason FROM sessions WHERE id = '{}'",
        b["session_id"].as_str().unwrap()
    ));
    assert_eq!(reason, "idle");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_while_home_is_busy_is_queued_for_the_run_not_a_cancel() {
    let env = Env::with_config(
        &json!({ "turns": [
            { "text": "Counting.", "delay_ms": 1500 },
            { "text": "Counted, in EUR." }
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, a) = post(
        &s,
        "/v1/channels/home/posts",
        json!({ "text": "Tally the invoices" }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_, q) = post(
        &s,
        "/v1/channels/home/posts",
        json!({ "text": "in EUR please" }),
    )
    .await;
    assert_eq!(q["queued"], true, "{q}");
    assert_eq!(q["run_id"], a["run_id"]);
    let done = wait_run(&s, a["run_id"].as_str().unwrap()).await;
    assert_eq!(
        (done["status"].as_str(), done["final_text"].as_str()),
        (Some("completed"), Some("Counted, in EUR."))
    );
    let runs: i64 = env.query("SELECT count(*) FROM runs");
    assert_eq!(runs, 1, "no second run, nothing cancelled");
    let (_, page) = get(&s, "/v1/channels/home/stream").await;
    let queued = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["queued"] == true)
        .unwrap();
    assert_eq!(queued["content"][0]["text"], "in EUR please");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_topic_channel_send_while_busy_posts_at_once_and_queues() {
    let env = Env::with_config(
        &json!({ "turns": [
            { "text": "Looking.", "delay_ms": 1500 },
            { "text": "Found both." }
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    post(&s, "/v1/channels", json!({ "slug": "launch" })).await;
    let (_, a) = post(
        &s,
        "/v1/channels/launch/posts",
        json!({ "text": "Find the venue" }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_, q) = post(
        &s,
        "/v1/channels/launch/posts",
        json!({ "text": "and the caterer" }),
    )
    .await;
    assert_eq!(
        (q["queued"].as_bool(), q["post"]["text"].as_str()),
        (Some(true), Some("and the caterer"))
    );
    wait_run(&s, a["run_id"].as_str().unwrap()).await;
    let posts = wait_get(&s, "/v1/channels/launch/posts", |v| {
        v.as_array().is_some_and(|a| a.len() == 3)
    })
    .await;
    assert_eq!(
        texts(&posts),
        ["Found both.", "and the caterer", "Find the venue"]
    );
}

/// The fake Bot API (`tests/fixtures/fake_telegram.py`).
struct FakeTelegram {
    _proc: subagents_env::Killed,
    url: String,
    http: reqwest::Client,
}

const CHAT: i64 = 7070;

impl FakeTelegram {
    fn start() -> Self {
        use std::io::BufRead;
        let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake_telegram.py");
        let mut child = std::process::Command::new("python3")
            .arg(script)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            _proc: subagents_env::Killed(child),
            url: format!("http://127.0.0.1:{}", line.trim()),
            http: reqwest::Client::new(),
        }
    }

    async fn user_says(&self, text: &str) {
        self.http
            .post(format!("{}/control/message", self.url))
            .json(&json!({ "chat_id": CHAT, "user_id": 1, "text": text }))
            .send()
            .await
            .unwrap();
    }

    async fn wait_for(&self, needle: &str) {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            let all: Vec<Value> = self
                .http
                .get(format!("{}/control/messages", self.url))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if all
                .iter()
                .any(|m| m["text"].as_str().unwrap_or("").contains(needle))
            {
                return;
            }
            assert!(
                Instant::now() < end,
                "no bot message with {needle:?}: {all:#?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn telegram_new_is_new_topic_and_carries_the_chat_over() {
    let tg = FakeTelegram::start();
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "And the second?", "turns": [{ "text": "Second: book the caterer." }] },
            { "matches": "First task", "turns": [{ "text": "First: find a venue." }] }
        ]}),
        &format!(
            "[gateway.telegram]\nenabled = true\nbot_token_secret = \"TELEGRAM_BOT_TOKEN\"\napi_url = \"{}\"\n",
            tg.url
        ),
    );
    let secrets = env.tern_home.join("secrets.toml");
    std::fs::write(&secrets, "TELEGRAM_BOT_TOKEN = \"TEST:TOKEN\"\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let _d = env.daemon_with(None);
    let out = env.cmd(&["gateway", "pair"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    tg.user_says(String::from_utf8_lossy(&out.stdout).trim())
        .await;
    tg.wait_for("Paired.").await;

    tg.user_says("First task").await;
    tg.wait_for("First: find a venue.").await;
    tg.user_says("/new").await;
    tg.wait_for("New topic.").await;
    tg.user_says("And the second?").await;
    tg.wait_for("Second: book the caterer.").await;

    let db = env.db();
    let (prev, reason): (String, String) = db
        .query_row(
            "SELECT prev_session_id, segment_reason FROM sessions WHERE source = 'telegram' AND prev_session_id IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(reason, "new_topic");
    let first: String = db
        .query_row(
            "SELECT session_id FROM runs ORDER BY started_at LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(prev, first);
    let carried: String = db
        .query_row(
            "SELECT group_concat(kind) FROM steps s JOIN runs r ON r.id = s.run_id WHERE r.session_id <> ?1",
            [&first],
            |r| r.get(0),
        )
        .unwrap();
    assert!(carried.starts_with("carry"), "{carried}");
}

// ---- C3: agents see each other's posts; taint flows through them ----

#[tokio::test(flavor = "multi_thread")]
async fn a_tainted_routine_post_reaches_the_channel_digest_and_taints_what_follows() {
    let env = Env::with_config(
        &json!({ "conversations": [
            // First: the main thread's message also carries the digest.
            { "matches": "What needs a decision?", "turns": [
                { "text": "", "tool_calls": [{ "name": "channel_post", "input": { "text": "Checking the invoices", "needs_you": false } }] },
                { "text": "", "tool_calls": [{ "name": "channel_read", "input": { "query": "invoice" } }] },
                { "text": "Invoice 12 is waiting on you." }
            ]},
            { "matches": "Sweep the inbox", "turns": [
                { "text": "Looking.", "tool_calls": [{ "name": "mcp_list", "input": {} }] },
                { "text": "Pay invoice 12 now." }
            ]}
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    post(&s, "/v1/channels", json!({ "slug": "finances" })).await;
    let (_, sweep) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Sweep the inbox", "channel": "finances" }),
    )
    .await;
    let (_, r) = post(
        &s,
        &format!("/v1/schedules/{}/run", sweep["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    wait_run(&s, r["run_id"].as_str().unwrap()).await;

    let (_, sent) = post(
        &s,
        "/v1/channels/finances/posts",
        json!({ "text": "What needs a decision?" }),
    )
    .await;
    let run = sent["run_id"].as_str().unwrap().to_owned();
    let done = wait_run(&s, &run).await;
    assert_eq!(
        done["final_text"], "Invoice 12 is waiting on you.",
        "{done}"
    );

    // The digest showed the routine's post, framed as untrusted, and
    // tainted the run; its own post and answer are tainted in turn.
    let kinds: String = env.query(&format!(
        "SELECT group_concat(kind, ',') FROM (SELECT kind FROM steps WHERE run_id = '{run}' ORDER BY idx)"
    ));
    assert_eq!(kinds, "channel,llm,tool,llm,tool,llm");
    let digest: String = env.query(&format!(
        "SELECT text FROM messages WHERE run_id = '{run}' AND step_idx = 0"
    ));
    assert!(
        digest.contains("[Posts in #finances since you last looked: 1.")
            && digest.contains(
                "<untrusted source=\"post in #finances\">\nPay invoice 12 now.\n</untrusted>"
            ),
        "{digest}"
    );
    let taint: String = env.query(&format!("SELECT taint FROM runs WHERE id = '{run}'"));
    assert_eq!(taint, "a post in #finances from a tainted run");
    let read: String = env.query(&format!(
        "SELECT output_json FROM steps WHERE run_id = '{run}' AND idx = 4"
    ));
    assert!(
        read.contains("\"untrusted\":true") && read.contains("Pay invoice 12"),
        "{read}"
    );

    let posts = wait_get(&s, "/v1/channels/finances/posts", |v| {
        v.as_array().is_some_and(|a| a.len() == 4)
    })
    .await;
    let by = |t: &str| {
        posts
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["text"] == t)
            .unwrap_or_else(|| panic!("{t}: {posts:#}"))
            .clone()
    };
    let update = by("Checking the invoices");
    assert_eq!(
        (update["kind"].as_str(), update["author_id"].as_str()),
        (Some("update"), Some("tern"))
    );
    assert_eq!(update["tainted"], true);
    assert_eq!(by("Invoice 12 is waiting on you.")["tainted"], true);
    assert_eq!(
        by("What needs a decision?").get("tainted"),
        None,
        "the user's own send"
    );

    // The thread's cursor moved past what it saw: nothing new, no step.
    let seen: i64 = env.query(&format!(
        "SELECT posts_seen FROM sessions WHERE id = '{}'",
        sent["session_id"].as_str().unwrap()
    ));
    assert_eq!(seen, by("Pay invoice 12 now.")["id"].as_i64().unwrap());
}
