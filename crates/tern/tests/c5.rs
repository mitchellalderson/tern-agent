//! Channels C5 end-to-end (docs/channels-design.md §6): `channel_suggest`
//! cards accepted (the channel created, the conversation and routines
//! moved there) and declined.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

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
