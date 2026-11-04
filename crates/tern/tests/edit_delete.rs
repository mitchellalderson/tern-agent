//! Editing and deleting what users create (DECISIONS.md "Editing and
//! deleting"): agents and channels through the API and `tern agents` /
//! `tern channels`. A deleted agent leaves its history readable; deleting
//! refuses while work is in progress and never touches the default agent,
//! Home or a DM.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use std::path::Path;

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

/// Post into a channel and wait for the run it started.
async fn say(socket: &Path, channel: &str, text: &str) {
    let (code, sent) = post(
        socket,
        &format!("/v1/channels/{channel}/posts"),
        json!({ "text": text }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(socket, &format!("/v1/runs/{run}/wait")).await;
}

async fn delete(socket: &Path, path: &str) -> (u16, Value) {
    send(socket, reqwest::Method::DELETE, path, Value::Null).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_is_deleted_only_when_confirmed_idle_and_with_its_routines_placed() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let _d = env.daemon_with(None);
    let s = env.socket();

    let (_, a) = post(&s, "/v1/agents", json!({ "name": "Research" })).await;
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();
    post(&s, "/v1/agents", json!({ "name": "Ops" })).await;
    let (_, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 9 * * *", "prompt": "morning brief", "agent": "research" }),
    )
    .await;
    let sch = sch["id"].as_str().unwrap().to_owned();
    let (_, c) = post(&s, "/v1/channels", json!({ "slug": "launch" })).await;
    let launch = c["id"].as_str().unwrap().to_owned();
    patch(&s, "/v1/channels/launch", json!({ "lead": "research" })).await;
    // A post by the agent, to find in History later.
    say(&s, &dm, "hello").await;

    // What it touches.
    let (code, imp) = get(&s, "/v1/agents/research/impact").await;
    assert_eq!(code, 200, "{imp}");
    assert_eq!(imp["routines"][0]["label"], "morning brief");
    assert_eq!(imp["led_channels"][0]["label"], "launch");
    assert_eq!(imp["blockers"], json!([]));

    // Not without confirm; never the default agent.
    assert_eq!(delete(&s, "/v1/agents/research").await.0, 400);
    let (code, body) = delete(&s, "/v1/agents/tern?confirm=true").await;
    assert_eq!(code, 409, "{body}");
    // Routines must be placed.
    let (code, body) = delete(&s, "/v1/agents/research?confirm=true").await;
    assert_eq!(code, 409, "{body}");
    assert!(body.to_string().contains("routine"), "{body}");
    assert_eq!(
        delete(
            &s,
            "/v1/agents/research?confirm=true&delete_routines=true&reassign_to=ops"
        )
        .await
        .0,
        409,
        "one or the other"
    );
    assert_eq!(
        delete(&s, "/v1/agents/research?confirm=true&reassign_to=nobody")
            .await
            .0,
        409
    );

    // A run in progress blocks it.
    let sess: String = env.query(&format!(
        "SELECT id FROM sessions WHERE channel_id = '{dm}' LIMIT 1"
    ));
    env.db()
        .execute(
            "INSERT INTO runs (id, session_id, status, trigger, started_at, updated_at)
             VALUES ('busy', ?1, 'running', 'user', 0, 0)",
            [&sess],
        )
        .unwrap();
    let (code, body) = delete(&s, "/v1/agents/research?confirm=true&reassign_to=ops").await;
    assert_eq!(code, 409, "{body}");
    assert!(body.to_string().contains("in progress"), "{body}");
    env.db()
        .execute("UPDATE runs SET status = 'completed' WHERE id = 'busy'", [])
        .unwrap();

    // Delete, handing routines and led channels to Ops.
    let (code, body) = delete(&s, "/v1/agents/research?confirm=true&reassign_to=ops").await;
    assert_eq!(code, 200, "{body}");
    let (_, list) = get(&s, "/v1/agents").await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|a| a["slug"] != "research")
    );
    assert_eq!(get(&s, "/v1/agents/research/persona").await.0, 404);
    // Listed with `deleted=1`, so history can name it.
    let (_, all) = get(&s, "/v1/agents?deleted=1").await;
    let gone = all
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "tern:research")
        .unwrap();
    assert_eq!(
        (gone["name"].as_str(), gone["deleted"].as_bool()),
        (Some("Research"), Some(true))
    );
    // Its routine and channel went to Ops; its DM is hidden but readable.
    let (_, mine) = get(&s, "/v1/schedules?agent=ops").await;
    assert_eq!(mine[0]["id"], sch.as_str());
    let (_, ch) = get(&s, "/v1/channels").await;
    let launch_row = ch
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == launch.as_str())
        .unwrap();
    assert_eq!(launch_row["agent_id"], "tern:ops");
    assert!(
        ch.as_array()
            .unwrap()
            .iter()
            .all(|c| c["id"] != dm.as_str())
    );
    // Its DM is hidden, but what was said in it is still readable.
    let sess: String = env.query(&format!(
        "SELECT id FROM sessions WHERE channel_id = '{dm}' LIMIT 1"
    ));
    let (code, stream) = get(&s, &format!("/v1/channels/{dm}/stream")).await;
    assert_eq!(code, 200, "{stream}");
    assert!(!stream["items"].as_array().unwrap().is_empty(), "{stream}");
    assert_eq!(get(&s, &format!("/v1/sessions/{sess}")).await.0, 200);
    // Its persona file is kept, out of the way.
    assert!(!env.tern_home.join("agents/research").exists());
    assert!(
        std::fs::read_dir(env.tern_home.join("agents/.deleted"))
            .unwrap()
            .any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("research-"))
    );
    // Continuing its old conversation is refused with a clear reason.
    let (code, body) = post(&s, "/v1/runs", json!({ "text": "hi", "session_id": sess })).await;
    assert_ne!(code, 200);
    assert!(body.to_string().contains("was deleted"), "{body}");

    // The name can be used again, with a fresh id.
    let (code, again) = post(&s, "/v1/agents", json!({ "name": "Research" })).await;
    assert_eq!(code, 201, "{again}");
    assert_eq!(again["id"], "tern:research-2");
    assert_eq!(again["slug"], "research");
    assert_eq!(delete(&s, "/v1/agents/research?confirm=true").await.0, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channel_is_renamed_and_deleted_keeping_its_conversations() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let _d = env.daemon_with(None);
    let s = env.socket();

    let (_, a) = post(&s, "/v1/agents", json!({ "name": "Research" })).await;
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();
    let (_, c) = post(
        &s,
        "/v1/channels",
        json!({ "slug": "launch", "title": "Launch" }),
    )
    .await;
    let id = c["id"].as_str().unwrap().to_owned();
    post(&s, "/v1/channels", json!({ "slug": "other" })).await;
    say(&s, &id, "plan it").await;
    let (_, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 9 * * *", "prompt": "brief", "agent": "research", "channel": "launch" }),
    )
    .await;
    let sess: String = env.query(&format!(
        "SELECT id FROM sessions WHERE channel_id = '{id}' LIMIT 1"
    ));

    // Rename: the id stays, and clashes and bad names are refused.
    let (code, r) = patch(&s, "/v1/channels/launch", json!({ "slug": "launches" })).await;
    assert_eq!(code, 200, "{r}");
    assert_eq!(
        (r["id"].as_str(), r["slug"].as_str()),
        (Some(id.as_str()), Some("launches"))
    );
    assert_eq!(
        patch(&s, "/v1/channels/launches", json!({ "slug": "other" }))
            .await
            .0,
        409
    );
    assert_eq!(
        patch(&s, "/v1/channels/launches", json!({ "slug": "Bad Slug" }))
            .await
            .0,
        409
    );
    assert_eq!(
        patch(&s, &format!("/v1/channels/{dm}"), json!({ "slug": "x" }))
            .await
            .0,
        409
    );
    assert_eq!(
        patch(&s, "/v1/channels/home", json!({ "slug": "x" }))
            .await
            .0,
        409
    );

    // Impact, then the confirmations and refusals.
    let (_, imp) = get(&s, "/v1/channels/launches/impact").await;
    assert!(imp["posts"].as_u64().unwrap() >= 1, "{imp}");
    assert_eq!(imp["routines"][0]["label"], "brief");
    assert_eq!(delete(&s, "/v1/channels/launches").await.0, 400);
    for protected in ["home", dm.as_str()] {
        let (code, body) = delete(&s, &format!("/v1/channels/{protected}?confirm=true")).await;
        assert_eq!(code, 409, "{body}");
    }
    env.db()
        .execute(
            "INSERT INTO runs (id, session_id, status, trigger, started_at, updated_at)
             VALUES ('busy', ?1, 'running', 'user', 0, 0)",
            [&sess],
        )
        .unwrap();
    assert_eq!(
        delete(&s, "/v1/channels/launches?confirm=true").await.0,
        409
    );
    env.db()
        .execute("UPDATE runs SET status = 'completed' WHERE id = 'busy'", [])
        .unwrap();

    assert_eq!(
        delete(&s, "/v1/channels/launches?confirm=true").await.0,
        200
    );
    let (_, list) = get(&s, "/v1/channels?archived=1").await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|c| c["id"] != id.as_str())
    );
    assert_eq!(get(&s, "/v1/channels/launches/posts").await.0, 404);
    // The conversation is still in History.
    let (code, session) = get(&s, &format!("/v1/sessions/{sess}")).await;
    assert_eq!(code, 200, "{session}");
    // The routine posts to its agent's DM now.
    let (_, mine) = get(&s, "/v1/schedules?agent=research").await;
    assert_eq!(mine[0]["id"], sch["id"]);
    assert_eq!(mine[0]["channel_id"], dm.as_str());
    // The name is free again.
    assert_eq!(
        post(&s, "/v1/channels", json!({ "slug": "launches" }))
            .await
            .0,
        201
    );
}

#[test]
fn the_cli_renames_and_deletes_agents_and_channels_with_confirmation() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let run = |args: &[&str]| {
        let o = env.cmd(args).output().unwrap();
        (
            o.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        )
    };

    assert!(run(&["agents", "new", "Research"]).0);
    assert!(run(&["channels", "new", "launch", "--title", "Launch"]).0);

    // Rename.
    let (ok, out) = run(&["agents", "rename", "research", "Scout", "--slug", "scout"]);
    assert!(ok, "{out}");
    assert!(run(&["agents"]).1.contains("@scout"));
    assert!(!run(&["agents", "rename", "tern", "--slug", "x"]).0);
    let (ok, out) = run(&[
        "channels", "edit", "launch", "--slug", "launches", "--title", "Launches",
    ]);
    assert!(ok, "{out}");
    assert!(run(&["channels"]).1.contains("#launches"));

    // Deleting needs --yes without a terminal, and the default agent, Home
    // and DMs can't go.
    let (ok, out) = run(&["agents", "delete", "scout"]);
    assert!(!ok && out.contains("--yes"), "{out}");
    assert!(run(&["agents"]).1.contains("@scout"));
    assert!(!run(&["agents", "delete", "tern", "--yes"]).0);
    assert!(!run(&["channels", "delete", "home", "--yes"]).0);
    assert!(!run(&["channels", "delete", "dm-scout", "--yes"]).0);
    let (ok, out) = run(&["channels", "delete", "launches"]);
    assert!(!ok && out.contains("--yes"), "{out}");

    let (ok, out) = run(&["channels", "delete", "launches", "--yes"]);
    assert!(ok, "{out}");
    assert!(!run(&["channels"]).1.contains("#launches"));
    let (ok, out) = run(&["agents", "delete", "scout", "--yes"]);
    assert!(ok, "{out}");
    assert!(!run(&["agents"]).1.contains("@scout"));
    assert!(
        run(&["agents", "delete", "scout", "--yes"])
            .1
            .contains("no agent")
    );
}
