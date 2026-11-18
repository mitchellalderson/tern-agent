//! Proactive help (docs/innbot-learnings.md P1, P2, P5, P6): every card is
//! dismissible, capped, and costs no model call unless something changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::subagents_env::{Env, Killed, get, post};
use serde_json::{Value, json};

const CONFIG: &str = "[subagents]\nenabled = true\n[schedules]\nquiet_hours = \"none\"\n";

async fn cards(s: &Path) -> Vec<Value> {
    get(s, "/v1/proactive/cards")
        .await
        .1
        .as_array()
        .unwrap()
        .clone()
}

async fn wait_cards(s: &Path, kind: &str, secs: u64) -> Option<Value> {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Some(c) = cards(s).await.into_iter().find(|c| c["kind"] == kind) {
            return Some(c);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

fn runs(env: &Env) -> i64 {
    env.query("SELECT count(*) FROM runs")
}

/// A daemon whose scheduler ticks fast, so the heartbeat shows up quickly.
fn daemon_ticking(env: &Env) -> Killed {
    let _ = std::fs::remove_file(env.socket());
    let child = env
        .cmd(&["daemon"])
        .env("TERN_SCHEDULER_TICK_MS", "200")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while !env.socket().exists() {
        assert!(Instant::now() < end, "daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    Killed(child)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_heartbeat_is_silent_when_all_is_well_and_speaks_once_when_not() {
    let env = Env::with_config(&json!({ "turns": [{ "text": "ok" }] }), CONFIG);
    let _d = daemon_ticking(&env);
    let s = env.socket();
    // A healthy routine; the tick checks and finds nothing.
    let (code, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 3 * * *", "prompt": "Check the build" }),
    )
    .await;
    assert_eq!(code, 201, "{sch}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(cards(&s).await.is_empty());
    assert_eq!(runs(&env), 0, "no model call for a quiet heartbeat");

    // The routine hits a cap and pauses: the next check says so, once.
    let id = sch["id"].as_str().unwrap();
    let far = 9_999_999_999_999_i64;
    env.db()
        .execute(
            "UPDATE schedules SET paused_until = ?2, paused_reason = 'daily cap' WHERE id = ?1",
            rusqlite::params![id, far],
        )
        .unwrap();
    for _ in 0..2 {
        // A new day (the check is once a day per agent).
        env.db()
            .execute(
                "DELETE FROM proactive_state WHERE key LIKE 'heartbeat:%'",
                [],
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
    }
    let all = cards(&s).await;
    assert_eq!(all.len(), 1, "same news twice is one card: {all:?}");
    assert_eq!(all[0]["kind"], "heartbeat");
    assert!(
        all[0]["body"]
            .as_str()
            .unwrap()
            .contains("paused: daily cap")
    );
    assert_eq!(runs(&env), 0, "the heartbeat never asks a model");
    // Dismissing closes it.
    let cid = all[0]["id"].as_i64().unwrap();
    let (code, _) = post(&s, &format!("/v1/proactive/cards/{cid}/dismiss"), json!({})).await;
    assert_eq!(code, 200);
    assert_eq!(
        get(&s, "/v1/proactive/cards?state=pending").await.1,
        json!([])
    );
    let (code, _) = post(&s, &format!("/v1/proactive/cards/{cid}/dismiss"), json!({})).await;
    assert_eq!(code, 409, "decided once");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_away_brief_is_once_a_day_after_eight_hours_and_only_with_news() {
    let env = Env::with_config(&json!({ "turns": [{ "text": "ok" }] }), CONFIG);
    let _d = env.daemon_with(None);
    let s = env.socket();
    let brief = |v: Value| v["brief"].as_bool().unwrap();
    // The first ever open has no "away".
    assert!(!brief(
        post(&s, "/v1/proactive/presence", json!({})).await.1
    ));
    let ago = |h: i64| (chrono::Utc::now().timestamp_millis() - h * 3_600_000).to_string();
    let set_seen = |h: i64| {
        env.db()
            .execute(
                "UPDATE proactive_state SET value = ?1 WHERE key = 'presence:last_seen'",
                [ago(h)],
            )
            .unwrap();
    };
    // Away, but nothing happened: no card.
    set_seen(9);
    assert!(!brief(
        post(&s, "/v1/proactive/presence", json!({})).await.1
    ));
    assert!(cards(&s).await.is_empty());
    // A held digest item and a new day's worth of away time: one brief.
    env.db()
        .execute("DELETE FROM proactive_state WHERE key = 'away:day'", [])
        .unwrap();
    env.db()
        .execute(
            "INSERT INTO held_deliveries (target, label, text, release_at, created_at, kind)
             VALUES ('telegram:1', 'Inbox', '3 new from Ana', 9999999999999, 0, 'digest')",
            [],
        )
        .unwrap();
    set_seen(9);
    assert!(brief(post(&s, "/v1/proactive/presence", json!({})).await.1));
    let all = cards(&s).await;
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0]["title"], "While you were away");
    assert!(all[0]["body"].as_str().unwrap().contains("3 new from Ana"));
    let held: i64 = env.query("SELECT count(*) FROM held_deliveries");
    assert_eq!(held, 0, "the digest is in the brief now");
    // Not again today, however long they were gone.
    set_seen(12);
    assert!(!brief(
        post(&s, "/v1/proactive/presence", json!({})).await.1
    ));
    // Under eight hours never counts.
    env.db()
        .execute("DELETE FROM proactive_state WHERE key = 'away:day'", [])
        .unwrap();
    set_seen(2);
    assert!(!brief(
        post(&s, "/v1/proactive/presence", json!({})).await.1
    ));
    assert_eq!(runs(&env), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn grants_are_listed_and_revocable() {
    let env = Env::with_config(&json!({ "turns": [{ "text": "ok" }] }), CONFIG);
    let _d = env.daemon_with(None);
    let s = env.socket();
    env.db()
        .execute(
            "INSERT INTO approval_allowlist (tool, pattern, created_at) VALUES ('shell', 'git status *', 0)",
            [],
        )
        .unwrap();
    let (_, list) = get(&s, "/v1/grants").await;
    assert_eq!(list[0]["tool"], "shell");
    assert_eq!(list[0]["pattern"], "git status *");
    let (code, _) = post(
        &s,
        "/v1/grants/revoke",
        json!({ "tool": "shell", "pattern": "git status *" }),
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(get(&s, "/v1/grants").await.1, json!([]));
    let (code, _) = post(
        &s,
        "/v1/grants/revoke",
        json!({ "tool": "shell", "pattern": "git status *" }),
    )
    .await;
    assert_eq!(code, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_routine_with_empty_reports_gets_one_fix_proposal_never_applied_silently() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Look at its recent runs", "turns": [
                { "text": "It searches for a label that no longer exists. Change the prompt to use the Inbox label." }
            ]},
            { "matches": "Go ahead and apply the fix", "turns": [{ "text": "Applied." }] },
            { "matches": "Check the inbox", "turns": [
                { "text": "NO_REPLY" }, { "text": "NO_REPLY" }, { "text": "NO_REPLY" }, { "text": "NO_REPLY" }
            ]}
        ]}),
        CONFIG,
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (code, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 3 * * *", "prompt": "Check the inbox", "initiative": "silent" }),
    )
    .await;
    assert_eq!(code, 201, "{sch}");
    let id = sch["id"].as_str().unwrap().to_owned();
    let run_now = |s: std::path::PathBuf, id: String| async move {
        let (code, r) = post(&s, &format!("/v1/schedules/{id}/run"), json!({})).await;
        assert_eq!(code, 200, "{r}");
        get(
            &s,
            &format!("/v1/runs/{}/wait", r["run_id"].as_str().unwrap()),
        )
        .await;
    };
    for _ in 0..2 {
        run_now(s.clone(), id.clone()).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        cards(&s).await.is_empty(),
        "two empty reports is not enough"
    );
    run_now(s.clone(), id.clone()).await;
    let card = wait_cards(&s, "routine_fix", 15).await.expect("a fix card");
    assert_eq!(card["state"], "pending");
    assert_eq!(card["accept_label"], "Apply fix");
    assert!(
        card["body"].as_str().unwrap().contains("Inbox label"),
        "{card}"
    );
    // Proposed, not applied.
    let prompt: String = env.query("SELECT prompt FROM schedules");
    assert_eq!(prompt, "Check the inbox");
    // The investigation could only read.
    let autonomy: String = env.query("SELECT autonomy FROM runs WHERE trigger = 'proactive'");
    assert_eq!(autonomy, "readonly");
    // Another empty report this week: the cap holds.
    run_now(s.clone(), id.clone()).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        cards(&s)
            .await
            .iter()
            .filter(|c| c["kind"] == "routine_fix")
            .count(),
        1
    );
    // Accepting hands the agent the fix as a message from the user.
    let cid = card["id"].as_i64().unwrap();
    let (code, _) = post(&s, &format!("/v1/proactive/cards/{cid}/accept"), json!({})).await;
    assert_eq!(code, 200);
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let n: i64 = env.query(
            "SELECT count(*) FROM messages WHERE role = 'user' AND text LIKE 'Go ahead and apply the fix%'",
        );
        if n > 0 {
            break;
        }
        assert!(Instant::now() < end, "the agent was never told");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        get(&s, "/v1/proactive/cards?state=accepted")
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_ask_three_times_suggests_a_routine_once() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "unread emails", "turns": [
                { "text": "Here you go." }, { "text": "Again." }, { "text": "Again." }, { "text": "Again." }
            ]}
        ]}),
        CONFIG,
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let ask = |text: &'static str| {
        let s = s.clone();
        async move {
            let (code, sent) = post(&s, "/v1/channels/home/posts", json!({ "text": text })).await;
            assert_eq!(code, 200, "{sent}");
            get(
                &s,
                &format!("/v1/runs/{}/wait", sent["run_id"].as_str().unwrap()),
            )
            .await;
        }
    };
    ask("Summarise my unread emails from today").await;
    ask("can you summarise the unread emails I got today").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(cards(&s).await.is_empty(), "twice is a coincidence");
    ask("Please summarise today's unread emails").await;
    let card = wait_cards(&s, "routine_suggestion", 10)
        .await
        .expect("a suggestion");
    assert_eq!(card["title"], "Make this a routine?");
    assert_eq!(card["accept_label"], "Set up routine");
    // A fourth like ask the same day doesn't nag.
    ask("summarise unread emails for today").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(cards(&s).await.len(), 1);
    // Dismissed, it stays dismissed.
    let cid = card["id"].as_i64().unwrap();
    let (code, _) = post(&s, &format!("/v1/proactive/cards/{cid}/dismiss"), json!({})).await;
    assert_eq!(code, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn switches_turn_each_feature_off() {
    let config = format!(
        "{CONFIG}[proactive]\nsuggest_routines = false\naway_brief = false\nheartbeat = false\n"
    );
    let env = Env::with_config(
        &json!({ "conversations": [{ "matches": "unread emails", "turns": [
            { "text": "a" }, { "text": "b" }, { "text": "c" }
        ]}]}),
        &config,
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    for text in [
        "Summarise my unread emails from today",
        "summarise the unread emails I got today",
        "Please summarise today's unread emails",
    ] {
        let (_, sent) = post(&s, "/v1/channels/home/posts", json!({ "text": text })).await;
        get(
            &s,
            &format!("/v1/runs/{}/wait", sent["run_id"].as_str().unwrap()),
        )
        .await;
    }
    post(&s, "/v1/proactive/presence", json!({})).await;
    env.db()
        .execute(
            "UPDATE proactive_state SET value = '0' WHERE key = 'presence:last_seen'",
            [],
        )
        .unwrap();
    env.db()
        .execute(
            "INSERT INTO held_deliveries (target, text, release_at, created_at, kind)
             VALUES ('telegram:1', 'x', 9999999999999, 0, 'digest')",
            [],
        )
        .unwrap();
    let (_, r) = post(&s, "/v1/proactive/presence", json!({})).await;
    assert_eq!(r["brief"], false);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(cards(&s).await.is_empty());
}

const DRAFT: &str = r#"return { name = "net", description = "test", capabilities = { http = { "example.invalid" } }, tools = {
  a = { description = "first", run = function(ctx, args) return "a ok" end } } }"#;

fn test_a(n: u32) -> Value {
    json!({ "name": "connector_test", "input": { "name": "net", "tool": "a", "args": { "n": n } } })
}

async fn approval(events: &mut crate::subagents_env::Events, already: usize) -> Value {
    let pick = |evs: &[Value]| -> Vec<Value> {
        evs.iter()
            .filter_map(|e| e.get("approval_requested").cloned())
            .collect()
    };
    let evs = events.until(30, |evs| pick(evs).len() > already).await;
    pick(&evs)[already].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn always_allow_is_offered_after_three_allow_once_and_stored_on_the_fourth() {
    let env = Env::with_config(
        &json!({ "turns": [
            { "tool_calls": [{ "name": "connector_write", "input": { "name": "net", "source": DRAFT } }] },
            { "tool_calls": [test_a(1), test_a(2), test_a(3), test_a(4), test_a(5)] },
            { "text": "Tested." }
        ]}),
        CONFIG,
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let mut events = crate::subagents_env::Events::open(&s).await;
    post(&s, "/v1/runs", json!({ "text": "test the draft" })).await;
    for n in 0..3 {
        let card = approval(&mut events, n).await;
        assert_eq!(card["grant"]["always"], false, "ask {n}: {card}");
        assert!(card["grant"]["label"].as_str().is_some());
        let url = format!("/v1/approvals/{}", card["id"].as_str().unwrap());
        post(&s, &url, json!({ "decision": "once" })).await;
    }
    // The fourth ask offers it, says why, and an "always" answer is stored.
    let card = approval(&mut events, 3).await;
    assert!(card["grant"].get("always").is_none(), "offered: {card}");
    assert!(
        card["notice"].as_str().unwrap().contains("3 times"),
        "{card}"
    );
    let url = format!("/v1/approvals/{}", card["id"].as_str().unwrap());
    post(&s, &url, json!({ "decision": "always" })).await;
    let evs = events
        .until(30, |evs| evs.iter().any(|e| e["type"] == "run_finished"))
        .await;
    let asked = evs
        .iter()
        .filter(|e| e.get("approval_requested").is_some())
        .count();
    assert_eq!(asked, 4, "the fifth is covered by the grant");
    let stored: i64 = env.query("SELECT count(*) FROM approval_allowlist");
    assert_eq!(stored, 1);
    // And it can be listed and revoked from the app's Settings.
    let (_, list) = get(&s, "/v1/grants").await;
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
}
