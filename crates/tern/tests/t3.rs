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
    // The allowlist entry below only counts for a shell the sandbox bounds.
    let env = Env::with_config_sandboxable(
        &json!({ "conversations": [
            { "matches": "Compare", "turns": [
                { "text": "Fetching.", "tool_calls": [{ "name": "shell", "input": { "command": "echo prices" } }] },
                { "text": "Done." }
            ]}
        ]}),
        // Off by default; a config from before that change turns it on.
        "[sandbox]\nnetwork = true\n",
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
        "every command can reach the network"
    );
}

/// Send `text` into a channel and wait for its run; returns the run id.
async fn say(socket: &std::path::Path, channel: &str, text: &str) -> String {
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

#[tokio::test(flavor = "multi_thread")]
async fn the_host_limit_setting_comes_from_the_agent_then_the_config() {
    let env = Env::with_config(
        &json!({ "turns": [{ "text": "ok" }] }),
        "[taint]\negress = \"ask\"\n",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let egress = |run: &str| -> Option<String> {
        env.db()
            .query_row("SELECT egress FROM runs WHERE id = ?1", [run], |r| r.get(0))
            .unwrap()
    };
    let run = say(&s, "home", "hello").await;
    assert_eq!(egress(&run).as_deref(), Some("ask"));

    let (code, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Scout", "egress": "allow" }),
    )
    .await;
    assert_eq!(code, 201, "{a}");
    assert_eq!(a["egress"], "allow");
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();
    let run = say(&s, &dm, "find things").await;
    assert_eq!(
        egress(&run).as_deref(),
        Some("allow"),
        "the agent's own wins"
    );

    // Back to the config's.
    let a = patch(&s, "/v1/agents/scout", json!({ "egress": null })).await;
    assert_eq!(a.get("egress"), None, "{a}");
    let (code, bad) = post(
        &s,
        "/v1/agents",
        json!({ "name": "X", "egress": "sometimes" }),
    )
    .await;
    assert_eq!(code, 422, "{bad}");
}

/// The posts in `channel`, newest first.
async fn posts(s: &std::path::Path, channel: &str) -> Vec<Value> {
    get(s, &format!("/v1/channels/{channel}/posts"))
        .await
        .1
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_outbox_item_is_a_needs_you_post_with_a_home_pointer_until_decided() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Sweep", "turns": [
                { "text": "Reading.", "tool_calls": [{ "name": "mcp_list", "input": {} }] },
                { "text": "Replying.", "tool_calls": [{ "name": "write_file",
                    "input": { "path": "reply.txt", "content": "hi" } }] },
                { "text": "Drafted; it waits for you." }
            ]}
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    post(&s, "/v1/channels", json!({ "slug": "email" })).await;
    say(&s, "email", "Sweep the inbox").await;

    let (_, items) = get(&s, "/v1/outbox").await;
    let id = items[0]["id"].as_i64().unwrap();
    let topic = posts(&s, "email").await;
    let needs = topic
        .iter()
        .find(|p| p["kind"] == "needs_you")
        .unwrap_or_else(|| panic!("{topic:#?}"));
    assert_eq!(needs["outbox_id"], id);
    assert_eq!(needs["tainted"], true);
    assert!(
        needs["text"]
            .as_str()
            .unwrap()
            .contains(&format!("outbox #{id}"))
    );
    let home = posts(&s, "home").await;
    let pointer = home.iter().find(|p| p["kind"] == "pointer").unwrap();
    assert_eq!(pointer["points_to"], needs["id"]);
    assert_eq!(pointer["outbox_id"], id);
    let (_, channels) = get(&s, "/v1/channels").await;
    let count = |slug: &str, chans: &Value| {
        chans
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["slug"] == slug)
            .unwrap()["needs_you"]
            .clone()
    };
    assert!(count("email", &channels).as_i64().unwrap() >= 1);

    // "Done" alone can't close it: the item must be decided.
    let (code, _) = post(
        &s,
        &format!("/v1/posts/{}/resolve", pointer["id"]),
        json!({}),
    )
    .await;
    assert_eq!(code, 409);

    // Rejecting closes the post and its pointer.
    let (code, _) = post(
        &s,
        &format!("/v1/outbox/{id}/reject"),
        json!({ "note": "no" }),
    )
    .await;
    assert_eq!(code, 200);
    let needs = wait_for(&s, "/v1/channels/email/posts", |v| {
        v.as_array()
            .unwrap()
            .iter()
            .any(|p| p["kind"] == "needs_you" && !p["resolved_at"].is_null())
    })
    .await;
    assert!(
        needs
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["outbox_id"] == id)
    );
    let home = posts(&s, "home").await;
    assert!(!home.iter().find(|p| p["kind"] == "pointer").unwrap()["resolved_at"].is_null());
    let (_, channels) = get(&s, "/v1/channels").await;
    assert_eq!(count("email", &channels), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_outbox_item_waits_in_its_dm_and_closes_when_approved() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Save it", "turns": [
                // A command always asks. Approved below with an edit that
                // makes it a no-op.
                { "text": "Saving.", "tool_calls": [{ "name": "shell",
                    "input": { "command": "echo saving" } }] },
                { "text": "It waits for you." }
            ]}
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (code, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Scout", "tools": { "builtins": ["shell"], "connectors": [], "mcp": [] } }),
    )
    .await;
    assert_eq!(code, 201, "{a}");
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();
    say(&s, &dm, "Save it").await;

    let (_, items) = get(&s, "/v1/outbox").await;
    assert_eq!(items[0]["channel_id"], dm.as_str(), "{items:#}");
    let id = items[0]["id"].as_i64().unwrap();
    let mine = posts(&s, &dm).await;
    let needs = mine.iter().find(|p| p["kind"] == "needs_you").unwrap();
    assert_eq!(needs["author"], "Scout");
    assert_eq!(needs["outbox_id"], id);
    let home = posts(&s, "home").await;
    assert!(
        home.iter()
            .any(|p| p["kind"] == "pointer" && p["outbox_id"] == id)
    );

    let (code, _) = post(
        &s,
        &format!("/v1/outbox/{id}/approve"),
        json!({ "input": { "command": "true" } }),
    )
    .await;
    assert_eq!(code, 200);
    wait_for(&s, &format!("/v1/channels/{dm}/posts"), |v| {
        v.as_array()
            .unwrap()
            .iter()
            .any(|p| p["kind"] == "needs_you" && !p["resolved_at"].is_null())
    })
    .await;
    let home = posts(&s, "home").await;
    assert!(
        home.iter()
            .filter(|p| p["kind"] == "pointer")
            .all(|p| !p["resolved_at"].is_null())
    );
}

/// The fake Bot API (`tests/fixtures/fake_telegram.py`).
struct FakeTelegram {
    _proc: subagents_env::Killed,
    url: String,
    http: reqwest::Client,
}

const CHAT: i64 = 7070;

impl FakeTelegram {
    fn start() -> Self {
        use std::io::BufRead;
        let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake_telegram.py");
        let mut child = std::process::Command::new("python3")
            .arg(script)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            _proc: subagents_env::Killed(child),
            url: format!("http://127.0.0.1:{}", line.trim()),
            http: reqwest::Client::new(),
        }
    }

    async fn user_says(&self, text: &str) {
        self.http
            .post(format!("{}/control/message", self.url))
            .json(&json!({ "chat_id": CHAT, "user_id": 1, "text": text }))
            .send()
            .await
            .unwrap();
    }

    async fn messages(&self) -> Vec<Value> {
        self.http
            .get(format!("{}/control/messages", self.url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// The first bot message containing `needle`.
    async fn wait_for(&self, needle: &str) -> Value {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            let all = self.messages().await;
            if let Some(m) = all
                .iter()
                .find(|m| m["text"].as_str().unwrap_or("").contains(needle))
            {
                return m.clone();
            }
            assert!(
                Instant::now() < end,
                "no bot message with {needle:?}: {all:#?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_routines_deferred_write_reaches_telegram_with_its_delivery_and_closes_there() {
    let tg = FakeTelegram::start();
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Sweep the inbox", "turns": [
                { "text": "Reading.", "tool_calls": [{ "name": "mcp_list", "input": {} }] },
                { "text": "Replying.", "tool_calls": [{ "name": "write_file",
                    "input": { "path": "reply.txt", "content": "hi" } }] },
                { "text": "Two invoices; I drafted a reply." }
            ]}
        ]}),
        &format!(
            "[gateway.telegram]\nenabled = true\nbot_token_secret = \"TELEGRAM_BOT_TOKEN\"\napi_url = \"{}\"\n\
             [schedules]\nquiet_hours = \"none\"\n[outbox]\nwait_secs = 1\n",
            tg.url
        ),
    );
    let secrets = env.tern_home.join("secrets.toml");
    std::fs::write(&secrets, "TELEGRAM_BOT_TOKEN = \"TEST:TOKEN\"\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let _d = env.daemon_with(None);
    let s = env.socket();
    let out = env.cmd(&["gateway", "pair"]).output().unwrap();
    tg.user_says(String::from_utf8_lossy(&out.stdout).trim())
        .await;
    tg.wait_for("Paired.").await;

    let (code, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 3 * * *", "prompt": "Sweep the inbox",
                "deliver_to": format!("telegram:{CHAT}"), "initiative": "interrupt" }),
    )
    .await;
    assert_eq!(code, 201, "{sch}");
    let (_, r) = post(
        &s,
        &format!("/v1/schedules/{}/run", sch["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    get(
        &s,
        &format!("/v1/runs/{}/wait", r["run_id"].as_str().unwrap()),
    )
    .await;

    // No card at 3 a.m.: the routine's own delivery says what waits.
    let delivered = tg.wait_for("Two invoices").await;
    let text = delivered["text"].as_str().unwrap();
    assert!(
        text.contains("Waiting for your approval") && text.contains("/outbox"),
        "{text}"
    );
    assert!(
        !tg.messages()
            .await
            .iter()
            .any(|m| m["text"].as_str().unwrap_or("").starts_with("📬")),
        "no card yet"
    );

    // /outbox shows it with buttons; deciding it elsewhere updates the card.
    tg.user_says("/outbox").await;
    let card = tg.wait_for("📬 Waiting for you (outbox #").await;
    assert_eq!(card["buttons"].as_array().unwrap().len(), 2, "{card}");
    let (_, items) = get(&s, "/v1/outbox").await;
    let id = items[0]["id"].as_i64().unwrap();
    let (code, _) = post(&s, &format!("/v1/outbox/{id}/approve"), json!({})).await;
    assert_eq!(code, 200);
    let edited = tg.wait_for("→ Approved").await;
    assert_eq!(edited["message_id"], card["message_id"]);
    assert!(edited["buttons"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_routine_in_quiet_hours_defers_without_pinging_the_chat() {
    let tg = FakeTelegram::start();
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Sweep the inbox", "turns": [
                { "text": "Reading.", "tool_calls": [{ "name": "mcp_list", "input": {} }] },
                { "text": "Replying.", "tool_calls": [{ "name": "write_file",
                    "input": { "path": "reply.txt", "content": "hi" } }] },
                { "text": "Drafted a reply." }
            ]}
        ]}),
        &format!(
            "[gateway.telegram]\nenabled = true\nbot_token_secret = \"TELEGRAM_BOT_TOKEN\"\napi_url = \"{}\"\n\
             [schedules]\nquiet_hours = \"00:01-00:00\"\n",
            tg.url
        ),
    );
    let secrets = env.tern_home.join("secrets.toml");
    std::fs::write(&secrets, "TELEGRAM_BOT_TOKEN = \"TEST:TOKEN\"\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let _d = env.daemon_with(None);
    let s = env.socket();
    let out = env.cmd(&["gateway", "pair"]).output().unwrap();
    tg.user_says(String::from_utf8_lossy(&out.stdout).trim())
        .await;
    tg.wait_for("Paired.").await;
    let (_, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 3 * * *", "prompt": "Sweep the inbox",
                "deliver_to": format!("telegram:{CHAT}"), "initiative": "interrupt" }),
    )
    .await;
    let started = Instant::now();
    let (_, r) = post(
        &s,
        &format!("/v1/schedules/{}/run", sch["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    get(
        &s,
        &format!("/v1/runs/{}/wait", r["run_id"].as_str().unwrap()),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "deferred at once, not after the wait"
    );
    let (_, items) = get(&s, "/v1/outbox").await;
    assert_eq!(items.as_array().unwrap().len(), 1);
    // Nothing reached the chat: no prompt, no card; the delivery is held.
    let sent: Vec<String> = tg
        .messages()
        .await
        .iter()
        .map(|m| m["text"].as_str().unwrap_or("").to_owned())
        .filter(|t| !t.starts_with("Paired."))
        .collect();
    assert!(sent.is_empty(), "{sent:?}");
}
