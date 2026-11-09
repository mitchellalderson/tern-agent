//! `GET /v1/sessions/{id}` pages by message index: `limit` keeps the
//! newest that many, `before` pages back, and a bare request is unchanged.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use serde_json::{Value, json};
use subagents_env::{Env, get, post};

#[tokio::test(flavor = "multi_thread")]
async fn a_session_pages_back_from_its_newest_messages() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let _d = env.daemon_with(None);
    let s = env.socket();

    let (_, a) = post(&s, "/v1/agents", json!({ "name": "Research" })).await;
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();
    let mut session = String::new();
    for n in 0..4 {
        let (code, sent) = post(
            &s,
            &format!("/v1/channels/{dm}/posts"),
            json!({ "text": format!("message {n}") }),
        )
        .await;
        assert_eq!(code, 200, "{sent}");
        let run = sent["run_id"].as_str().unwrap().to_owned();
        get(&s, &format!("/v1/runs/{run}/wait")).await;
        session = sent["session_id"].as_str().unwrap().to_owned();
    }

    // Unchanged without parameters: everything, from the start.
    let (code, all) = get(&s, &format!("/v1/sessions/{session}")).await;
    assert_eq!(code, 200, "{all}");
    let every = all["messages"].as_array().unwrap().clone();
    let total = every.len();
    assert!(total >= 8, "four turns, a question and an answer each");
    assert_eq!(all["total"], total);
    assert_eq!(all["offset"], 0);

    // The newest three.
    let (_, page) = get(&s, &format!("/v1/sessions/{session}?limit=3")).await;
    assert_eq!(page["total"], total);
    assert_eq!(page["offset"], total - 3);
    assert_eq!(page["messages"].as_array().unwrap(), &every[total - 3..]);

    // Back from there: the three before them, then what is left.
    let (_, older) = get(
        &s,
        &format!("/v1/sessions/{session}?limit=3&before={}", total - 3),
    )
    .await;
    assert_eq!(older["offset"], total - 6);
    assert_eq!(
        older["messages"].as_array().unwrap(),
        &every[total - 6..total - 3]
    );
    let (_, rest) = get(
        &s,
        &format!("/v1/sessions/{session}?limit=100&before={}", total - 6),
    )
    .await;
    assert_eq!(rest["offset"], 0);
    assert_eq!(rest["messages"].as_array().unwrap(), &every[..total - 6]);

    // Past either end is empty or clamped, never an error.
    let (code, none) = get(&s, &format!("/v1/sessions/{session}?limit=5&before=0")).await;
    assert_eq!(code, 200);
    assert_eq!(none["messages"], Value::Array(vec![]));
    let (code, tail) = get(&s, &format!("/v1/sessions/{session}?limit=2&before=99999")).await;
    assert_eq!(code, 200);
    assert_eq!(tail["messages"].as_array().unwrap(), &every[total - 2..]);
}
