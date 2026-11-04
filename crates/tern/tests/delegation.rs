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
        { "matches": "Find Acme's pricing", "turns": [
            { "text": "Acme's Team plan is $15 a seat.", "delay_ms": 1200 },
            { "text": "Their Enterprise plan is quoted." }
        ]},
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
                { "name": "agent_spawn", "input": {
                    "label": "acme-2", "agent": "research",
                    "task": "Find Acme's pricing for Enterprise too." } },
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
    let (_, research) = post(
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

    let (_, sent) = post(
        &s,
        "/v1/channels/home/posts",
        json!({ "text": "Research Acme for me", "new_topic": true }),
    )
    .await;
    let run = sent["run_id"].as_str().unwrap().to_owned();
    // While Research works for tern, Research shows as working.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (_, list) = get(&s, "/v1/agents").await;
        let r = list
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["slug"] == "research")
            .cloned()
            .unwrap();
        if r["active_runs"].as_u64().unwrap_or(0) >= 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < end,
            "Research never counted as working: {r}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    get(&s, &format!("/v1/runs/{run}/wait")).await;
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
        "SELECT id FROM runs WHERE parent_run_id = '{run}' AND label = 'acme'"
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

    // One pointer in Research's DM for the batch of two, now done.
    let dm = research["main_channel_id"].as_str().unwrap().to_owned();
    let (_, posts) = get(&s, &format!("/v1/channels/{dm}/posts")).await;
    let pointers: Vec<&Value> = posts
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["kind"] == "delegated")
        .collect();
    assert_eq!(pointers.len(), 1, "{posts:#}");
    let ptr = pointers[0].clone();
    let text = ptr["text"].as_str().unwrap();
    assert!(
        text.starts_with("Worked for Home · \"Find Acme's pricing"),
        "{text}"
    );
    assert!(text.contains("2 tasks"), "{text}");
    assert_eq!(ptr["author_id"], "tern:research");
    // Open: tern's thread, where the sub-runs are.
    let parent_session = col(&env, "SELECT session_id FROM runs WHERE id = ?1", &run).unwrap();
    assert_eq!(ptr["session_id"], parent_session.as_str());
    // From a tainted run, so tainted.
    assert_eq!(ptr["tainted"], true, "{ptr}");
    let (_, work) = get(&s, "/v1/agents/research/work").await;
    assert_eq!(
        (
            work[0]["kind"].as_str(),
            work[0]["status"].as_str(),
            work[0]["caller"].as_str(),
            work[0]["tasks"].as_u64()
        ),
        (Some("delegated"), Some("done"), Some("Home"), Some(2)),
        "{work:#}"
    );

    // A reply under it continues the work in Research's DM, as Research,
    // starting from the brief and the reports; the next reply continues it.
    let (code, sent) = post(
        &s,
        &format!("/v1/channels/{dm}/posts"),
        json!({ "text": "And Enterprise?", "reply_to": ptr["id"] }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let reply_run = sent["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{reply_run}/wait")).await;
    let thread = sent["session_id"].as_str().unwrap().to_owned();
    assert_eq!(
        col(&env, "SELECT agent_id FROM sessions WHERE id = ?1", &thread).as_deref(),
        Some("tern:research")
    );
    assert_eq!(
        col(
            &env,
            "SELECT branched_from FROM sessions WHERE id = ?1",
            &thread
        ),
        Some(format!("post:{}", ptr["id"]))
    );
    let input: String = env
        .db()
        .query_row(
            "SELECT m.text FROM runs r JOIN messages m ON m.id = r.input_message_id WHERE r.id = ?1",
            [&reply_run],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        input.starts_with("[Continuing work you did for Home"),
        "{input}"
    );
    assert!(
        input.contains("Acme's Team plan is $15 a seat.") && input.ends_with("And Enterprise?"),
        "{input}"
    );
    let (_, again) = post(
        &s,
        &format!("/v1/channels/{dm}/posts"),
        json!({ "text": "Thanks", "reply_to": ptr["id"] }),
    )
    .await;
    assert_eq!(again["session_id"], thread.as_str(), "the same side thread");

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
