//! Approvals land where the work is (DECISIONS.md "Approvals land where
//! the work is"): a run's approvals and secret requests wait in its
//! conversation as `needs_you` posts, with a pointer in Home, whether the
//! run is the conversation's own, a sub-agent, a delegate or a routine.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use subagents_env::{Env, Events, get, post};

async fn posts(s: &Path, channel: &str) -> Vec<Value> {
    get(s, &format!("/v1/channels/{channel}/posts"))
        .await
        .1
        .as_array()
        .unwrap()
        .clone()
}

/// A channel by slug, or by id.
async fn channel(s: &Path, slug: &str) -> Value {
    let (_, all) = get(s, "/v1/channels").await;
    all.as_array()
        .unwrap()
        .iter()
        .find(|c| c["slug"] == slug || c["id"] == slug)
        .unwrap_or_else(|| panic!("no #{slug}: {all:#}"))
        .clone()
}

/// An agent's DM channel id.
async fn dm(s: &Path, agent: &str) -> String {
    let (_, all) = get(s, "/v1/agents").await;
    all.as_array()
        .unwrap()
        .iter()
        .find(|a| a["slug"] == agent)
        .and_then(|a| a["main_channel_id"].as_str())
        .unwrap_or_else(|| panic!("no DM for {agent}: {all:#}"))
        .to_owned()
}

/// The first approval or secret request from the event stream that isn't
/// one of `seen`.
async fn next_request_after(events: &mut Events, kind: &str, seen: &[&str]) -> Value {
    let fresh = |v: &Value| {
        v.get(kind)
            .filter(|r| !seen.iter().any(|id| r["id"] == *id))
            .cloned()
    };
    let evs = events
        .until(30, |e| e.iter().any(|v| fresh(v).is_some()))
        .await;
    evs.iter().find_map(fresh).unwrap()
}

async fn next_request(events: &mut Events, kind: &str) -> Value {
    next_request_after(events, kind, &[]).await
}

/// The `needs_you` post in `channel` standing for request `id`, once its
/// state is `open` (or closed).
async fn request_post(s: &Path, channel: &str, field: &str, id: &str, open: bool) -> Value {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let all = posts(s, channel).await;
        if let Some(p) = all.iter().find(|p| {
            p["kind"] == "needs_you" && p[field] == id && p["resolved_at"].is_null() == open
        }) {
            return p.clone();
        }
        assert!(Instant::now() < end, "#{channel}: {all:#?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn pointer_to(home: &[Value], post: &Value) -> Value {
    home.iter()
        .find(|p| p["kind"] == "pointer" && p["points_to"] == post["id"])
        .unwrap_or_else(|| panic!("no pointer to {post}: {home:#?}"))
        .clone()
}

async fn answer(s: &Path, id: &str, decision: &str) {
    let (code, body) = post(
        s,
        &format!("/v1/approvals/{id}"),
        json!({ "decision": decision }),
    )
    .await;
    assert!(code < 300, "{code} {body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channel_runs_approval_waits_in_the_channel_with_a_pointer_in_home() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Check the listing", "turns": [
            { "text": "", "tool_calls": [{ "name": "shell", "input": { "command": "echo listing" } }] },
            { "text": "", "tool_calls": [{ "name": "shell", "input": { "command": "echo again" } }] },
            { "text": "Checked." }
        ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    post(&s, "/v1/channels", json!({ "slug": "home-service-market" })).await;
    let market = channel(&s, "home-service-market").await;
    let mut events = Events::open(&s).await;
    let (code, sent) = post(
        &s,
        "/v1/channels/home-service-market/posts",
        json!({ "text": "Check the listing" }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");

    // The card is the channel's: the event says so, and the channel has a
    // needs-you post for it, with a pointer in Home.
    let a = next_request(&mut events, "approval_requested").await;
    assert_eq!(a["channel_id"], market["id"], "{a}");
    let id = a["id"].as_str().unwrap();
    let needs = request_post(&s, "home-service-market", "approval_id", id, true).await;
    assert!(
        needs["text"]
            .as_str()
            .unwrap()
            .starts_with("Needs your approval: shell"),
        "{needs}"
    );
    let home = posts(&s, "home").await;
    let pointer = pointer_to(&home, &needs);
    assert_eq!(pointer["approval_id"], id);
    assert_eq!(pointer["points_to_channel"], market["id"]);
    assert_eq!(channel(&s, "home-service-market").await["needs_you"], 1);
    // Home counts the pointer, not the card a second time.
    assert_eq!(channel(&s, "home").await["needs_you"], 1);

    // "Done" can't close it while the run waits for the answer.
    let (code, _) = post(
        &s,
        &format!("/v1/posts/{}/resolve", pointer["id"]),
        json!({}),
    )
    .await;
    assert_eq!(code, 409);
    let (code, _) = post(&s, &format!("/v1/posts/{}/resolve", needs["id"]), json!({})).await;
    assert_eq!(code, 409);

    // Answering closes both, and the post says how it ended.
    answer(&s, id, "once").await;
    let done = request_post(&s, "home-service-market", "approval_id", id, false).await;
    assert!(
        done["text"]
            .as_str()
            .unwrap()
            .starts_with("Approved: shell"),
        "{done}"
    );
    let home = posts(&s, "home").await;
    assert!(!pointer_to(&home, &done)["resolved_at"].is_null());

    // The second approval: denied, which closes it the same way.
    let b = next_request_after(&mut events, "approval_requested", &[id]).await;
    let id = b["id"].as_str().unwrap();
    request_post(&s, "home-service-market", "approval_id", id, true).await;
    answer(&s, id, "deny").await;
    let done = request_post(&s, "home-service-market", "approval_id", id, false).await;
    assert!(
        done["text"].as_str().unwrap().starts_with("Denied: "),
        "{done}"
    );
    let home = posts(&s, "home").await;
    assert!(!pointer_to(&home, &done)["resolved_at"].is_null());
    get(
        &s,
        &format!("/v1/runs/{}/wait", sent["run_id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(channel(&s, "home-service-market").await["needs_you"], 0);
    assert_eq!(channel(&s, "home").await["needs_you"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_homes_own_approval_stays_a_card_in_home() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Echo it", "turns": [
            { "text": "", "tool_calls": [{ "name": "shell", "input": { "command": "echo home" } }] },
            { "text": "Done." }
        ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let mut events = Events::open(&s).await;
    let (_, sent) = post(&s, "/v1/channels/home/posts", json!({ "text": "Echo it" })).await;
    let a = next_request(&mut events, "approval_requested").await;
    assert_eq!(a["channel_id"], "home");
    assert_eq!(channel(&s, "home").await["needs_you"], 1);
    assert!(
        posts(&s, "home")
            .await
            .iter()
            .all(|p| p["kind"] != "needs_you"),
        "no post: the card is in Home's stream"
    );
    answer(&s, a["id"].as_str().unwrap(), "once").await;
    get(
        &s,
        &format!("/v1/runs/{}/wait", sent["run_id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(channel(&s, "home").await["needs_you"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn sub_agent_and_delegated_approvals_wait_in_the_top_level_conversation() {
    let env = Env::new(&json!({ "conversations": [
        // Children's briefs first: they are also in the parent's transcript.
        { "matches": "Count the files", "turns": [
            { "text": "", "tool_calls": [{ "name": "shell", "input": { "command": "echo helper" } }] },
            { "text": "Counted." }
        ]},
        { "matches": "Draft the permit note", "turns": [
            { "text": "", "tool_calls": [{ "name": "write_file",
                "input": { "path": "/tmp/tern-in-place-permit.txt", "content": "permit" } }] },
            { "text": "Drafted." }
        ]},
        { "matches": "Split it up", "turns": [
            { "text": "", "tool_calls": [
                { "name": "agent_spawn", "input": { "label": "helper", "task": "Count the files." } },
                { "name": "agent_spawn", "input": {
                    "label": "permit", "agent": "writer", "task": "Draft the permit note." } },
                { "name": "agent_wait", "input": {} }] },
            { "text": "Both done." }
        ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    post(
        &s,
        "/v1/agents",
        json!({ "name": "Writer", "model": "mock:writer" }),
    )
    .await;
    post(&s, "/v1/channels", json!({ "slug": "renovation" })).await;
    let reno = channel(&s, "renovation").await;
    let writer = dm(&s, "writer").await;
    let mut events = Events::open(&s).await;
    let (_, sent) = post(
        &s,
        "/v1/channels/renovation/posts",
        json!({ "text": "Split it up" }),
    )
    .await;

    let evs = events
        .until(30, |e| {
            e.iter()
                .filter(|v| v.get("approval_requested").is_some())
                .count()
                >= 2
        })
        .await;
    let asks: Vec<Value> = evs
        .into_iter()
        .filter_map(|v| v.get("approval_requested").cloned())
        .collect();
    for a in &asks {
        // Both children ask in #renovation, where the user is watching,
        // not in Home and not in Writer's DM.
        assert_eq!(a["channel_id"], reno["id"], "{a}");
        assert!(a["parent_run_id"].is_string(), "{a}");
        let id = a["id"].as_str().unwrap();
        let needs = request_post(&s, "renovation", "approval_id", id, true).await;
        pointer_to(&posts(&s, "home").await, &needs);
    }
    let delegated = asks
        .iter()
        .find(|a| a["agent_label"] == "permit")
        .unwrap_or_else(|| panic!("{asks:#?}"));
    let by = request_post(
        &s,
        "renovation",
        "approval_id",
        delegated["id"].as_str().unwrap(),
        true,
    )
    .await;
    assert_eq!(by["author"], "Writer", "the delegate asks: {by}");
    assert!(
        posts(&s, &writer)
            .await
            .iter()
            .all(|p| p["kind"] != "needs_you"),
        "nothing waits in Writer's DM"
    );
    for a in &asks {
        answer(&s, a["id"].as_str().unwrap(), "deny").await;
    }
    get(
        &s,
        &format!("/v1/runs/{}/wait", sent["run_id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(channel(&s, "renovation").await["needs_you"], 0);
    assert_eq!(channel(&s, "home").await["needs_you"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_routines_requests_wait_in_the_channel_it_posts_to() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Morning check", "turns": [
            { "text": "", "tool_calls": [{ "name": "shell", "input": { "command": "echo morning" } }] },
            { "text": "", "tool_calls": [{ "name": "secret_request", "input": {
                "names": ["LISTINGS_KEY"], "reason": "The listings site needs a key." } }] },
            { "text": "All checked." }
        ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    post(&s, "/v1/channels", json!({ "slug": "listings" })).await;
    let listings = channel(&s, "listings").await;
    let (_, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Morning check", "channel": "listings" }),
    )
    .await;
    let mut events = Events::open(&s).await;
    let (_, r) = post(
        &s,
        &format!("/v1/schedules/{}/run", sch["id"].as_str().unwrap()),
        json!({}),
    )
    .await;

    // The routine's private session isn't in a channel: its requests go
    // where it posts.
    let a = next_request(&mut events, "approval_requested").await;
    assert_eq!(a["channel_id"], listings["id"], "{a}");
    let id = a["id"].as_str().unwrap();
    let needs = request_post(&s, "listings", "approval_id", id, true).await;
    pointer_to(&posts(&s, "home").await, &needs);
    answer(&s, id, "once").await;

    let r2 = next_request(&mut events, "secret_requested").await;
    assert_eq!(r2["channel_id"], listings["id"], "{r2}");
    let sid = r2["id"].as_str().unwrap();
    let needs = request_post(&s, "listings", "secret_request_id", sid, true).await;
    assert!(
        needs["text"]
            .as_str()
            .unwrap()
            .starts_with("Needs your secret LISTINGS_KEY · "),
        "{needs}"
    );
    let pointer = pointer_to(&posts(&s, "home").await, &needs);
    assert_eq!(pointer["secret_request_id"], sid);
    let (code, _) = post(
        &s,
        &format!("/v1/secret-requests/{sid}"),
        json!({ "values": {} }),
    )
    .await;
    assert!(code < 300);
    let done = request_post(&s, "listings", "secret_request_id", sid, false).await;
    assert!(
        done["text"]
            .as_str()
            .unwrap()
            .starts_with("Skipped: secret LISTINGS_KEY"),
        "{done}"
    );
    assert!(!pointer_to(&posts(&s, "home").await, &done)["resolved_at"].is_null());
    get(
        &s,
        &format!("/v1/runs/{}/wait", r["run_id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(channel(&s, "listings").await["needs_you"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_dm_run_asks_in_its_dm() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Tidy the notes", "turns": [
            { "text": "", "tool_calls": [{ "name": "write_file",
                "input": { "path": "/tmp/tern-in-place-notes.txt", "content": "tidy" } }] },
            { "text": "Tidied." }
        ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    post(
        &s,
        "/v1/agents",
        json!({ "name": "Notes", "model": "mock:notes" }),
    )
    .await;
    let dm = dm(&s, "notes").await;
    let mut events = Events::open(&s).await;
    let (_, sent) = post(
        &s,
        &format!("/v1/channels/{dm}/posts"),
        json!({ "text": "Tidy the notes" }),
    )
    .await;
    let a = next_request(&mut events, "approval_requested").await;
    assert_eq!(a["channel_id"], dm.as_str());
    let id = a["id"].as_str().unwrap();
    let needs = request_post(&s, &dm, "approval_id", id, true).await;
    assert_eq!(needs["author"], "Notes");
    pointer_to(&posts(&s, "home").await, &needs);
    assert_eq!(channel(&s, &dm).await["needs_you"], 1);
    assert_eq!(channel(&s, "home").await["needs_you"], 1);
    answer(&s, id, "deny").await;
    get(
        &s,
        &format!("/v1/runs/{}/wait", sent["run_id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(channel(&s, &dm).await["needs_you"], 0);
    assert_eq!(channel(&s, "home").await["needs_you"], 0);
}
