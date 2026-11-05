//! Proactive delegation: every agent that may hand work on sees a bounded,
//! fresh roster of its peers, the prompt says when and how to hand off, the
//! seeded starters can delegate to each other out of the box, and a hand-off
//! never widens what the caller could do (ceilings, depth, no self-hand-off).
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use std::path::Path;

use serde_json::{Value, json};
use subagents_env::{Env, api, get, post};

/// Seed the starter agents, as a first start does (the harness turns it off).
fn seed(env: &Env) {
    let o = env
        .cmd(&["agents", "list"])
        .env_remove("TERN_SEED_AGENTS")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

async fn say(socket: &Path, channel: &str, text: &str) -> String {
    let (code, sent) = post(
        socket,
        &format!("/v1/channels/{channel}/posts"),
        json!({ "text": text, "new_topic": true }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(socket, &format!("/v1/runs/{run}/wait")).await;
    run
}

async fn patch(socket: &Path, path: &str, body: Value) -> Value {
    let r = api(socket)
        .patch(format!("http://tern{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    r.json().await.unwrap_or(Value::Null)
}

fn col(env: &Env, sql: &str, id: &str) -> Option<String> {
    env.db().query_row(sql, [id], |r| r.get(0)).unwrap()
}

fn system_prompt(env: &Env, run: &str) -> String {
    col(env, "SELECT system_prompt FROM runs WHERE id = ?1", run).unwrap()
}

fn tool_scope(env: &Env, run: &str) -> Vec<String> {
    serde_json::from_str(&col(env, "SELECT tool_scope FROM runs WHERE id = ?1", run).unwrap())
        .unwrap()
}

fn first_tool(env: &Env, run: &str) -> String {
    env.db()
        .query_row(
            "SELECT output_json FROM steps WHERE run_id = ?1 AND kind = 'tool' ORDER BY idx LIMIT 1",
            [run],
            |r| r.get(0),
        )
        .unwrap()
}

/// The delegation list in `sys`: the lines under "You may delegate to:".
fn roster(sys: &str) -> Vec<String> {
    let rest = sys.split("You may delegate to:").nth(1).unwrap_or("");
    rest.lines()
        .skip(1)
        .take_while(|l| l.starts_with("- "))
        .map(str::to_owned)
        .collect()
}

async fn dm(s: &Path, slug: &str) -> String {
    let (_, list) = get(s, "/v1/agents").await;
    let a = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["slug"] == slug)
        .unwrap_or_else(|| panic!("no agent {slug}: {list}"));
    a["main_channel_id"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn tern_hands_a_clearly_in_lane_request_to_the_specialist_and_says_so() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Run the SEO audit for acme.test", "turns": [
            { "text": "SEO audit: three things to fix first." }] },
        { "matches": "Audit acme.test", "turns": [
            { "text": "Handing this to @seo.", "tool_calls": [{ "name": "agent_spawn", "input": {
                "label": "audit", "agent": "seo",
                "task": "Run the SEO audit for acme.test and report what to fix first." } }] },
            { "text": "", "tool_calls": [{ "name": "agent_wait", "input": {} }] },
            { "text": "seo found three things to fix first." }
        ]}
    ]}));
    seed(&env);
    let _d = env.daemon_with(None);
    let s = env.socket();

    let run = say(&s, "home", "Audit acme.test for SEO").await;
    let sys = system_prompt(&env, &run);
    // The roster: peers with what they are for and their ceiling, never itself.
    let lines = roster(&sys);
    assert!(!lines.is_empty(), "{sys}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("- `seo` · SEO") && l.contains("at most readonly")),
        "{lines:#?}"
    );
    assert!(lines.iter().any(|l| l.starts_with("- `research` ·")));
    assert!(
        lines.iter().all(|l| !l.starts_with("- `tern`")),
        "{lines:#?}"
    );
    for needle in [
        "Never hand off silently",
        "data the user wrote",
        "one hand-off per task",
    ] {
        assert!(sys.contains(needle), "missing {needle:?}");
    }

    // It said so, then handed the work to seo.
    let said: String = env.query(&format!(
        "SELECT m.text FROM runs r JOIN messages m ON m.session_id = r.session_id \
         WHERE r.id = '{run}' AND m.role = 'assistant' ORDER BY m.id LIMIT 1"
    ));
    assert!(said.contains("Handing this to @seo."), "{said}");
    let child: String = env.query(&format!(
        "SELECT id FROM runs WHERE parent_run_id = '{run}' AND label = 'audit'"
    ));
    let csys = system_prompt(&env, &child);
    assert!(
        csys.contains("## You are working for another agent"),
        "{csys}"
    );
    assert_eq!(
        col(
            &env,
            "SELECT s.agent_id FROM runs r JOIN sessions s ON s.id = r.session_id WHERE r.id = ?1",
            &child
        )
        .as_deref(),
        Some("tern:seo")
    );
    // Depth limit: a delegate (depth 1, max_depth 1) can't hand on again.
    assert!(!tool_scope(&env, &child).contains(&"agent_spawn".to_owned()));
    assert!(!csys.contains("Handing work to your agents"), "{csys}");
}

#[tokio::test(flavor = "multi_thread")]
async fn seeded_starters_can_delegate_to_each_other_by_default() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "hi", "turns": [{ "text": "hello" }] }
    ]}));
    seed(&env);
    let _d = env.daemon_with(None);
    let s = env.socket();

    let run = say(&s, &dm(&s, "assistant").await, "hi").await;
    // It has the tools, and the roster names its peers but not itself.
    let scope = tool_scope(&env, &run);
    assert!(scope.contains(&"agent_spawn".to_owned()), "{scope:?}");
    assert!(scope.contains(&"agent_wait".to_owned()));
    let lines = roster(&system_prompt(&env, &run));
    let slugs: Vec<&str> = lines.iter().map(|l| l.split('`').nth(1).unwrap()).collect();
    assert!(
        slugs.contains(&"research") && slugs.contains(&"seo"),
        "{slugs:?}"
    );
    assert!(!slugs.contains(&"assistant"), "{slugs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_agent_cannot_launder_a_write_through_a_delegate() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Rewrite the page copy", "turns": [
                { "text": "Handing this to @writer-editor.", "tool_calls": [{ "name": "agent_spawn",
                  "input": { "label": "copy", "agent": "writer-editor",
                             "task": "Draft the new copy." } }] },
                { "text": "Done." }
            ]},
            { "matches": "Draft the new copy", "turns": [{ "text": "Drafted." }] },
            { "matches": "hi", "turns": [{ "text": "hello" }] }
        ]}),
        "autonomy = \"autonomous\"\n[subagents]\nenabled = true\n",
    );
    seed(&env);
    let _d = env.daemon_with(None);
    let s = env.socket();

    // seo is read-only; writer-editor alone may write (supervised).
    let direct = say(&s, &dm(&s, "writer-editor").await, "hi").await;
    assert_eq!(
        col(&env, "SELECT autonomy FROM runs WHERE id = ?1", &direct).as_deref(),
        Some("supervised")
    );
    let run = say(&s, &dm(&s, "seo").await, "Rewrite the page copy").await;
    assert_eq!(
        col(&env, "SELECT autonomy FROM runs WHERE id = ?1", &run).as_deref(),
        Some("readonly")
    );
    let child: String = env.query(&format!(
        "SELECT id FROM runs WHERE parent_run_id = '{run}' AND label = 'copy'"
    ));
    assert_eq!(
        col(&env, "SELECT autonomy FROM runs WHERE id = ?1", &child).as_deref(),
        Some("readonly"),
        "the delegate runs under the lower of its ceiling and the caller's"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_hands_work_to_itself() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Do it yourself", "turns": [
            { "text": "", "tool_calls": [{ "name": "agent_spawn", "input": {
                "label": "me", "agent": "tern", "task": "Do the thing." } }] },
            { "text": "Couldn't." }
        ]},
        { "matches": "Research, do it yourself", "turns": [
            { "text": "", "tool_calls": [{ "name": "agent_spawn", "input": {
                "label": "me", "agent": "research", "task": "Do the thing." } }] },
            { "text": "Couldn't." }
        ]}
    ]}));
    seed(&env);
    let _d = env.daemon_with(None);
    let s = env.socket();

    let run = say(&s, "home", "Do it yourself").await;
    assert!(
        first_tool(&env, &run).contains("that's you"),
        "{}",
        first_tool(&env, &run)
    );
    let run = say(&s, &dm(&s, "research").await, "Research, do it yourself").await;
    assert!(
        first_tool(&env, &run).contains("that's you"),
        "{}",
        first_tool(&env, &run)
    );
    let n: i64 = env.query("SELECT COUNT(*) FROM runs WHERE parent_run_id IS NOT NULL");
    assert_eq!(n, 0);
    // And a roster never lists the reader.
    let sys = system_prompt(&env, &run);
    assert!(roster(&sys).iter().all(|l| !l.starts_with("- `research`")));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_roster_is_bounded_ranked_by_the_request_and_never_stale() {
    let env = Env::new(&json!({ "turns": [{ "text": "ok" }] }));
    let _d = env.daemon_with(None);
    let s = env.socket();

    for i in 0..15 {
        let (code, a) = post(
            &s,
            "/v1/agents",
            json!({ "name": format!("Agent {}", (b'a' + i) as char),
                    "persona": format!("You are agent {i}.\n\n- Your job: handle widget batch {i}.") }),
        )
        .await;
        assert_eq!(code, 201, "{a}");
    }
    let (_, zed) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Zed", "persona": "You are Zed.\n\n- Your job: groom zebras at the reserve." }),
    )
    .await;
    assert!(zed["slug"] == "zed", "{zed}");

    // Capped, with a pointer to the rest; the best match for the request is
    // in even though it sorts last by name.
    let run = say(&s, "home", "Please groom the zebras").await;
    let sys = system_prompt(&env, &run);
    let lines = roster(&sys);
    assert!(
        lines.len() <= 13,
        "12 plus the 'and more' line: {}",
        lines.len()
    );
    assert!(lines.last().unwrap().contains("and 4 more"), "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.starts_with("- `zed` ·")),
        "{lines:#?}"
    );
    assert!(
        lines[0].starts_with("- `zed` ·"),
        "best match first: {lines:#?}"
    );
    let others = sys.split("Other agents:").nth(1).unwrap();
    let others = others
        .lines()
        .skip(1)
        .take_while(|l| l.starts_with("- "))
        .count();
    assert!(others <= 13, "{others}");

    // Archive and delete drop out of the next run's roster; a new agent
    // appears in it. Nothing is cached across runs.
    patch(&s, "/v1/agents/zed", json!({ "archived": true })).await;
    let (_, fresh) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Newcomer", "persona": "You are Newcomer.\n\n- Your job: groom zebras daily." }),
    )
    .await;
    assert_eq!(fresh["slug"], "newcomer");
    let run = say(&s, "home", "Please groom the zebras").await;
    let sys = system_prompt(&env, &run);
    assert!(
        !sys.contains("`zed`") && !sys.contains("@zed"),
        "archived: {sys}"
    );
    assert!(roster(&sys)[0].starts_with("- `newcomer` ·"), "{sys}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_with_no_delegates_is_listed_peers_but_no_hand_off_section() {
    let env = Env::new(&json!({ "turns": [{ "text": "ok" }] }));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, _) = post(&s, "/v1/agents", json!({ "name": "Loner" })).await;
    let (_, _) = post(&s, "/v1/agents", json!({ "name": "Other" })).await;
    let run = say(&s, &dm(&s, "loner").await, "hi").await;
    let sys = system_prompt(&env, &run);
    assert!(
        sys.contains("Other agents:") && sys.contains("@other"),
        "{sys}"
    );
    assert!(!sys.contains("Handing work to your agents"), "{sys}");
}
