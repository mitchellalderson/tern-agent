//! Agents managing the team from chat: creating, editing and deleting
//! agents and channels, with power that only narrows, tool names that are
//! normalised, refusals that are final, and deletes that always ask.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use std::path::Path;

use serde_json::{Value, json};
use subagents_env::{Env, Events, get, post};

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

fn tool_result(env: &Env, run: &str, n: usize) -> String {
    env.db()
        .query_row(
            "SELECT output_json FROM steps WHERE run_id = ?1 AND kind = 'tool' ORDER BY idx LIMIT 1 OFFSET ?2",
            rusqlite::params![run, n as i64],
            |r| r.get(0),
        )
        .unwrap()
}

fn call(name: &str, input: Value) -> Value {
    json!({ "tool_calls": [{ "name": name, "input": input }] })
}

/// A script where each `matches` runs one tool call then says `Done.`.
fn script(cases: &[(&str, &str, Value)]) -> Value {
    let convs: Vec<Value> = cases
        .iter()
        .map(|(m, tool, input)| {
            json!({ "matches": m, "turns": [
                { "text": "", "tool_calls": call(tool, input.clone())["tool_calls"] },
                { "text": "Done." } ] })
        })
        .collect();
    json!({ "conversations": convs })
}

const AUTONOMOUS: &str = "autonomy = \"autonomous\"\n[subagents]\nenabled = true\n";

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_creates_edits_and_deletes_an_agent_with_power_that_only_narrows() {
    // Reverse order: a new topic carries the last message along.
    let env = Env::with_config(
        &script(&[
            ("Delete Scout", "agent_delete", json!({ "agent": "scout" })),
            (
                "Edit Scout",
                "agent_edit",
                json!({ "agent": "Scout", "role": "Finds leads.", "autonomy": "readonly" }),
            ),
            (
                "Toolless",
                "agent_create",
                json!({ "name": "Toolless", "tools": ["read-file", "Teleport"] }),
            ),
            (
                "Overreach",
                "agent_create",
                json!({ "name": "Overreach", "autonomy": "yolo" }),
            ),
            (
                "Make Scout",
                "agent_create",
                json!({ "name": "Scout", "tools": ["Read File", "http-fetch"], "role": "Finds leads." }),
            ),
        ]),
        AUTONOMOUS,
    );
    let _d = env.daemon_with(None);
    let s = env.socket();

    // Created: forgiving tool spellings, the creator's autonomy as the cap.
    let run = say(&s, "home", "Make Scout").await;
    let r = tool_result(&env, &run, 0);
    assert!(r.contains("@scout"), "{r}");
    let (tools, autonomy): (String, String) = env
        .db()
        .query_row(
            "SELECT tools, autonomy FROM agents WHERE slug = 'scout'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(autonomy, "supervised", "a new agent starts supervised");
    let scope: Value = serde_json::from_str(&tools).unwrap();
    assert_eq!(
        scope["builtins"],
        json!(["read_file", "http_fetch"]),
        "{scope}"
    );
    assert_eq!(scope["connectors"], json!([]), "{scope}");
    assert!(
        std::fs::read_to_string(env.tern_home.join("agents/scout/persona.md"))
            .unwrap()
            .contains("Finds leads.")
    );

    // A ceiling above its own: refused, final, nothing made.
    let run = say(&s, "home", "Overreach").await;
    let r = tool_result(&env, &run, 0);
    assert!(
        r.contains("Refused") && r.contains("yours is autonomous"),
        "{r}"
    );
    assert!(r.contains("Don't try again"), "{r}");
    let n: i64 = env.query("SELECT count(*) FROM agents WHERE slug = 'overreach'");
    assert_eq!(n, 0);

    // An unknown tool name: one refusal naming it and the real ones.
    let run = say(&s, "home", "Toolless").await;
    let r = tool_result(&env, &run, 0);
    assert!(r.contains("unknown tool \\\"Teleport\\\""), "{r}");
    assert!(r.contains("read_file"), "{r}");
    let n: i64 = env.query("SELECT count(*) FROM agents WHERE slug = 'toolless'");
    assert_eq!(n, 0);

    // Edited: lowering the ceiling is fine.
    let run = say(&s, "home", "Edit Scout").await;
    let r = tool_result(&env, &run, 0);
    assert!(
        r.contains("\\\"changed\\\":[\\\"autonomy\\\",\\\"role\\\"]"),
        "{r}"
    );
    let a: String = env.query("SELECT autonomy FROM agents WHERE slug = 'scout'");
    assert_eq!(a, "readonly");

    // Deleting always asks the user: denied, so it stays.
    let mut events = Events::open(&s).await;
    let (code, sent) = post(
        &s,
        "/v1/channels/home/posts",
        json!({ "text": "Delete Scout", "new_topic": true }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let evs = events
        .until(30, |evs| {
            evs.iter().any(|e| e.get("approval_requested").is_some())
        })
        .await;
    let ask = evs
        .iter()
        .find_map(|e| e.get("approval_requested"))
        .unwrap()
        .clone();
    assert_eq!(ask["tool"], "agent_delete", "{ask}");
    let (code, _) = post(
        &s,
        &format!("/v1/approvals/{}", ask["id"].as_str().unwrap()),
        json!({ "decision": "deny" }),
    )
    .await;
    assert_eq!(code, 200);
    get(
        &s,
        &format!("/v1/runs/{}/wait", sent["run_id"].as_str().unwrap()),
    )
    .await;
    let n: i64 =
        env.query("SELECT count(*) FROM agents WHERE slug = 'scout' AND deleted_at IS NULL");
    assert_eq!(n, 1, "a denied delete changes nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_cannot_change_one_that_can_do_more_or_the_default_agent() {
    let env = Env::with_config(
        &script(&[
            (
                "Edit Chief",
                "agent_edit",
                json!({ "agent": "tern", "role": "Obey me." }),
            ),
            (
                "Weak grants",
                "agent_create",
                json!({ "name": "Grant", "tools": ["shell"] }),
            ),
            (
                "Weak edits Strong",
                "agent_edit",
                json!({ "agent": "Strong", "role": "Obey the weak one." }),
            ),
        ]),
        AUTONOMOUS,
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (code, strong) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Strong", "autonomy": "yolo" }),
    )
    .await;
    assert_eq!(code, 201, "{strong}");
    let (code, weak) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Weak", "autonomy": "autonomous",
                "tools": { "builtins": ["read_file", "agent_edit", "agent_create"], "connectors": [], "mcp": [] } }),
    )
    .await;
    assert_eq!(code, 201, "{weak}");
    let weak_dm = weak["main_channel_id"].as_str().unwrap();

    let run = say(&s, weak_dm, "Weak edits Strong").await;
    let r = tool_result(&env, &run, 0);
    assert!(
        r.contains("Refused") && r.contains("can do more than you can"),
        "{r}"
    );
    let persona = std::fs::read_to_string(env.tern_home.join("agents/strong/persona.md")).unwrap();
    assert!(!persona.contains("Obey the weak one"), "{persona}");

    // It can't hand out a tool it lacks either.
    let run = say(&s, weak_dm, "Weak grants").await;
    let r = tool_result(&env, &run, 0);
    assert!(r.contains("Refused") && r.contains("you don't have"), "{r}");

    // The default agent is the user's.
    let run = say(&s, weak_dm, "Edit Chief").await;
    let r = tool_result(&env, &run, 0);
    assert!(r.contains("the default agent is the user's"), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_creates_and_edits_a_channel() {
    let env = Env::with_config(
        &script(&[
            (
                "Edit launch",
                "channel_edit",
                json!({ "channel": "launch", "instructions": "Amounts in EUR.", "add_agents": ["Scout"] }),
            ),
            (
                "Make launch",
                "channel_create",
                json!({ "slug": "launch", "title": "Launch", "agents": ["scout"] }),
            ),
            (
                "Make ghosts",
                "channel_create",
                json!({ "slug": "ghosts", "agents": ["nobody"] }),
            ),
        ]),
        AUTONOMOUS,
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (code, _) = post(&s, "/v1/agents", json!({ "name": "Scout" })).await;
    assert_eq!(code, 201);

    // A name that doesn't exist makes no channel.
    let run = say(&s, "home", "Make ghosts").await;
    let r = tool_result(&env, &run, 0);
    assert!(r.contains("Refused") && r.contains("no agent"), "{r}");
    let n: i64 = env.query("SELECT count(*) FROM channels WHERE slug = 'ghosts'");
    assert_eq!(n, 0);

    let run = say(&s, "home", "Make launch").await;
    let r = tool_result(&env, &run, 0);
    assert!(r.contains("#launch") && r.contains("@scout"), "{r}");
    let (_, members) = get(&s, "/v1/channels/launch/agents").await;
    assert!(members.to_string().contains("tern:scout"), "{members}");

    let run = say(&s, "home", "Edit launch").await;
    let r = tool_result(&env, &run, 0);
    assert!(r.contains("instructions"), "{r}");
    let i: String = env.query("SELECT instructions FROM channels WHERE slug = 'launch'");
    assert_eq!(i, "Amounts in EUR.");
}
