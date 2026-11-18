//! Proactive help that asks a model, a little (docs/innbot-learnings.md P3,
//! P4, P9, P11): each is behind a switch and a cap, asks a `decide()` yes/no
//! before any writing call, and does nothing when the answer is no. The mock
//! provider answers by the first words of each prompt.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::subagents_env::{Env, Killed, get, post};
use serde_json::{Value, json};

fn config(on: &str) -> String {
    let switches: String = [
        "heartbeat",
        "fix_routines",
        "suggest_routines",
        "away_brief",
        "new_topic",
        "open_loops",
        "learn_facts",
        "tidy_memory",
    ]
    .iter()
    .map(|k| format!("{k} = {}\n", *k == on))
    .collect();
    format!(
        "[subagents]\nenabled = true\n[schedules]\nquiet_hours = \"none\"\n[proactive]\n{switches}"
    )
}

/// A daemon whose scheduler ticks fast; every model request is logged.
fn daemon(env: &Env) -> (Killed, PathBuf) {
    let log = env.socket().with_file_name("requests.jsonl");
    let _ = std::fs::remove_file(env.socket());
    let child = env
        .cmd(&["daemon"])
        .env("TERN_SCHEDULER_TICK_MS", "200")
        .env("TERN_MOCK_REQUEST_LOG", &log)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while !env.socket().exists() {
        assert!(Instant::now() < end, "daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    (Killed(child), log)
}

/// Requests whose first user message starts with `prefix`.
fn asked(log: &Path, prefix: &str) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| {
            v["first_user"]
                .as_str()
                .is_some_and(|f| f.starts_with(prefix))
        })
        .count()
}

async fn say(s: &Path, channel: &str, text: &str) -> Value {
    let (code, sent) = post(
        s,
        &format!("/v1/channels/{channel}/posts"),
        json!({ "text": text }),
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

async fn card_of(s: &Path, kind: &str, secs: u64) -> Option<Value> {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        let all = get(s, "/v1/proactive/cards").await.1;
        if let Some(c) = all.as_array().unwrap().iter().find(|c| c["kind"] == kind) {
            return Some(c.clone());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

fn yes(id: &str, p: f64) -> Value {
    json!({ "matches": format!("id `{id}`"), "turns": [{ "text": json!({ "answers": { id: p } }).to_string() }] })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_durable_fact_is_saved_with_a_card_that_forgets_it() {
    let script = json!({ "conversations": [
        yes("fact", 0.95),
        { "matches": "Known memories:", "turns": [{ "text":
            "[\"Mitch prefers tea to coffee\", \"Mitch works at Inngest\", \"Mitch prefers tea over coffee\"]" }] },
        { "matches": "tea", "turns": [{ "text": "Noted, tea it is." }] }
    ]});
    let env = Env::with_config(&script, &config("learn_facts"));
    let (_d, log) = daemon(&env);
    let s = env.socket();
    say(&s, "home", "I prefer tea to coffee and I work at Inngest").await;
    let card = card_of(&s, "memory_learned", 15).await.expect("a card");
    assert_eq!(card["accept_label"], "Forget");
    let n: i64 =
        env.query("SELECT count(*) FROM memories WHERE source = 'agent' AND tags = 'auto'");
    assert_eq!(n, 2, "the third is a repeat of the first");
    assert_eq!(asked(&log, "Known memories:"), 1);
    // The ledger records them like any other save.
    let saves: i64 = env.query("SELECT count(*) FROM ledger WHERE kind = 'memory.save'");
    assert_eq!(saves, 2);
    // "Forget" undoes it.
    let id = card["id"].as_i64().unwrap();
    let (code, _) = post(&s, &format!("/v1/proactive/cards/{id}/accept"), json!({})).await;
    assert_eq!(code, 200);
    let n: i64 = env.query("SELECT count(*) FROM memories");
    assert_eq!(n, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_durable_fact_means_no_writing_call() {
    let script = json!({ "conversations": [
        yes("fact", 0.1),
        { "matches": "Known memories:", "turns": [{ "text": "[\"should never be saved\"]" }] },
        { "matches": "weather", "turns": [{ "text": "Sunny." }] }
    ]});
    let env = Env::with_config(&script, &config("learn_facts"));
    let (_d, log) = daemon(&env);
    let s = env.socket();
    say(&s, "home", "What is the weather like in Leeds today?").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(asked(&log, "Text:"), 1, "one decide per message");
    assert_eq!(
        asked(&log, "Known memories:"),
        0,
        "the writer is never asked"
    );
    let n: i64 = env.query("SELECT count(*) FROM memories");
    assert_eq!(n, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_promise_in_a_reply_becomes_a_follow_up_that_wakes_the_agent() {
    let due = (chrono::Local::now() + chrono::Duration::days(3))
        .format("%Y-%m-%d")
        .to_string();
    let script = json!({ "conversations": [
        yes("follow_up", 0.9),
        { "matches": "Now: ", "turns": [{ "text": json!({
            "follow_up": true, "note": "Check whether Riley replied about the quote", "when": due
        }).to_string() }] },
        { "matches": "chase the quote", "turns": [
            { "text": "Asked Riley. I'll check back on Friday to see if there's a reply." },
            { "text": "Still nothing." }
        ] }
    ]});
    let env = Env::with_config(&script, &config("open_loops"));
    let (_d, log) = daemon(&env);
    let s = env.socket();
    say(&s, "home", "Please chase the quote from Riley").await;
    let card = card_of(&s, "follow_up", 15).await.expect("a card");
    assert_eq!(card["accept_label"], "Cancel");
    let waits = get(&s, "/v1/waits").await.1;
    let w = &waits.as_array().unwrap()[0];
    assert_eq!(w["followup"], true);
    assert_eq!(w["check"], "time");
    let day = 24 * 3_600_000_i64;
    assert!(
        w["expires_at"].as_i64().unwrap() > chrono::Utc::now().timestamp_millis() + day,
        "a time follow-up may be days out: {w}"
    );
    assert_eq!(asked(&log, "Now: "), 1);

    // Due: the daemon continues the conversation by itself.
    env.db()
        .execute("UPDATE waits SET next_check_at = 0, expires_at = 0", [])
        .unwrap();
    env.wait_for("the follow-up to fire", 15, |e| {
        e.query::<i64>("SELECT count(*) FROM runs WHERE trigger = 'wait'") == 1
    });
    env.wait_for("the wait to go", 5, |e| {
        e.query::<i64>("SELECT count(*) FROM waits") == 0
    });
    let state: String = env.query(&format!(
        "SELECT state FROM proactive_cards WHERE id = {}",
        card["id"]
    ));
    assert_eq!(state, "dismissed", "its card closes with it");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_follow_up_card_ends_the_wait() {
    let due = (chrono::Local::now() + chrono::Duration::days(2))
        .format("%Y-%m-%d")
        .to_string();
    let script = json!({ "conversations": [
        yes("follow_up", 0.9),
        { "matches": "Now: ", "turns": [{ "text": json!({
            "follow_up": true, "note": "Ask Riley again", "when": due }).to_string() }] },
        { "matches": "Riley", "turns": [{ "text": "Waiting on Riley; I'll look again in two days." }] }
    ]});
    let env = Env::with_config(&script, &config("open_loops"));
    let (_d, _log) = daemon(&env);
    let s = env.socket();
    say(&s, "home", "Any word from Riley?").await;
    let card = card_of(&s, "follow_up", 15).await.expect("a card");
    let id = card["id"].as_i64().unwrap();
    let (code, _) = post(&s, &format!("/v1/proactive/cards/{id}/accept"), json!({})).await;
    assert_eq!(code, 200);
    let n: i64 = env.query("SELECT count(*) FROM waits");
    assert_eq!(n, 0);
    let c: i64 = env.query("SELECT count(*) FROM ledger WHERE kind = 'wait.cancel'");
    assert_eq!(c, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_without_a_promise_asks_nothing_more() {
    let script = json!({ "conversations": [
        yes("follow_up", 0.05),
        { "matches": "Now: ", "turns": [{ "text": "{\"follow_up\": true, \"note\": \"x\"}" }] },
        { "matches": "capital", "turns": [{ "text": "The capital of France is Paris." }] }
    ]});
    let env = Env::with_config(&script, &config("open_loops"));
    let (_d, log) = daemon(&env);
    let s = env.socket();
    say(&s, "home", "What is the capital of France?").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(asked(&log, "Now: "), 0);
    let n: i64 = env.query("SELECT count(*) FROM waits");
    assert_eq!(n, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_subject_gets_one_hint_that_never_switches_by_itself() {
    let script = json!({
        "conversations": [ yes("new_topic", 0.95) ],
        "turns": [
            { "text": "Here is a plan." }, { "text": "Add the roadmap slide." },
            { "text": "Booked." }, { "text": "Done." }
        ]
    });
    let env = Env::with_config(&script, &config("new_topic"));
    let (_d, log) = daemon(&env);
    let s = env.socket();
    let head = |env: &Env| -> String {
        env.query("SELECT main_session_id FROM channels WHERE id = 'home'")
    };
    say(&s, "home", "Plan the quarterly roadmap review").await;
    let before = head(&env);
    say(&s, "home", "What should go in the roadmap deck?").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(asked(&log, "Text:"), 0, "no history yet: nothing asked");
    say(&s, "home", "Book me a flight to Lisbon for Tuesday").await;
    let card = card_of(&s, "new_topic", 15).await.expect("a hint");
    assert_eq!(card["accept_label"], "New topic");
    assert_eq!(asked(&log, "Text:"), 1);
    // Not switched on its own.
    assert_eq!(head(&env), before);
    // One hint per conversation: a fourth message asks nothing.
    say(&s, "home", "And a hotel near the airport please").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(asked(&log, "Text:"), 1);
    // Accepting is the cut: the stream moves to a new segment.
    let id = card["id"].as_i64().unwrap();
    let (code, _) = post(&s, &format!("/v1/proactive/cards/{id}/accept"), json!({})).await;
    assert_eq!(code, 200);
    let after = head(&env);
    assert_ne!(after, before);
    let reason: String = env.query(&format!(
        "SELECT segment_reason FROM sessions WHERE id = '{after}'"
    ));
    assert_eq!(reason, "new_topic");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hint_is_off_when_the_switch_is() {
    let script = json!({
        "conversations": [ yes("new_topic", 0.95) ],
        "turns": [{ "text": "One." }, { "text": "Two." }, { "text": "Three." }]
    });
    let env = Env::with_config(&script, &config("heartbeat"));
    let (_d, log) = daemon(&env);
    let s = env.socket();
    for t in [
        "Plan the roadmap",
        "Draft the roadmap deck",
        "Book a flight to Lisbon",
    ] {
        say(&s, "home", t).await;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(asked(&log, "Text:"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_weekly_tidy_merges_flags_logs_and_undoes() {
    let script = json!({ "conversations": [
        yes("tidy", 0.9),
        { "matches": "#1 (", "turns": [{ "text": json!({
            "merge": [{ "ids": [1, 2], "text": "Mitch prefers tea to coffee." }],
            "conflicts": [{ "ids": [3, 4], "note": "Standup time differs" }]
        }).to_string() }] }
    ]});
    let env = Env::with_config(&script, &config("tidy_memory"));
    let (_d, log) = daemon(&env);
    let s = env.socket();
    for (i, m) in [
        "Mitch likes tea",
        "Mitch prefers tea to coffee",
        "Standup is at 9",
        "Standup is at 10",
        "Mitch works at Inngest",
        "Mitch lives in Leeds",
    ]
    .iter()
    .enumerate()
    {
        env.db()
            .execute(
                "INSERT INTO memories (id, content, tags, created_at, source) VALUES (?1, ?2, '', 1, 'agent')",
                rusqlite::params![i as i64 + 1, m],
            )
            .unwrap();
    }
    let week = |ago: &str| {
        env.db()
            .execute(
                "INSERT INTO proactive_state (key, value, updated_at) VALUES ('tidy:last', ?1, 0)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [ago],
            )
            .unwrap();
    };
    week("0");
    let card = card_of(&s, "memory_tidy", 15).await.expect("a card");
    assert!(card["body"].as_str().unwrap().contains("Merged 1"));
    let n: i64 = env.query("SELECT count(*) FROM memories");
    assert_eq!(n, 5);
    let kept: String = env.query("SELECT content FROM memories WHERE id = 1");
    assert_eq!(kept, "Mitch prefers tea to coffee.");
    // Conflicts are only flagged: both are still there.
    let both: i64 = env.query("SELECT count(*) FROM memories WHERE id IN (3, 4)");
    assert_eq!(both, 2);
    let tidy = get(&s, "/v1/memory-tidy").await.1;
    let items = tidy.as_array().unwrap();
    assert_eq!(items.len(), 2, "{tidy}");
    let merge = items.iter().find(|i| i["action"] == "merge").unwrap();
    assert_eq!(merge["memories"].as_array().unwrap().len(), 2);
    assert_eq!(asked(&log, "#1 ("), 1);

    // A week later with nothing changed: no model call at all.
    week("0");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(asked(&log, "Text:"), 1, "one decide, from the first pass");
    assert_eq!(asked(&log, "#1 ("), 1);

    // Undo brings the merged memory and its old words back.
    let id = merge["id"].as_i64().unwrap();
    let (code, _) = post(&s, &format!("/v1/memory-tidy/{id}/undo"), json!({})).await;
    assert_eq!(code, 200);
    let n: i64 = env.query("SELECT count(*) FROM memories");
    assert_eq!(n, 6);
    let old: String = env.query("SELECT content FROM memories WHERE id = 1");
    assert_eq!(old, "Mitch likes tea");
    let (code, _) = post(&s, &format!("/v1/memory-tidy/{id}/undo"), json!({})).await;
    assert_eq!(code, 404, "once");
}
