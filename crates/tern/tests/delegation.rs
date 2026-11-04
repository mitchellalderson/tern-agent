//! Delegating to another agent profile (docs/agent-profiles-design.md
//! §6.1, AP3's delegation pulled forward): `agent_spawn {agent}` runs the
//! child as that profile, still as the caller's sub-agent, within the
//! caller's `delegates`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use serde_json::{Value, json};
use subagents_env::{Env, api, get, post};

async fn say(socket: &std::path::Path, channel: &str, text: &str) -> String {
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

fn col(env: &Env, sql: &str, id: &str) -> Option<String> {
    env.db().query_row(sql, [id], |r| r.get(0)).unwrap()
}

/// The first tool result of `run`.
fn first_tool(env: &Env, run: &str) -> String {
    env.db()
        .query_row(
            "SELECT output_json FROM steps WHERE run_id = ?1 AND kind = 'tool' ORDER BY idx LIMIT 1",
            [run],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn tern_hands_research_to_the_research_agent() {
    let env = Env::new(&json!({ "conversations": [
        // The child's brief comes first: it is also in the parent's transcript;
        // a new topic's message carries the last one's, so "Again" before it.
        { "matches": "Find Acme's pricing", "turns": [{ "text": "Acme's Team plan is $15 a seat." }] },
        { "matches": "Again, research", "turns": [
            { "text": "", "tool_calls": [{ "name": "agent_spawn", "input": {
                "label": "y", "agent": "research", "task": "Look this up too." } }] },
            { "text": "Couldn't." }
        ]},
        { "matches": "Research Acme", "turns": [
            // Reading MCP servers taints tern's run first.
            { "text": "", "tool_calls": [{ "name": "mcp_list", "input": {} }] },
            { "text": "", "tool_calls": [
                { "name": "agent_spawn", "input": {
                    "label": "acme", "agent": "research",
                    "task": "Find Acme's pricing and cite the page." } },
                { "name": "agent_wait", "input": {} }] },
            { "text": "Research says $15 a seat." }
        ]},
        { "matches": "Inbox, try", "turns": [
            { "text": "", "tool_calls": [{ "name": "agent_spawn", "input": {
                "label": "x", "agent": "research", "task": "Look this up." } }] },
            { "text": "Couldn't." }
        ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    post(
        &s,
        "/v1/agents",
        json!({ "name": "Research", "template": "research", "model": "mock:research" }),
    )
    .await;
    let (_, inbox) = post(&s, "/v1/agents", json!({ "name": "Inbox" })).await;

    // The tool says how, and tern's prompt says to whom and when.
    let (_, tools) = get(&s, "/v1/tools").await;
    let spawn = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "agent_spawn")
        .unwrap();
    assert!(
        spawn["input_schema"]["properties"]["agent"].is_object(),
        "{spawn}"
    );
    assert!(spawn["description"].as_str().unwrap().contains("`agent`"));

    let run = say(&s, "home", "Research Acme for me").await;
    let sys = col(&env, "SELECT system_prompt FROM runs WHERE id = ?1", &run).unwrap();
    assert!(sys.contains("You can hand work to these agents"), "{sys}");
    assert!(
        sys.contains("`research` · Research · use for: find things out."),
        "{sys}"
    );
    assert!(
        sys.contains("instead of spawning a generic sub-agent"),
        "{sys}"
    );

    // The child ran as Research, under tern.
    let child: String = env.query(&format!(
        "SELECT id FROM runs WHERE parent_run_id = '{run}'"
    ));
    let csys = col(&env, "SELECT system_prompt FROM runs WHERE id = ?1", &child).unwrap();
    assert!(csys.starts_with("You are Research"), "{csys}");
    assert!(csys.contains("## You are working for another agent"));
    assert!(csys.contains("tern, another of the user's agents"));
    assert_eq!(
        col(&env, "SELECT model FROM runs WHERE id = ?1", &child).as_deref(),
        Some("mock:research")
    );
    let scope: Vec<String> = serde_json::from_str(
        &col(&env, "SELECT tool_scope FROM runs WHERE id = ?1", &child).unwrap(),
    )
    .unwrap();
    assert!(scope.contains(&"read_file".to_owned()), "{scope:?}");
    assert!(
        !scope.contains(&"write_file".to_owned()),
        "Research is read-only: {scope:?}"
    );
    assert_eq!(
        col(&env, "SELECT memory_scope FROM runs WHERE id = ?1", &child).as_deref(),
        Some("private")
    );
    let session = col(&env, "SELECT session_id FROM runs WHERE id = ?1", &child).unwrap();
    assert_eq!(
        col(
            &env,
            "SELECT agent_id FROM sessions WHERE id = ?1",
            &session
        )
        .as_deref(),
        Some("tern:research")
    );
    // In tern's channel, and tainted like any child of a tainted run.
    assert_eq!(
        col(
            &env,
            "SELECT channel_id FROM sessions WHERE id = ?1",
            &session
        )
        .as_deref(),
        Some("home")
    );
    assert_eq!(
        col(&env, "SELECT taint FROM runs WHERE id = ?1", &child).as_deref(),
        Some("spawned by a tainted run")
    );
    // Front ends see which agent it is; the report came back.
    let (_, kids) = get(&s, &format!("/v1/runs/{run}/children")).await;
    assert_eq!(kids[0]["agent_id"], "tern:research", "{kids}");
    let (_, out) = get(&s, &format!("/v1/runs/{run}/wait")).await;
    assert_eq!(out["status"], "completed", "{out}");

    // Inbox has no delegates by default: refused on the host side.
    let run = say(
        &s,
        inbox["main_channel_id"].as_str().unwrap(),
        "Inbox, try research",
    )
    .await;
    let out = first_tool(&env, &run);
    assert!(out.contains("can't hand work to Research"), "{out}");
    let n: i64 = env.query(&format!(
        "SELECT count(*) FROM runs WHERE parent_run_id = '{run}'"
    ));
    assert_eq!(n, 0);

    // tern restricted to Inbox can't reach Research either.
    let r = api(&s)
        .patch("http://tern/v1/agents/tern")
        .json(&json!({ "delegates": ["inbox"] }))
        .send()
        .await
        .unwrap();
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["delegates"], json!(["tern:inbox"]), "{body}");
    let run = say(&s, "home", "Again, research this").await;
    let out = first_tool(&env, &run);
    assert!(out.contains("isn't in tern's delegates"), "{out}");
}
