//! Workspaces end to end (docs/alpha-workspaces.md W0/W1): the API, the
//! resolution order, the folder a run really uses, refusals, missing
//! folders, sub-agent narrowing and the unattended-shell gate.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use subagents_env::{Env, Events, api, get, post};

async fn send(s: &Path, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let c = api(s);
    let url = format!("http://tern{path}");
    let req = match method {
        "PUT" => c.put(url),
        "PATCH" => c.patch(url),
        "DELETE" => c.delete(url),
        _ => c.get(url),
    };
    let req = match body {
        Some(b) => req.json(&b),
        None => req,
    };
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

fn mkdir(env: &Env, name: &str) -> PathBuf {
    let p = env.home.join("projects").join(name);
    std::fs::create_dir_all(&p).unwrap();
    std::fs::canonicalize(p).unwrap()
}

async fn say(s: &Path, channel: &str, text: &str, ws: Option<&Path>) -> Value {
    let mut body = json!({ "text": text });
    if let Some(w) = ws {
        body["workspace"] = json!(w);
    }
    let (code, sent) = post(s, &format!("/v1/channels/{channel}/posts"), body).await;
    assert_eq!(code, 200, "{sent}");
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(s, &format!("/v1/runs/{run}/wait")).await;
    sent
}

async fn wait_file(p: &Path) -> String {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(t) = std::fs::read_to_string(p) {
            return t;
        }
        if Instant::now() >= end {
            let home = p.ancestors().nth(3).unwrap();
            let mut seen = Vec::new();
            let mut stack = vec![home.to_path_buf()];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    let path = e.path();
                    if path.is_dir() && !path.ends_with(".tern") {
                        stack.push(path);
                    } else if !path.to_string_lossy().contains("/.tern/") {
                        seen.push(path.display().to_string());
                    }
                }
            }
            panic!("{} never appeared; files: {seen:#?}", p.display());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn writer(prompt: &str, path: &str, content: &str) -> Value {
    json!({ "matches": prompt, "turns": [
        { "text": "", "tool_calls": [{ "name": "write_file",
            "input": { "path": path, "content": content } }] },
        { "text": "Done." }
    ]})
}

#[tokio::test(flavor = "multi_thread")]
async fn the_api_lists_the_default_validates_and_adds() {
    let env = Env::new(&json!({ "turns": [{ "text": "ok" }] }));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, health) = get(&s, "/v1/health").await;
    assert!(
        health["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "workspaces"),
        "{health}"
    );
    let (_, list) = get(&s, "/v1/workspaces").await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["id"], "default");
    assert_eq!(list[0]["is_default"], true);
    assert_eq!(list[0]["status"], "ok");
    assert!(
        ["sandboxed", "read_only", "unverified", "unbounded"]
            .contains(&list[0]["shell_state"].as_str().unwrap()),
        "{}",
        list[0]
    );

    // Validate returns the words and persists nothing.
    let (code, v) = post(&s, "/v1/workspaces/validate", json!({ "path": "/" })).await;
    assert_eq!(code, 200);
    assert!(
        v["errors"][0].as_str().unwrap().contains("whole disk"),
        "{v}"
    );
    let home = env.home.to_str().unwrap();
    let (_, v) = post(&s, "/v1/workspaces/validate", json!({ "path": home })).await;
    assert!(!v["errors"].as_array().unwrap().is_empty(), "{v}");
    let proj = mkdir(&env, "alpha");
    let (_, v) = post(&s, "/v1/workspaces/validate", json!({ "path": proj })).await;
    assert_eq!(v["errors"], json!([]), "{v}");
    assert_eq!(v["canonical"], json!(proj));
    assert_eq!(
        get(&s, "/v1/workspaces").await.1.as_array().unwrap().len(),
        1
    );

    // A refused folder: 422 with the verdict. A good one: 201, then 200 again.
    let (code, r) = post(&s, "/v1/workspaces", json!({ "path": "/etc" })).await;
    assert_eq!(code, 422, "{r}");
    assert!(
        r["verdict"]["errors"]
            .as_array()
            .is_some_and(|e| !e.is_empty())
    );
    let (code, r) = post(
        &s,
        "/v1/workspaces",
        json!({ "path": proj, "name": "Alpha" }),
    )
    .await;
    assert_eq!(code, 201, "{r}");
    let id = r["workspace"]["id"].as_str().unwrap().to_owned();
    assert_eq!(r["workspace"]["name"], "Alpha");
    let (code, again) = post(&s, "/v1/workspaces", json!({ "path": proj })).await;
    assert_eq!(code, 200);
    assert_eq!(again["workspace"]["id"], json!(id));
    let (_, one) = get(&s, &format!("/v1/workspaces/{id}")).await;
    assert_eq!(one["path"], json!(proj));

    // Read-only extra folders: add, list, remove; a secret folder is refused.
    let docs = mkdir(&env, "docs");
    let (code, r) = post(
        &s,
        &format!("/v1/workspaces/{id}/roots"),
        json!({ "path": docs }),
    )
    .await;
    assert_eq!(code, 201, "{r}");
    let file = docs.join("notes.md");
    std::fs::write(&file, "n").unwrap();
    let (code, r) = post(
        &s,
        &format!("/v1/workspaces/{id}/roots"),
        json!({ "path": file }),
    )
    .await;
    assert_eq!(code, 201, "{r}");
    assert_eq!(r["root"]["is_file"], true);
    let ssh = env.home.join(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    let (code, _) = post(
        &s,
        &format!("/v1/workspaces/{id}/roots"),
        json!({ "path": ssh }),
    )
    .await;
    assert_eq!(code, 422);
    let (_, roots) = get(&s, &format!("/v1/workspaces/{id}/roots")).await;
    assert_eq!(roots.as_array().unwrap().len(), 2);
    let (code, _) = send(
        &s,
        "DELETE",
        &format!("/v1/workspaces/{id}/roots?path={}", docs.display()),
        None,
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(
        get(&s, &format!("/v1/workspaces/{id}/roots"))
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Removing: refused while used, fine after.
    let (_, ch) = post(
        &s,
        "/v1/channels",
        json!({ "slug": "launch", "workspace": id }),
    )
    .await;
    let ch_id = ch["id"].as_str().unwrap().to_owned();
    let (code, r) = send(&s, "DELETE", &format!("/v1/workspaces/{id}"), None).await;
    assert_eq!(code, 409, "{r}");
    assert_eq!(r["impact"]["channels"], json!(["launch"]));
    let (code, _) = send(
        &s,
        "DELETE",
        &format!("/v1/workspaces/{id}?reassign_to=default"),
        None,
    )
    .await;
    assert_eq!(code, 200);
    let (_, b) = get(&s, &format!("/v1/channels/{ch_id}/workspace")).await;
    assert_eq!(b["effective"]["id"], "default", "{b}");
    assert_eq!(
        send(&s, "DELETE", "/v1/workspaces/default", None).await.0,
        400
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_uses_the_resolved_folder_session_over_channel_over_agent_over_default() {
    // Newest first: a conversation's transcript still holds its earlier
    // messages, and the mock answers with the first script that matches.
    let env = Env::new(&json!({ "conversations": [
        // One conversation, two runs: its second run is turns 3 and 4.
        { "matches": "From c3 first", "turns": [
            { "text": "", "tool_calls": [{ "name": "write_file",
                "input": { "path": "who.txt", "content": "c3" } }] },
            { "text": "Done." },
            { "text": "", "tool_calls": [{ "name": "write_file",
                "input": { "path": "who.txt", "content": "session" } }] },
            { "text": "Done." }
        ]},
        writer("From channel", "who.txt", "channel"),
        writer("From agent", "who.txt", "agent"),
        writer("From default", "who.txt", "default"),
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (a, c, sess) = (
        mkdir(&env, "agent"),
        mkdir(&env, "channel"),
        mkdir(&env, "session"),
    );
    let default = env.home.join("tern-workspace");
    for slug in ["c0", "c1", "c2", "c3"] {
        post(&s, "/v1/channels", json!({ "slug": slug })).await;
    }

    // Nothing bound: the default, exactly as before workspaces.
    say(&s, "c0", "From default", None).await;
    assert_eq!(wait_file(&default.join("who.txt")).await, "default");
    let (_, b) = get(&s, "/v1/channels/c0/workspace").await;
    assert_eq!(
        (b["source"].as_str(), b["workspace"].is_null()),
        (Some("default"), true)
    );

    // The agent's default applies where nothing is more specific.
    let (code, _) = send(
        &s,
        "PUT",
        "/v1/agents/tern/workspace",
        Some(json!({ "workspace": a })),
    )
    .await;
    assert_eq!(code, 200);
    let (_, b) = get(&s, "/v1/channels/c1/workspace").await;
    assert_eq!(b["source"], "agent", "{b}");
    say(&s, "c1", "From agent", None).await;
    assert_eq!(wait_file(&a.join("who.txt")).await, "agent");

    // The channel beats the agent.
    let (code, b) = send(
        &s,
        "PUT",
        "/v1/channels/c2/workspace",
        Some(json!({ "workspace": c })),
    )
    .await;
    assert_eq!(code, 200, "{b}");
    assert_eq!(b["source"], "channel");
    say(&s, "c2", "From channel", None).await;
    assert_eq!(wait_file(&c.join("who.txt")).await, "channel");

    // The conversation beats the channel.
    send(
        &s,
        "PUT",
        "/v1/channels/c3/workspace",
        Some(json!({ "workspace": c })),
    )
    .await;
    let sent = say(&s, "c3", "From c3 first", None).await;
    let sid = sent["session_id"].as_str().unwrap().to_owned();
    let (code, b) = send(
        &s,
        "PUT",
        &format!("/v1/sessions/{sid}/workspace"),
        Some(json!({ "workspace": sess })),
    )
    .await;
    assert_eq!(code, 200, "{b}");
    assert_eq!(b["source"], "session");
    let sent = say(&s, "c3", "And again", None).await;
    assert_eq!(sent["session_id"], json!(sid));
    assert_eq!(wait_file(&sess.join("who.txt")).await, "session");

    // The folder is frozen on the run row.
    let root: String = env
        .db()
        .query_row(
            "SELECT workspace_root FROM runs WHERE id = ?1",
            [sent["run_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(root, sess.to_str().unwrap());

    // Clearing goes back down the order.
    let (_, b) = send(&s, "DELETE", &format!("/v1/sessions/{sid}/workspace"), None).await;
    assert_eq!(b["source"], "channel", "{b}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_run_in_a_new_channel_or_home_topic_already_uses_the_chosen_folder() {
    let env = Env::new(&json!({ "conversations": [
        writer("Second in home", "second.txt", "home2"),
        writer("First in home", "first.txt", "home"),
        writer("First in channel", "first.txt", "channel"),
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (c, h1, h2) = (
        mkdir(&env, "newchan"),
        mkdir(&env, "home1"),
        mkdir(&env, "home2"),
    );

    // Channel created with a folder: its first send uses it.
    let (code, ch) = post(
        &s,
        "/v1/channels",
        json!({ "slug": "fresh", "workspace": c }),
    )
    .await;
    assert_eq!(code, 201, "{ch}");
    say(&s, "fresh", "First in channel", None).await;
    assert_eq!(wait_file(&c.join("first.txt")).await, "channel");
    // A refused folder makes no channel.
    let (code, _) = post(
        &s,
        "/v1/channels",
        json!({ "slug": "bad", "workspace": "/etc" }),
    )
    .await;
    assert_eq!(code, 422);
    assert_eq!(get(&s, "/v1/channels/bad/posts").await.0, 404);

    // Home: the very first send with a folder binds the topic before it runs.
    let sent = say(&s, "home", "First in home", Some(&h1)).await;
    assert_eq!(wait_file(&h1.join("first.txt")).await, "home");
    let sid = sent["session_id"].as_str().unwrap().to_owned();
    let (_, b) = get(&s, &format!("/v1/sessions/{sid}/workspace")).await;
    assert_eq!(b["effective"]["path"], json!(h1));
    // A different folder in Home starts a new topic in it.
    let (code, sent2) = post(
        &s,
        "/v1/channels/home/posts",
        json!({ "text": "Second in home", "workspace": h2 }),
    )
    .await;
    assert_eq!(code, 200, "{sent2}");
    assert_eq!(sent2["new_segment"], true, "{sent2}");
    assert_ne!(sent2["session_id"], json!(sid));
    assert_eq!(wait_file(&h2.join("second.txt")).await, "home2");
    // The same folder again continues the topic.
    let (_, b) = get(
        &s,
        &format!(
            "/v1/sessions/{}/workspace",
            sent2["session_id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(b["effective"]["path"], json!(h2));

    // POST /v1/runs takes it too.
    let (code, r) = post(&s, "/v1/runs", json!({ "text": "x", "workspace": "/etc" })).await;
    assert_eq!(code, 400, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn files_outside_the_folder_are_refused_logged_and_never_asked() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Escape", "turns": [
            { "text": "", "tool_calls": [
                { "name": "write_file", "input": { "path": "/tmp/tern-ws-escape.txt", "content": "x" } },
                { "name": "write_file", "input": { "path": "../escape.txt", "content": "x" } },
                { "name": "read_file", "input": { "path": "/etc/hosts" } },
                { "name": "write_file", "input": { "path": "inside.txt", "content": "ok" } }
            ] },
            { "text": "Done." }
        ]}
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let ws = mkdir(&env, "boxed");
    let (_, ch) = post(
        &s,
        "/v1/channels",
        json!({ "slug": "boxed", "workspace": ws }),
    )
    .await;
    let id = ch["id"].as_str().unwrap().to_owned();
    say(&s, &id, "Escape", None).await;
    assert_eq!(wait_file(&ws.join("inside.txt")).await, "ok");
    assert!(!Path::new("/tmp/tern-ws-escape.txt").exists());
    assert!(!ws.parent().unwrap().join("escape.txt").exists());
    let (_, ledger) = get(&s, "/v1/ledger?kind=workspace_refused").await;
    let entries = ledger.as_array().unwrap();
    assert!(entries.len() >= 3, "{ledger:#}");
    // Never an approval: nothing waited on the user.
    assert_eq!(get(&s, "/v1/approvals").await.1, json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_folder_stops_the_run_and_is_never_recreated() {
    let env = Env::new(&json!({ "conversations": [
        writer("Again", "back.txt", "back"),
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let ws = mkdir(&env, "unplugged");
    let (_, ch) = post(
        &s,
        "/v1/channels",
        json!({ "slug": "drive", "workspace": ws }),
    )
    .await;
    let id = ch["id"].as_str().unwrap().to_owned();
    let (_, w) = get(&s, &format!("/v1/channels/{id}/workspace")).await;
    let wid = w["workspace"]["id"].as_str().unwrap().to_owned();
    let old_id = dir_id(&ws);
    std::fs::remove_dir_all(&ws).unwrap();
    let (code, r) = post(
        &s,
        &format!("/v1/channels/{id}/posts"),
        json!({ "text": "Again" }),
    )
    .await;
    assert_eq!(code, 400, "{r}");
    assert!(r["error"].as_str().unwrap().contains("unavailable"), "{r}");
    assert!(!ws.exists(), "a user-chosen folder was recreated");
    let (_, w) = get(&s, &format!("/v1/workspaces/{wid}")).await;
    assert_eq!(w["status"], "missing", "{w}");
    // Plugged back in (same folder): runs again, status clears.
    recreate_as_a_different_dir(&ws, old_id);
    // A recreated directory is a different one: it needs Locate with confirm.
    let (code, _) = post(
        &s,
        &format!("/v1/channels/{id}/posts"),
        json!({ "text": "Again" }),
    )
    .await;
    assert_eq!(code, 400);
    let (code, r) = send(
        &s,
        "PATCH",
        &format!("/v1/workspaces/{wid}"),
        Some(json!({ "locate": ws })),
    )
    .await;
    assert_eq!(code, 409, "{r}");
    assert_eq!(r["needs_confirm"], true);
    let (code, r) = send(
        &s,
        "PATCH",
        &format!("/v1/workspaces/{wid}"),
        Some(json!({ "locate": ws, "confirm": true })),
    )
    .await;
    assert_eq!(code, 200, "{r}");
    assert_eq!(r["status"], "ok");
    say(&s, &id, "Again", None).await;
    assert_eq!(wait_file(&ws.join("back.txt")).await, "back");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sub_agent_inherits_the_folder_and_can_only_narrow_it() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Split the work", "turns": [
            { "text": "", "tool_calls": [
                { "name": "agent_spawn", "input": { "label": "kid", "subdir": "sub", "task": "Kid task." } },
                { "name": "agent_spawn", "input": { "label": "plain", "task": "Plain task." } },
                { "name": "agent_spawn", "input": { "label": "up", "subdir": "..", "task": "Up task." } },
                { "name": "agent_spawn", "input": { "label": "abs", "subdir": "/etc", "task": "Abs task." } },
                { "name": "agent_wait", "input": {} }
            ] },
            { "text": "All done." }
        ]},
        { "matches": "Kid task", "turns": [
            { "text": "", "tool_calls": [
                { "name": "write_file", "input": { "path": "kid.txt", "content": "kid" } },
                { "name": "write_file", "input": { "path": "../escape.txt", "content": "no" } }
            ] },
            { "text": "Kid done." }
        ]},
        { "matches": "Plain task", "turns": [
            { "text": "", "tool_calls": [
                { "name": "write_file", "input": { "path": "plain.txt", "content": "plain" } }
            ] },
            { "text": "Plain done." }
        ]},
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let ws = mkdir(&env, "tree");
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    let (_, ch) = post(
        &s,
        "/v1/channels",
        json!({ "slug": "tree", "workspace": ws }),
    )
    .await;
    let id = ch["id"].as_str().unwrap().to_owned();
    say(&s, &id, "Split the work", None).await;
    assert_eq!(wait_file(&ws.join("sub/kid.txt")).await, "kid");
    assert_eq!(wait_file(&ws.join("plain.txt")).await, "plain");
    assert!(
        !ws.join("escape.txt").exists(),
        "the narrowed child left its folder"
    );
    let db = env.db();
    let rows: Vec<(String, Option<String>, Option<String>)> = db
        .prepare("SELECT label, workspace_root, workspace_subdir FROM runs WHERE parent_run_id IS NOT NULL ORDER BY label")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let labels: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        labels,
        ["kid", "plain"],
        "widening spawns never started: {rows:?}"
    );
    assert_eq!(rows[0].1.as_deref(), ws.to_str());
    assert_eq!(rows[0].2.as_deref(), Some("sub"));
    assert_eq!(rows[1].1.as_deref(), ws.to_str());
    assert_eq!(rows[1].2, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unconfined_shell_is_refused_under_yolo_and_asks_when_supervised() {
    // No OS sandbox (confine = off): the shell has no folder to be bound to.
    let script = json!({ "conversations": [
        { "matches": "Run it", "turns": [
            { "text": "", "tool_calls": [{ "name": "shell", "input": { "command": "echo ran > ran.txt" } }] },
            { "text": "Done." }
        ]}
    ]});
    let env = Env::with_config(
        &script,
        "autonomy = \"yolo\"\n[sandbox]\nconfine = \"off\"\n",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let ws = env.home.join("tern-workspace");
    let sent = say(&s, "home", "Run it", None).await;
    let sid = sent["session_id"].as_str().unwrap();
    let (_, sess) = get(&s, &format!("/v1/sessions/{sid}")).await;
    let all = sess.to_string();
    assert!(all.contains("unattended commands are off"), "{all}");
    assert!(!ws.join("ran.txt").exists(), "the command ran unattended");
    let (_, ledger) = get(&s, "/v1/ledger?kind=approval").await;
    assert!(ledger.to_string().contains("unbounded_shell"), "{ledger:#}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_supervised_unconfined_shell_asks_every_time() {
    let script = json!({ "conversations": [
        { "matches": "Run it", "turns": [
            { "text": "", "tool_calls": [{ "name": "shell", "input": { "command": "echo ran > ran.txt" } }] },
            { "text": "Done." }
        ]}
    ]});
    let env = Env::with_config(&script, "[sandbox]\nconfine = \"off\"\n");
    let _d = env.daemon_with(None);
    let s = env.socket();
    let mut events = Events::open(&s).await;
    let (code, sent) = post(&s, "/v1/channels/home/posts", json!({ "text": "Run it" })).await;
    assert_eq!(code, 200, "{sent}");
    let evs = events
        .until(30, |e| {
            e.iter().any(|v| v.get("approval_requested").is_some())
        })
        .await;
    let req = evs
        .iter()
        .find_map(|v| v.get("approval_requested").cloned())
        .unwrap();
    assert!(
        req["pattern"].is_null(),
        "no \"always allow\" for a command nothing confines: {req}"
    );
}

#[test]
fn the_cli_adds_refuses_binds_and_reports() {
    let env = Env::new(&json!({ "turns": [{ "text": "ok" }] }));
    let run = |args: &[&str]| {
        let out = env.cmd(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (ok, _, err) = run(&["workspace", "add", "/"]);
    assert!(!ok && err.contains("whole disk"), "{err}");
    let (ok, _, err) = run(&["workspace", "add", env.home.to_str().unwrap()]);
    assert!(!ok && err.contains("home folder"), "{err}");
    let proj = mkdir(&env, "cli-proj");
    let (ok, out, err) = run(&["workspace", "add", proj.to_str().unwrap(), "--name", "Proj"]);
    assert!(ok, "{err}");
    assert!(out.contains("Proj") && out.contains("ws_"), "{out}");
    assert!(run(&["channels", "new", "docs"]).0);
    let (ok, out, err) = run(&[
        "workspace",
        "set",
        proj.to_str().unwrap(),
        "--channel",
        "#docs",
    ]);
    assert!(ok, "{err}");
    assert!(out.contains("now uses Proj"), "{out}");
    let (_, out, _) = run(&["workspace", "get", "--channel", "docs"]);
    assert!(
        out.contains(proj.to_str().unwrap()) && out.contains("its own binding"),
        "{out}"
    );
    let (_, out, _) = run(&["workspace", "get"]);
    assert!(out.contains("tern-workspace"), "{out}");
    // Read-only extras, then a folder in use can't be removed without --reassign-to.
    let docs = mkdir(&env, "cli-docs");
    assert!(
        !run(&["workspace", "roots", "Proj", "add", docs.to_str().unwrap()]).0,
        "a name isn't an id"
    );
    let id = run(&["workspace"]).1;
    let id = id
        .lines()
        .find(|l| l.contains("Proj"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let (ok, _, err) = run(&["workspace", "roots", &id, "add", docs.to_str().unwrap()]);
    assert!(ok, "{err}");
    let (ok, _, err) = run(&["workspace", "remove", &id]);
    assert!(!ok && err.contains("in use"), "{err}");
    let (ok, _, err) = run(&["workspace", "remove", &id, "--reassign-to", "default"]);
    assert!(ok, "{err}");
    let (_, out, _) = run(&["workspace", "get", "--channel", "docs"]);
    assert!(
        out.contains("tern-workspace"),
        "reassigned to the default: {out}"
    );
    let (ok, _, _) = run(&["workspace", "check"]);
    assert!(ok);
    std::fs::remove_dir_all(&proj).ok();
}

/// A directory's `(device, inode)`, what the daemon knows a workspace by.
fn dir_id(dir: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(dir).unwrap();
    (m.dev(), m.ino())
}

/// Create `dir` again as a different directory. ext4 and tmpfs often hand a
/// just-freed inode straight to the next `mkdir`, which makes the new folder
/// indistinguishable from the old one; set any such reuse aside and retry.
fn recreate_as_a_different_dir(dir: &Path, old: (u64, u64)) {
    for n in 0..16 {
        std::fs::create_dir_all(dir).unwrap();
        if dir_id(dir) != old {
            return;
        }
        let held = dir.with_file_name(format!(".held-inode-{n}"));
        std::fs::rename(dir, held).unwrap();
    }
    panic!("mkdir kept reusing the old inode");
}
