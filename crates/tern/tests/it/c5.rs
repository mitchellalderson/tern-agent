//! Channels C5 end-to-end (docs/channels-design.md §6): `channel_suggest`
//! cards accepted (the channel created, the conversation and routines
//! moved there) and declined.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use subagents_env::{Env, get, post};

async fn say(s: &std::path::Path, channel: &str, text: &str, new_topic: bool) -> Value {
    let (code, sent) = post(
        s,
        &format!("/v1/channels/{channel}/posts"),
        json!({ "text": text, "new_topic": new_topic }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    get(
        s,
        &format!("/v1/runs/{}/wait", sent["run_id"].as_str().unwrap()),
    )
    .await;
    sent
}

/// Home's open suggestion cards.
async fn cards(s: &std::path::Path) -> Vec<Value> {
    let (_, posts) = get(s, "/v1/channels/home/posts").await;
    posts
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p.get("suggestion").is_some())
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_suggested_channel_is_created_only_when_accepted() {
    let env = Env::new(&json!({ "conversations": [
        // A new topic's first message carries the last one's: first.
        { "matches": "Plan the trip", "turns": [
            { "text": "", "tool_calls": [
                { "name": "channel_suggest", "input": { "slug": "travel", "why": "Trips." } },
                { "name": "channel_suggest", "input": { "slug": "home", "why": "Taken." } }
            ] },
            { "text": "Suggested #travel." }
        ]},
        { "matches": "Another invoice", "turns": [
            { "text": "", "tool_calls": [{ "name": "channel_suggest", "input": {
                "slug": "finances", "title": "Finances",
                "why": "Invoices keep coming up.", "routines": ["Sweep invoices"] } }] },
            { "text": "Noted. I suggested a #finances channel." }
        ]},
        { "matches": "Sweep invoices", "turns": [{ "text": "Nothing new." }] }
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, routine) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Sweep invoices every morning" }),
    )
    .await;
    let routine = routine["id"].as_str().unwrap().to_owned();

    let sent = say(&s, "home", "Another invoice from Acme", false).await;
    let session = sent["session_id"].as_str().unwrap().to_owned();
    // Nothing created yet: a card in the conversation it came from (Home).
    let (_, channels) = get(&s, "/v1/channels").await;
    assert_eq!(channels.as_array().unwrap().len(), 1, "{channels:#}");
    let card = cards(&s).await.remove(0);
    assert_eq!(card["kind"], "needs_you");
    assert_eq!(
        card["text"],
        "Suggests a new channel, #finances: Invoices keep coming up."
    );
    let suggestion = &card["suggestion"];
    assert_eq!(
        (suggestion["slug"].as_str(), suggestion["state"].as_str()),
        (Some("finances"), Some("pending"))
    );
    assert_eq!(suggestion["routines"][0]["id"], routine.as_str());
    assert_eq!(suggestion["session_id"], session.as_str());
    let id = suggestion["id"].as_i64().unwrap();

    // Accepting creates it and moves the conversation and the routine.
    let (code, accepted) = post(&s, &format!("/v1/suggestions/{id}/accept"), json!({})).await;
    assert_eq!(code, 200, "{accepted}");
    assert_eq!(accepted["channel"]["slug"], "finances");
    assert_eq!(accepted["moved"][0]["session_id"], session.as_str());
    assert_eq!(accepted["routines"], json!([routine]));
    let channel = accepted["channel"]["id"].as_str().unwrap().to_owned();
    let (_, list) = get(&s, "/v1/schedules").await;
    let sch = list
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == routine.as_str())
        .unwrap();
    assert_eq!(sch["channel_id"], channel.as_str(), "{sch:#}");
    // The card closed, saying what happened; it can't be decided again.
    let (_, posts) = get(&s, "/v1/channels/home/posts").await;
    let closed = posts
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == card["id"])
        .unwrap()
        .clone();
    assert!(closed["resolved_at"].is_i64(), "{closed:#}");
    assert!(
        closed["text"].as_str().unwrap().starts_with(
            "Created #finances, and moved this conversation there; 1 routine now posts there"
        ),
        "{closed:#}"
    );
    assert_eq!(closed["suggestion"]["state"], "accepted");
    let (code, _) = post(&s, &format!("/v1/suggestions/{id}/decline"), json!({})).await;
    assert_eq!(code, 409);

    // A slug that exists is refused to the agent; declining creates nothing.
    say(&s, "home", "Plan the trip to Lisbon", true).await;
    let steps: String = env.query(
        "SELECT group_concat(output_json, '|') FROM steps WHERE kind = 'tool' AND output_json LIKE '%already exists%'",
    );
    assert!(steps.contains("#home already exists"), "{steps}");
    let card = cards(&s)
        .await
        .into_iter()
        .find(|p| p["suggestion"]["slug"] == "travel")
        .unwrap();
    // Dismissing the card is declining it.
    let (code, _) = post(&s, &format!("/v1/posts/{}/resolve", card["id"]), json!({})).await;
    assert_eq!(code, 200);
    let (_, posts) = get(&s, "/v1/channels/home/posts").await;
    let closed = posts
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == card["id"])
        .unwrap()
        .clone();
    assert!(
        closed["text"]
            .as_str()
            .unwrap()
            .starts_with("Declined #travel"),
        "{closed:#}"
    );
    let (_, channels) = get(&s, "/v1/channels").await;
    assert!(
        !channels
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["slug"] == "travel")
    );
}

/// The fake Bot API (`tests/fixtures/fake_telegram.py`), as in `ap1.rs`.
struct FakeTelegram {
    _proc: subagents_env::Killed,
    url: String,
    http: reqwest::Client,
}

const CHAT: i64 = 7070;
const GROUP: i64 = -100_300;
const TOPICS_CHAT: i64 = 8080;

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

    async fn says(&self, body: Value) {
        self.http
            .post(format!("{}/control/message", self.url))
            .json(&body)
            .send()
            .await
            .unwrap();
    }

    async fn messages(&self) -> Vec<Value> {
        self.http
            .get(format!("{}/control/messages", self.url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// The first bot message containing `needle`.
    async fn wait_for(&self, needle: &str) -> Value {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            let all = self.messages().await;
            if let Some(m) = all
                .iter()
                .find(|m| m["text"].as_str().unwrap_or("").contains(needle))
            {
                return m.clone();
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
async fn telegram_maps_channels_to_topics_or_prefixes() {
    let tg = FakeTelegram::start();
    let env = Env::with_config(
        &json!({ "conversations": [
            // A woken agent's message comes with the channel's digest, which
            // quotes earlier posts: the later script first.
            { "matches": "Northwind terms", "turns": [{ "text": "Net 30." }] },
            // tern has one conversation in #finances, whichever chat or
            // topic writes to it: these are its turns.
            { "matches": "How much on cards", "turns": [
                { "text": "About 1,240 EUR." },
                { "text": "Forum answer." },
                { "text": "Private topic answer." }
            ]},
            { "matches": "Hello Home", "turns": [{ "text": "Home here." }] }
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
    let s = env.socket();
    post(&s, "/v1/agents", json!({ "name": "Research" })).await;
    post(&s, "/v1/channels", json!({ "slug": "finances" })).await;
    let out = env.cmd(&["gateway", "pair"]).output().unwrap();
    let code = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    tg.says(json!({ "chat_id": CHAT, "user_id": 1, "text": code }))
        .await;
    tg.wait_for("Paired.").await;

    // A private chat without topics follows the channel: #slug prefixes.
    tg.says(json!({ "chat_id": CHAT, "user_id": 1, "text": "/channels" }))
        .await;
    tg.wait_for("· #finances (/channel finances)").await;
    tg.says(json!({ "chat_id": CHAT, "user_id": 1, "text": "/channel finances" }))
        .await;
    tg.wait_for("You're posting in #finances now").await;
    tg.says(json!({ "chat_id": CHAT, "user_id": 1, "text": "How much on cards?" }))
        .await;
    let m = tg.wait_for("#finances · About 1,240 EUR.").await;
    assert_eq!(m["chat_id"], CHAT);
    // It went through the channel: a post and the lead's answer there.
    let (_, posts) = get(&s, "/v1/channels/finances/posts").await;
    let texts: Vec<_> = posts
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["text"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        texts.contains(&"How much on cards?".to_owned()),
        "{texts:?}"
    );
    assert!(texts.contains(&"About 1,240 EUR.".to_owned()));
    // Said elsewhere (the app), it reaches the chat, by name; the user's
    // own post and the answer delivered above aren't repeated.
    post(
        &s,
        "/v1/channels/finances/posts",
        json!({ "text": "@research Northwind terms?" }),
    )
    .await;
    tg.wait_for("#finances · Research · Net 30.").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let all = tg.messages().await;
    let count = |needle: &str| {
        all.iter()
            .filter(|m| m["text"].as_str().unwrap_or("").contains(needle))
            .count()
    };
    assert_eq!(count("About 1,240 EUR."), 1, "{all:#?}");
    assert_eq!(count("Northwind terms?"), 0);
    tg.says(json!({ "chat_id": CHAT, "user_id": 1, "text": "/new" }))
        .await;
    tg.wait_for("A channel is one conversation").await;
    tg.says(json!({ "chat_id": CHAT, "user_id": 1, "text": "/channel home" }))
        .await;
    tg.wait_for("Back to tern.").await;
    tg.says(json!({ "chat_id": CHAT, "user_id": 1, "text": "Hello Home" }))
        .await;
    let m = tg.wait_for("Home here.").await;
    assert!(!m["text"].as_str().unwrap().contains('#'), "{m}");

    // A forum group: one topic per channel, made once, and written in.
    tg.says(json!({ "chat_id": GROUP, "user_id": 1, "text": "/channel finances", "forum": true }))
        .await;
    let m = tg
        .wait_for("This topic is #finances. Everything said there")
        .await;
    let topic = m["thread"].as_i64().unwrap();
    tg.says(json!({
        "chat_id": GROUP, "user_id": 1, "text": "From the forum", "forum": true, "thread": topic
    }))
    .await;
    let m = tg.wait_for("Forum answer.").await;
    assert_eq!(
        (m["chat_id"].as_i64(), m["thread"].as_i64()),
        (Some(GROUP), Some(topic))
    );
    assert_eq!(m["text"], "Forum answer.", "in its own topic, no prefix");
    tg.says(json!({ "chat_id": GROUP, "user_id": 1, "text": "/channel finances", "forum": true }))
        .await;
    let m = tg.wait_for("This topic is #finances: write there.").await;
    assert_eq!(
        m["thread"].as_i64(),
        Some(topic),
        "the same topic, not a new one"
    );

    // A private chat with topics (Bot API 9.3; created since 9.4) gets one too.
    tg.says(json!({ "chat_id": TOPICS_CHAT, "user_id": 1, "text": "/channel finances", "topics": true }))
        .await;
    let end = Instant::now() + Duration::from_secs(15);
    let m = loop {
        let all = tg.messages().await;
        if let Some(m) = all.iter().find(|m| {
            m["chat_id"] == TOPICS_CHAT
                && m["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("This topic is #finances")
        }) {
            break m.clone();
        }
        assert!(Instant::now() < end, "{all:#?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let ptopic = m["thread"].as_i64().unwrap();
    tg.says(json!({
        "chat_id": TOPICS_CHAT, "user_id": 1, "text": "From a private topic", "topics": true, "thread": ptopic
    }))
    .await;
    // Delivered in that topic; the forum's #finances topic gets it as a post.
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let all = tg.messages().await;
        let here = all.iter().any(|m| {
            m["chat_id"] == TOPICS_CHAT
                && m["thread"].as_i64() == Some(ptopic)
                && m["text"] == "Private topic answer."
        });
        let there = all.iter().any(|m| {
            m["chat_id"] == GROUP
                && m["thread"].as_i64() == Some(topic)
                && m["text"] == "Private topic answer."
        });
        if here && there {
            break;
        }
        assert!(Instant::now() < end, "{all:#?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
