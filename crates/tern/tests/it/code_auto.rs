//! Code auto mode's per-agent opt-in through the API: never silent, never
//! from a starter, never past a ceiling, and a run freezes the lifted level.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use std::path::Path;

use serde_json::{Value, json};
use subagents_env::{Env, api, get, post};

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

#[tokio::test(flavor = "multi_thread")]
async fn opting_in_is_explicit_per_agent_and_visible_in_the_ledger() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let _d = env.daemon_with(None);
    let s = env.socket();

    // A starter keeps the conservative ceiling and is not opted in; create
    // cannot set the flag at all.
    let (code, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Fixer", "template": "bug-fixer" }),
    )
    .await;
    assert_eq!(code, 201, "{a}");
    assert_eq!(a["autonomy"], "supervised");
    assert!(a.get("code_auto").is_none(), "{a}");
    let (code, _) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Sneaky", "code_auto": true }),
    )
    .await;
    assert_ne!(code, 201, "create must not accept code_auto");
    let slug = a["slug"].as_str().unwrap();

    // The flag alone, under a ceiling below code: refused with a reason.
    let (code, body) = patch(
        &s,
        &format!("/v1/agents/{slug}"),
        json!({ "code_auto": true }),
    )
    .await;
    assert_eq!(code, 409, "{body}");
    assert!(body.to_string().contains("ceiling_below_code"), "{body}");

    // Opting in raises the ceiling in the same, visible request.
    let (code, a) = patch(
        &s,
        &format!("/v1/agents/{slug}"),
        json!({ "code_auto": true, "autonomy": "code" }),
    )
    .await;
    assert_eq!(code, 200, "{a}");
    assert_eq!(a["code_auto"], true);
    assert_eq!(a["autonomy"], "code");
    let n: i64 = env.query("SELECT COUNT(*) FROM ledger WHERE kind = 'code_auto.agent'");
    assert_eq!(n, 1);
    let n: i64 = env.query("SELECT COUNT(*) FROM ledger WHERE kind = 'autonomy.agent'");
    assert_eq!(n, 1, "the ceiling change is its own entry");

    // Lowering the ceiling below code turns the mode off again, on the record.
    let (code, a) = patch(
        &s,
        &format!("/v1/agents/{slug}"),
        json!({ "autonomy": "supervised" }),
    )
    .await;
    assert_eq!(code, 200, "{a}");
    assert!(a.get("code_auto").is_none(), "{a}");
    let n: i64 = env.query("SELECT COUNT(*) FROM ledger WHERE kind = 'code_auto.agent'");
    assert_eq!(n, 2);

    // Opting out is a plain request, and a no-op when already off.
    let (code, a) = patch(
        &s,
        &format!("/v1/agents/{slug}"),
        json!({ "code_auto": false }),
    )
    .await;
    assert_eq!(code, 200, "{a}");
    let n: i64 = env.query("SELECT COUNT(*) FROM ledger WHERE kind = 'code_auto.agent'");
    assert_eq!(n, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_starter_cannot_opt_in() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (code, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Researcher", "template": "research" }),
    )
    .await;
    assert_eq!(code, 201, "{a}");
    // Pick whichever read-only starter exists in the catalog.
    let (_, tpls) = get(&s, "/v1/agents/templates").await;
    let ro = tpls
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["autonomy"] == "readonly")
        .expect("a read-only starter")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (code, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Looker", "template": ro }),
    )
    .await;
    assert_eq!(code, 201, "{a}");
    assert_eq!(a["autonomy"], "readonly");
    let slug = a["slug"].as_str().unwrap();
    for body in [
        json!({ "code_auto": true }),
        json!({ "code_auto": true, "autonomy": "code" }),
    ] {
        let (code, b) = patch(&s, &format!("/v1/agents/{slug}"), body).await;
        assert_eq!(code, 409, "{b}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conversation_with_an_opted_in_agent_freezes_the_code_level() {
    let env = Env::with_config(&json!({ "turns": [{ "text": "ok" }] }), "");
    let _d = env.daemon_with(None);
    let s = env.socket();

    // Not opted in: the configured level.
    let r = subagents_env::start(&s, "hello").await;
    assert!(r["run_id"].is_string(), "{r}");
    let level: String = env.query("SELECT autonomy FROM runs ORDER BY started_at DESC LIMIT 1");
    assert_eq!(level, "supervised");

    // Opted in (the default agent has no ceiling): the next run is at code.
    let (code, a) = patch(&s, "/v1/agents/tern", json!({ "code_auto": true })).await;
    assert_eq!(code, 200, "{a}");
    let r = subagents_env::start(&s, "again").await;
    assert!(r["run_id"].is_string(), "{r}");
    let level: String = env.query("SELECT autonomy FROM runs ORDER BY started_at DESC LIMIT 1");
    assert_eq!(level, "code");
}
