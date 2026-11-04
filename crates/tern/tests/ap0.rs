//! Agent profiles AP0 end-to-end (docs/agent-profiles-design.md §8): the
//! `/v1/agents` API and persona files, each agent's DM, what `start_run`
//! freezes, tool and memory scopes, and agent-owned schedules.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use subagents_env::{Env, api, get, post};

async fn send(socket: &Path, method: reqwest::Method, path: &str, body: Value) -> (u16, Value) {
    let r = api(socket)
        .request(method, format!("http://tern{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

async fn patch(socket: &Path, path: &str, body: Value) -> (u16, Value) {
    send(socket, reqwest::Method::PATCH, path, body).await
}

async fn put(socket: &Path, path: &str, body: Value) -> (u16, Value) {
    send(socket, reqwest::Method::PUT, path, body).await
}

/// Send `text` into a channel and wait for its run; returns the run id.
async fn say(socket: &Path, channel: &str, text: &str) -> String {
    let (code, sent) = post(
        socket,
        &format!("/v1/channels/{channel}/posts"),
        json!({ "text": text }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(socket, &format!("/v1/runs/{run}/wait")).await;
    run
}

fn run_col(env: &Env, run: &str, col: &str) -> Option<String> {
    env.db()
        .query_row(
            &format!("SELECT {col} FROM runs WHERE id = ?1"),
            [run],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_are_created_listed_renamed_and_archived_with_their_dms() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let _d = env.daemon_with(None);
    let s = env.socket();

    let (_, health) = get(&s, "/v1/health").await;
    assert!(
        health["features"]
            .as_array()
            .unwrap()
            .contains(&json!("agents"))
    );
    // An existing setup is one agent, tern, whose DM is Home.
    let (_, list) = get(&s, "/v1/agents").await;
    assert_eq!(list.as_array().unwrap().len(), 1, "{list:#}");
    assert_eq!(
        (list[0]["id"].as_str(), list[0]["main_channel_id"].as_str()),
        (Some("tern"), Some("home"))
    );
    assert_eq!(list[0]["memory"], "shared");
    assert_eq!(list[0]["tools"], Value::Null);

    let (_, templates) = get(&s, "/v1/agents/templates").await;
    assert!(
        templates
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == "research")
    );

    let (code, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Research", "template": "research" }),
    )
    .await;
    assert_eq!(code, 201, "{a}");
    assert_eq!(
        (a["id"].as_str(), a["slug"].as_str(), a["memory"].as_str()),
        (Some("tern:research"), Some("research"), Some("private"))
    );
    assert_eq!(a["tools"]["builtins"], "read_only");
    assert_eq!(a["avatar"]["glyph"], "◕");
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();
    let persona = env.tern_home.join("agents/research/persona.md");
    assert!(
        std::fs::read_to_string(&persona)
            .unwrap()
            .starts_with("You are Research")
    );
    // Its DM is a main channel.
    let (_, channels) = get(&s, "/v1/channels").await;
    let c = channels
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == dm.as_str())
        .unwrap();
    assert_eq!(
        (
            c["kind"].as_str(),
            c["slug"].as_str(),
            c["agent_id"].as_str()
        ),
        (Some("main"), Some("dm-research"), Some("tern:research"))
    );

    for (body, want) in [
        (json!({ "name": "Research" }), 409),
        (json!({ "name": "x", "slug": "Bad Slug" }), 400),
        (json!({ "name": "x", "template": "nope" }), 400),
        (json!({ "name": "x", "memory": "both" }), 400),
        (json!({ "name": "  " }), 400),
    ] {
        assert_eq!(post(&s, "/v1/agents", body.clone()).await.0, want, "{body}");
    }

    // Persona: the editor's text, with the engine-owned sections apart.
    let (_, p) = get(&s, "/v1/agents/research/persona").await;
    assert!(
        p["persona"]
            .as_str()
            .unwrap()
            .starts_with("You are Research")
    );
    let sys = p["system_sections"].as_str().unwrap();
    assert!(sys.contains("## Untrusted content") && sys.contains("## Agents"));
    assert!(!sys.contains("You are Research"));
    let (code, _) = put(
        &s,
        "/v1/agents/research/persona",
        json!({ "persona": "You are Scout." }),
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(
        std::fs::read_to_string(&persona).unwrap(),
        "You are Scout.\n"
    );

    // Rename: the id stays, the DM follows.
    let (code, a) = patch(
        &s,
        "/v1/agents/research",
        json!({ "name": "Scout", "slug": "scout", "memory": "shared", "avatar": null }),
    )
    .await;
    assert_eq!(code, 200, "{a}");
    assert_eq!(
        (a["id"].as_str(), a["slug"].as_str()),
        (Some("tern:research"), Some("scout"))
    );
    assert_eq!(a["avatar"], Value::Null);
    assert_eq!(get(&s, "/v1/channels/dm-scout/posts").await.0, 200);

    // The default agent: renamed, never archived or made private.
    let (code, t) = patch(&s, "/v1/agents/tern", json!({ "name": "Ari" })).await;
    assert_eq!((code, t["name"].as_str()), (200, Some("Ari")));
    for body in [
        json!({ "archived": true }),
        json!({ "memory": "private" }),
        json!({ "slug": "ari" }),
    ] {
        assert_eq!(patch(&s, "/v1/agents/tern", body).await.0, 400);
    }
    // A DM can't be archived on its own.
    assert_eq!(
        patch(
            &s,
            &format!("/v1/channels/{dm}"),
            json!({ "archived": true })
        )
        .await
        .0,
        400
    );

    let (code, _) = patch(&s, "/v1/agents/scout", json!({ "archived": true })).await;
    assert_eq!(code, 200);
    let (_, list) = get(&s, "/v1/agents").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, list) = get(&s, "/v1/agents?archived=1").await;
    assert_eq!(list[1]["archived"], true);
    let (code, _) = post(
        &s,
        &format!("/v1/channels/{dm}/posts"),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(code, 400, "an archived agent's DM takes no sends");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_freezes_its_agent_and_an_edit_applies_to_the_next_one() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "slow", "turns": [{ "text": "done slowly", "delay_ms": 1500 }] },
            { "matches": "again", "turns": [{ "text": "done again" }] },
            { "matches": "hello", "turns": [{ "text": "hi from home" }] }
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Inbox", "persona": "You are Inbox, version one.", "memory": "private" }),
    )
    .await;
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();

    // Start a slow run, edit the agent while it works.
    let (_, sent) = post(
        &s,
        &format!("/v1/channels/{dm}/posts"),
        json!({ "text": "slow please" }),
    )
    .await;
    let slow = sent["run_id"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(300)).await;
    put(
        &s,
        "/v1/agents/inbox/persona",
        json!({ "persona": "You are Inbox, version two." }),
    )
    .await;
    patch(
        &s,
        "/v1/agents/inbox",
        json!({ "tools": { "builtins": ["read_file"], "connectors": [], "mcp": [] }, "memory": "shared" }),
    )
    .await;
    let (_, out) = get(&s, &format!("/v1/runs/{slow}/wait")).await;
    assert_eq!(out["status"], "completed", "{out}");

    // The running run kept what it started with.
    let sys = run_col(&env, &slow, "system_prompt").unwrap();
    assert!(sys.starts_with("You are Inbox, version one."), "{sys}");
    assert!(sys.contains("## Agents") && sys.contains("@tern"));
    let scope: Vec<String> =
        serde_json::from_str(&run_col(&env, &slow, "tool_scope").unwrap()).unwrap();
    assert!(!scope.contains(&"shell".to_owned()), "{scope:?}");
    assert!(scope.contains(&"write_file".to_owned()), "{scope:?}");
    assert_eq!(
        run_col(&env, &slow, "memory_scope").as_deref(),
        Some("private")
    );

    // The next one has the edit.
    let next = say(&s, &dm, "again").await;
    let sys = run_col(&env, &next, "system_prompt").unwrap();
    assert!(sys.starts_with("You are Inbox, version two."), "{sys}");
    let scope: Vec<String> =
        serde_json::from_str(&run_col(&env, &next, "tool_scope").unwrap()).unwrap();
    assert!(scope.contains(&"read_file".to_owned()) && !scope.contains(&"write_file".to_owned()));
    assert!(
        scope.contains(&"memory_save".to_owned()),
        "the system layer is always in"
    );
    assert_eq!(run_col(&env, &next, "memory_scope"), None);

    // Home's runs are tern's, as before: everything in scope.
    let home = say(&s, "home", "hello").await;
    assert_eq!(run_col(&env, &home, "tool_scope"), None);
    let sys = run_col(&env, &home, "system_prompt").unwrap();
    assert!(!sys.starts_with("You are Inbox"));
    let (_, list) = get(&s, "/v1/sessions?channel=home").await;
    assert_eq!(list[0]["agent_id"], Value::Null);
    let (_, list) = get(&s, &format!("/v1/sessions?channel={dm}")).await;
    assert_eq!(list[0]["agent_id"], "tern:inbox");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_is_held_to_its_tool_and_memory_scope() {
    let env = Env::with_config(
        &json!({ "conversations": [
            // First: the new topic's message carries the last one's text.
            { "matches": "remember", "turns": [
                { "tool_calls": [{ "name": "memory_save", "input": { "content": "acme runs postgres 16" } }] },
                { "text": "saved" }
            ]},
            { "matches": "try the shell", "turns": [
                { "tool_calls": [{ "name": "shell", "input": { "command": "echo hi" } }] },
                { "text": "couldn't" }
            ]},
            { "matches": "what do you know", "turns": [
                { "tool_calls": [{ "name": "memory_search", "input": { "query": "acme postgres" } }] },
                { "text": "checked" }
            ]}
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Research", "template": "research" }),
    )
    .await;
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();

    let run = say(&s, &dm, "try the shell").await;
    let out: String = env
        .db()
        .query_row(
            "SELECT output_json FROM steps WHERE run_id = ?1 AND kind = 'tool' ORDER BY idx LIMIT 1",
            [&run],
            |r| r.get(0),
        )
        .unwrap();
    assert!(out.contains("not available to this agent"), "{out}");

    // A new topic: the mock picks its script by a session's first message.
    let (_, sent) = post(
        &s,
        &format!("/v1/channels/{dm}/posts"),
        json!({ "text": "remember this", "new_topic": true }),
    )
    .await;
    let r = sent["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{r}/wait")).await;
    let owner: Option<String> = env.query("SELECT agent_id FROM memories");
    assert_eq!(owner.as_deref(), Some("tern:research"));
    let (_, own) = get(&s, "/v1/memories?agent=research").await;
    assert_eq!(own.as_array().unwrap().len(), 1);
    let (_, shared) = get(&s, "/v1/memories?agent=shared").await;
    assert!(shared.as_array().unwrap().is_empty());

    // tern doesn't see Research's private memory.
    let run = say(&s, "home", "what do you know about acme").await;
    let out: String = env
        .db()
        .query_row(
            "SELECT output_json FROM steps WHERE run_id = ?1 AND kind = 'tool' ORDER BY idx LIMIT 1",
            [&run],
            |r| r.get(0),
        )
        .unwrap();
    assert!(out.contains("no matching memories"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_schedules_are_its_own_and_post_to_its_dm() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "every weekday", "turns": [
                { "tool_calls": [{ "name": "schedule_create", "input": { "cron": "0 8 * * 1-5", "prompt": "sweep my inbox" } }] },
                { "text": "scheduled" }
            ]},
            { "matches": "sweep my inbox", "turns": [{ "text": "Two need a reply." }] },
            { "matches": "list", "turns": [
                { "tool_calls": [{ "name": "schedule_list", "input": {} }] },
                { "text": "listed" }
            ]}
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Inbox", "template": "inbox" }),
    )
    .await;
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();

    say(&s, &dm, "every weekday at 8, sweep my inbox").await;
    let (_, mine) = get(&s, "/v1/schedules?agent=inbox").await;
    let sch = &mine.as_array().unwrap()[0];
    assert_eq!(
        (sch["agent_id"].as_str(), sch["channel_id"].as_str()),
        (Some("tern:inbox"), Some(dm.as_str()))
    );
    let (_, terns) = get(&s, "/v1/schedules?agent=tern").await;
    assert!(terns.as_array().unwrap().is_empty());

    // Its runs act as Inbox, and the result is posted into Inbox's DM.
    let (_, r) = post(
        &s,
        &format!("/v1/schedules/{}/run", sch["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let run = r["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{run}/wait")).await;
    let sys = run_col(&env, &run, "system_prompt").unwrap();
    assert!(sys.starts_with("You are Inbox"), "{sys}");
    let (_, posts) = get(&s, &format!("/v1/channels/{dm}/posts")).await;
    assert_eq!(posts[0]["text"], "Two need a reply.");
    assert_eq!(posts[0]["author_kind"], "routine");

    // tern's schedule_list doesn't show Inbox's job.
    let run = say(&s, "home", "list my schedules").await;
    let out: String = env
        .db()
        .query_row(
            "SELECT output_json FROM steps WHERE run_id = ?1 AND kind = 'tool' ORDER BY idx LIMIT 1",
            [&run],
            |r| r.get(0),
        )
        .unwrap();
    assert!(out.contains("no schedules"), "{out}");

    // Handing the job to tern moves it to Home.
    let (_, moved) = patch(
        &s,
        &format!("/v1/schedules/{}", sch["id"].as_str().unwrap()),
        json!({ "agent": "tern" }),
    )
    .await;
    assert_eq!(
        (moved["agent_id"].as_str(), moved["channel_id"].as_str()),
        (Some("tern"), Some("home"))
    );
    // Archiving pauses an agent's jobs.
    let (_, _) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 9 * * *", "prompt": "brief", "agent": "inbox" }),
    )
    .await;
    patch(&s, "/v1/agents/inbox", json!({ "archived": true })).await;
    let (_, mine) = get(&s, "/v1/schedules?agent=inbox").await;
    assert_eq!(mine[0]["enabled"], false);
}

#[test]
fn tern_agents_lists_creates_and_archives() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let out = |args: &[&str]| {
        let o = env.cmd(args).output().unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    assert!(out(&["agents"]).contains("@tern"));
    let created = out(&["agents", "new", "Market Watch", "--template", "marketing"]);
    assert!(
        created.contains("@market-watch") && created.contains("#dm-market-watch"),
        "{created}"
    );
    let list = out(&["agents", "list"]);
    assert!(
        list.contains("@market-watch") && list.contains("Market Watch"),
        "{list}"
    );
    out(&["agents", "archive", "market-watch"]);
    assert!(!out(&["agents"]).contains("market-watch"));
    assert!(out(&["agents", "list", "--all"]).contains("[archived]"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_model_chain_falls_back_like_default_models() {
    let env = Env::with_config(
        &json!({ "fail_models": { "down": 529 }, "turns": [{ "text": "hello from up" }] }),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (code, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Fast", "model": ["mock:down", "mock:up"] }),
    )
    .await;
    assert_eq!(code, 201, "{a}");
    assert_eq!(a["model"], json!(["mock:down", "mock:up"]));
    let run = say(&s, a["main_channel_id"].as_str().unwrap(), "hi").await;
    assert_eq!(run_col(&env, &run, "model").as_deref(), Some("mock:down"));
    let usage: String = env
        .db()
        .query_row(
            "SELECT usage_json FROM steps WHERE run_id = ?1 AND kind = 'llm'",
            [&run],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        usage.contains("mock:up"),
        "answered by the fallback: {usage}"
    );
    // The agent's model isn't pinned on its DM thread, so an edit reaches it.
    patch(&s, "/v1/agents/fast", json!({ "model": "mock:up" })).await;
    let run = say(&s, a["main_channel_id"].as_str().unwrap(), "hi again").await;
    assert_eq!(run_col(&env, &run, "model").as_deref(), Some("mock:up"));
}
