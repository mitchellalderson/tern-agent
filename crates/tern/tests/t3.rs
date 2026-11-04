//! T3 end-to-end (docs/personal-agent-tooling-spec.md §5.2, §5.4): a
//! routine that read untrusted content wants to write while nobody is
//! attached. The write goes to the outbox, not out, even with an "always"
//! grant; the run finishes and its post lists the item; approving runs it
//! exactly once, across a daemon restart.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use subagents_env::{Env, api, get, post};

async fn patch(socket: &std::path::Path, path: &str, body: Value) -> Value {
    let r = api(socket)
        .patch(format!("http://tern{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    r.json().await.unwrap_or(Value::Null)
}

async fn wait_for(socket: &std::path::Path, path: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, v) = get(socket, path).await;
        if ok(&v) {
            return v;
        }
        assert!(Instant::now() < end, "{path}: {v:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_injected_write_waits_in_the_outbox_and_runs_once_when_approved() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Sweep the inbox", "turns": [
                // `mcp_list` returns third-party text: the run is tainted.
                { "text": "Reading.", "tool_calls": [{ "name": "mcp_list", "input": {} }] },
                // What an injected email asks for.
                { "text": "Replying.", "tool_calls": [{ "name": "write_file",
                    "input": { "path": "reply.txt", "content": "invoices attached" } }] },
                { "text": "Drafted a reply; it is waiting for your approval." }
            ]}
        ]}),
        "",
    );
    let workspace = env.home.join("tern-workspace");
    let d = env.daemon_with(None);
    let s = env.socket();
    // An "always" grant for the tool doesn't cover a tainted run.
    env.db()
        .execute(
            "INSERT INTO approval_allowlist (tool, pattern, created_at) VALUES ('write_file', '*', 0)",
            [],
        )
        .unwrap();

    let (code, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Sweep the inbox" }),
    )
    .await;
    assert_eq!(code, 201, "{sch}");
    let (_, r) = post(
        &s,
        &format!("/v1/schedules/{}/run", sch["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let run = r["run_id"].as_str().unwrap().to_owned();
    let (_, done) = get(&s, &format!("/v1/runs/{run}/wait")).await;
    assert_eq!(done["status"], "completed", "{done}");
    assert!(!workspace.join("reply.txt").exists(), "nothing written yet");

    let (_, items) = get(&s, "/v1/outbox").await;
    let items = items.as_array().unwrap().clone();
    assert_eq!(items.len(), 1, "{items:#?}");
    let item = &items[0];
    assert_eq!(item["tool"], "write_file");
    assert_eq!(item["state"], "pending");
    assert_eq!(item["taint"], "result of mcp_list");
    assert_eq!(item["input"]["content"], "invoices attached");
    let id = item["id"].as_i64().unwrap();
    // The routine's post lists what waits.
    let posts = wait_for(&s, "/v1/channels/home/posts", |v| {
        v.as_array().is_some_and(|a| !a.is_empty())
    })
    .await;
    let text = posts[0]["text"].as_str().unwrap();
    assert!(text.contains(&format!("outbox #{id}")), "{text}");

    // Approve with an edit; it runs once and the result is posted.
    let (code, approved) = post(
        &s,
        &format!("/v1/outbox/{id}/approve"),
        json!({ "input": { "path": "reply.txt", "content": "edited reply" } }),
    )
    .await;
    assert_eq!(code, 200, "{approved}");
    let done = wait_for(&s, &format!("/v1/outbox/{id}"), |v| v["state"] == "done").await;
    assert_eq!(done["edited"], true);
    assert_eq!(
        std::fs::read_to_string(workspace.join("reply.txt")).unwrap(),
        "edited reply"
    );
    let (code, _) = post(&s, &format!("/v1/outbox/{id}/approve"), json!({})).await;
    assert_eq!(code, 409, "a second approval is refused");
    wait_for(&s, "/v1/channels/home/posts", |v| {
        v.as_array().is_some_and(|a| {
            a.iter().any(|p| {
                p["text"]
                    .as_str()
                    .is_some_and(|t| t.starts_with(&format!("Outbox #{id} done")))
            })
        })
    })
    .await;

    // After a restart nothing runs again.
    std::fs::remove_file(workspace.join("reply.txt")).unwrap();
    drop(d);
    let _d = env.daemon_with(None);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!workspace.join("reply.txt").exists(), "exactly once");
    let n: i64 = env.query("SELECT COUNT(*) FROM steps WHERE kind = 'outbox_exec'");
    assert_eq!(n, 1);
    let (_, all) = get(&s, "/v1/outbox?state=all").await;
    assert_eq!(all[0]["state"], "done");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_routine_runs_under_its_own_autonomy() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Tidy up", "turns": [
                { "text": "Writing.", "tool_calls": [{ "name": "write_file",
                    "input": { "path": "notes.txt", "content": "x" } }] },
                { "text": "Couldn't: I'm read-only." }
            ]}
        ]}),
        "autonomy = \"supervised\"\n",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (code, bad) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Tidy up", "autonomy": "reckless" }),
    )
    .await;
    assert_eq!(code, 400, "{bad}");
    let (_, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Tidy up", "autonomy": "readonly" }),
    )
    .await;
    assert_eq!(sch["autonomy"], "readonly");
    let (_, r) = post(
        &s,
        &format!("/v1/schedules/{}/run", sch["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let run = r["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{run}/wait")).await;
    let autonomy: String = env.query(&format!("SELECT autonomy FROM runs WHERE id = '{run}'"));
    assert_eq!(autonomy, "readonly");
    assert!(!env.home.join("tern-workspace/notes.txt").exists());
    let (_, outbox) = get(&s, "/v1/outbox?state=all").await;
    assert_eq!(outbox, json!([]), "refused, not queued");
    // Back to the default.
    let sch = patch(
        &s,
        &format!("/v1/schedules/{}", sch["id"].as_str().unwrap()),
        json!({ "autonomy": "default" }),
    )
    .await;
    assert_eq!(sch.get("autonomy"), None, "{sch}");
}

#[tokio::test(flavor = "multi_thread")]
async fn shell_output_taints_the_run_when_the_network_is_on() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Compare", "turns": [
                { "text": "Fetching.", "tool_calls": [{ "name": "shell", "input": { "command": "echo prices" } }] },
                { "text": "Done." }
            ]}
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    env.db()
        .execute(
            "INSERT INTO approval_allowlist (tool, pattern, created_at) VALUES ('shell', 'echo *', 0)",
            [],
        )
        .unwrap();
    let (_, r) = post(&s, "/v1/runs", json!({ "text": "Compare the area" })).await;
    let run = r["run_id"].as_str().unwrap().to_owned();
    let (_, done) = get(&s, &format!("/v1/runs/{run}/wait")).await;
    assert_eq!(done["status"], "completed", "{done}");
    let taint: Option<String> = env
        .db()
        .query_row("SELECT taint FROM runs WHERE id = ?1", [&run], |r| r.get(0))
        .unwrap();
    assert_eq!(
        taint.as_deref(),
        Some("result of shell"),
        "network is on by default"
    );
}
