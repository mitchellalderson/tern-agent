//! Agent profiles AP3 end-to-end (docs/agent-profiles-design.md §8): the
//! per-agent autonomy ceiling, agent templates as bundles, "Let agents
//! decide" and a Telegram bot per agent.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use std::path::Path;

use serde_json::{Value, json};
use subagents_env::{Env, api, get, post};

async fn patch(socket: &Path, path: &str, body: Value) -> (u16, Value) {
    let r = api(socket)
        .request(reqwest::Method::PATCH, format!("http://tern{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
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

fn autonomy_of(env: &Env, run: &str) -> String {
    env.query(&format!("SELECT autonomy FROM runs WHERE id = '{run}'"))
}

// ---- 1. the autonomy ceiling ----

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_autonomy_ceiling_caps_its_runs_routines_and_delegates() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Hand it over", "turns": [
                { "text": "", "tool_calls": [{ "name": "agent_spawn", "input": {
                    "label": "c", "agent": "careful", "task": "Look this up." } }] },
                { "text": "Done." }
            ]},
            { "matches": "Look this up", "turns": [{ "text": "Found it." }] },
            { "matches": "hi", "turns": [{ "text": "hello" }] },
            { "matches": "Sweep", "turns": [{ "text": "Swept." }] }
        ]}),
        "autonomy = \"autonomous\"\n",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();

    let (code, bad) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Bad", "autonomy": "reckless" }),
    )
    .await;
    assert!(
        code == 400 || code == 422,
        "an unknown level is refused: {code} {bad}"
    );

    let (code, careful) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Careful", "autonomy": "readonly" }),
    )
    .await;
    assert_eq!(code, 201, "{careful}");
    assert_eq!(careful["autonomy"], "readonly");
    let (_, bold) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Bold", "autonomy": "yolo" }),
    )
    .await;
    let (_, plain) = post(&s, "/v1/agents", json!({ "name": "Plain" })).await;
    assert_eq!(plain.get("autonomy"), None);

    // The lower of the ceiling and the configured autonomy, frozen on the run.
    let run = say(&s, careful["main_channel_id"].as_str().unwrap(), "hi").await;
    assert_eq!(autonomy_of(&env, &run), "readonly");
    // A ceiling never raises it.
    let run = say(&s, bold["main_channel_id"].as_str().unwrap(), "hi").await;
    assert_eq!(autonomy_of(&env, &run), "autonomous");
    let run = say(&s, plain["main_channel_id"].as_str().unwrap(), "hi").await;
    assert_eq!(autonomy_of(&env, &run), "autonomous");

    // A routine's own profile can't pass its agent's ceiling either.
    let (_, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Sweep", "agent": "careful",
                "autonomy": "yolo" }),
    )
    .await;
    let (_, r) = post(
        &s,
        &format!("/v1/schedules/{}/run", sch["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let run = r["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{run}/wait")).await;
    assert_eq!(autonomy_of(&env, &run), "readonly");

    // Delegating to a capped agent runs the child under the cap.
    patch(&s, "/v1/agents/tern", json!({ "delegates": ["careful"] })).await;
    let run = say(&s, "home", "Hand it over").await;
    let child: String = env.query(&format!(
        "SELECT id FROM runs WHERE parent_run_id = '{run}' AND label = 'c'"
    ));
    assert_eq!(autonomy_of(&env, &child), "readonly");
    assert_eq!(autonomy_of(&env, &run), "autonomous");

    // Editable, removable, and recorded in the ledger.
    let (_, a) = patch(&s, "/v1/agents/careful", json!({ "autonomy": null })).await;
    assert_eq!(a.get("autonomy"), None, "{a}");
    let (_, a) = patch(
        &s,
        "/v1/agents/careful",
        json!({ "autonomy": "supervised" }),
    )
    .await;
    assert_eq!(a["autonomy"], "supervised");
    let n: i64 = env.query("SELECT COUNT(*) FROM ledger WHERE kind = 'autonomy.agent'");
    assert!(n >= 2, "ledger entries: {n}");
}

// ---- 3. let agents decide ----

async fn agent(s: &Path, body: Value) -> String {
    let (code, a) = post(s, "/v1/agents", body).await;
    assert_eq!(code, 201, "{a}");
    a["id"].as_str().unwrap().to_owned()
}

async fn posts_until(s: &Path, channel: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let start = std::time::Instant::now();
    loop {
        let (_, posts) = get(s, &format!("/v1/channels/{channel}/posts?limit=100")).await;
        let mut posts = posts.as_array().cloned().unwrap_or_default();
        posts.reverse();
        if done(&posts) {
            return posts;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(20),
            "timed out; posts: {posts:#?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn send(s: &Path, channel: &str, text: &str) -> Value {
    let (code, sent) = post(
        s,
        &format!("/v1/channels/{channel}/posts"),
        json!({ "text": text }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    sent
}

#[tokio::test(flavor = "multi_thread")]
async fn let_agents_decide_routes_a_post_to_the_right_members() {
    let env = Env::with_config(
        &json!({ "conversations": [
            // The routing call (it carries "Message to route:"); a real
            // run's first message never does.
            { "matches": "Message to route:\nWhat does Acme charge", "turns": [
                { "text": "{\"agents\": [\"research\"]}" }] },
            { "matches": "Message to route:\nBoth please", "turns": [
                { "text": "Sure: {\"agents\": [\"@Ops\", \"ghost\", \"research\", \"ops\"]}" }] },
            { "matches": "Message to route:\nGarbled", "turns": [
                { "text": "I think research." }] },
            { "matches": "Message to route:\nOutsider", "turns": [
                { "text": "{\"agents\": [\"outsider\"]}" }] },
            { "matches": "What does Acme charge", "turns": [{ "text": "Research: $15." }] },
            { "matches": "Both please", "turns": [{ "text": "On it." }] },
            { "matches": "Garbled", "turns": [{ "text": "Lead here." }] },
            { "matches": "Outsider", "turns": [{ "text": "Lead again." }] },
            { "matches": "Unrouted", "turns": [{ "text": "Lead, as always." }] }
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let research = agent(
        &s,
        json!({ "name": "Research", "persona": "You find things out on the web." }),
    )
    .await;
    let ops = agent(
        &s,
        json!({ "name": "Ops", "persona": "You run the calendar and logistics." }),
    )
    .await;
    agent(&s, json!({ "name": "Outsider" })).await;

    for slug in ["ask", "plain"] {
        let (code, c) = post(&s, "/v1/channels", json!({ "slug": slug })).await;
        assert_eq!(code, 201, "{c}");
        for a in ["research", "ops"] {
            post(
                &s,
                &format!("/v1/channels/{slug}/agents"),
                json!({ "agent": a }),
            )
            .await;
        }
    }
    // Off by default, and only for topic channels.
    let (_, m) = get(&s, "/v1/channels/ask/agents").await;
    assert_eq!(m["let_agents_decide"], false);
    let (code, _) = patch(
        &s,
        "/v1/channels/home",
        json!({ "let_agents_decide": true }),
    )
    .await;
    assert_eq!(code, 400);
    let (code, c) = patch(&s, "/v1/channels/ask", json!({ "let_agents_decide": true })).await;
    assert_eq!(code, 200, "{c}");
    assert_eq!(c["agents"]["let_agents_decide"], true);

    // One agent picked: it answers, the lead doesn't, and a note says so.
    let sent = send(&s, "ask", "What does Acme charge?").await;
    let woke = sent["woke"].as_array().unwrap();
    assert_eq!(woke.len(), 1, "{sent:#}");
    assert_eq!(woke[0]["agent_id"], research.as_str());
    let posts = posts_until(&s, "ask", |p| {
        p.iter().any(|p| p["author_id"] == research.as_str())
    })
    .await;
    assert!(
        posts
            .iter()
            .any(|p| p["kind"] == "note" && p["text"] == "Sent to @research"),
        "{posts:#?}"
    );

    // Several picks: members only, each once (`ghost` isn't an agent).
    let sent = send(&s, "ask", "Both please").await;
    let agents: Vec<_> = sent["woke"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["agent_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(agents, vec![ops.clone(), research.clone()], "{sent:#}");

    // An answer that can't be read, or names someone who isn't a member:
    // the lead answers, as without the setting.
    for text in ["Garbled", "Outsider"] {
        let sent = send(&s, "ask", text).await;
        assert_eq!(sent["woke"][0]["agent_id"], "tern", "{text}: {sent:#}");
    }
    // A mention beats routing, and the setting off means the lead.
    let sent = send(&s, "ask", "@ops Unrouted").await;
    assert_eq!(sent["woke"][0]["agent_id"], ops.as_str());
    let sent = send(&s, "plain", "What does Acme charge?").await;
    assert_eq!(sent["woke"][0]["agent_id"], "tern", "{sent:#}");
}

// ---- 4. a Telegram bot per agent ----

/// The fake Bot API (`tests/fixtures/fake_telegram.py`); one per bot.
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

    async fn says(&self, user: i64, text: &str) {
        self.http
            .post(format!("{}/control/message", self.url))
            .json(&json!({ "chat_id": CHAT, "user_id": user, "text": text }))
            .send()
            .await
            .unwrap();
    }

    /// The first bot message containing `needle`.
    async fn wait_for(&self, needle: &str) -> Value {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let all: Vec<Value> = self
                .http
                .get(format!("{}/control/messages", self.url))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if let Some(m) = all
                .iter()
                .find(|m| m["text"].as_str().unwrap_or("").contains(needle))
            {
                return m.clone();
            }
            assert!(
                std::time::Instant::now() < end,
                "no bot message with {needle:?}: {all:#?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_with_its_own_bot_is_its_own_contact_and_pairing_is_shared() {
    let main_bot = FakeTelegram::start();
    let research_bot = FakeTelegram::start();
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Look into Acme", "turns": [{ "text": "Acme moved to Stripe." }] },
            { "matches": "Hello tern", "turns": [{ "text": "tern here." }] }
        ]}),
        &format!(
            "[gateway.telegram]\nenabled = true\nbot_token_secret = \"MAIN_TOKEN\"\napi_url = \"{}\"\n",
            main_bot.url
        ),
    );
    let secrets = env.tern_home.join("secrets.toml");
    std::fs::write(
        &secrets,
        "MAIN_TOKEN = \"TEST:TOKEN\"\nRESEARCH_TOKEN = \"TEST:TOKEN\"\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    // The CLI sets it up without a daemon: an unknown or shared secret, and
    // the default agent's bot, are refused.
    let run = |args: &[&str]| env.cmd(args).output().unwrap();
    assert!(
        run(&["agents", "new", "Research", "--template", "research"])
            .status
            .success()
    );
    assert!(run(&["agents", "new", "Other"]).status.success());
    assert!(
        !run(&["agents", "telegram", "tern", "RESEARCH_TOKEN"])
            .status
            .success()
    );
    assert!(
        !run(&["agents", "telegram", "research", "MAIN_TOKEN"])
            .status
            .success()
    );
    let ok = run(&["agents", "telegram", "research", "RESEARCH_TOKEN"]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(
        !run(&["agents", "telegram", "other", "RESEARCH_TOKEN"])
            .status
            .success()
    );
    let list = String::from_utf8_lossy(&run(&["agents"]).stdout).into_owned();
    assert!(
        list.lines()
            .any(|l| l.contains("@research") && l.contains("own bot")),
        "{list}"
    );
    // Tests point it at the fake: the API refuses an api_url, so write it.
    env.db()
        .execute(
            "UPDATE agents SET telegram = ?1 WHERE slug = 'research'",
            [
                json!({ "bot_token_secret": "RESEARCH_TOKEN", "api_url": research_bot.url })
                    .to_string(),
            ],
        )
        .unwrap();

    let _d = env.daemon_with(None);
    let s = env.socket();
    // The API shows it, and won't take a shared token or an api_url.
    let (_, agents) = get(&s, "/v1/agents").await;
    let r = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["slug"] == "research")
        .unwrap();
    assert_eq!(r["telegram"]["bot_token_secret"], "RESEARCH_TOKEN");
    for bad in [
        json!({ "telegram": { "bot_token_secret": "MAIN_TOKEN" } }),
        json!({ "telegram": { "bot_token_secret": "RESEARCH_TOKEN" } }),
        json!({ "telegram": { "bot_token_secret": "NEW_TOKEN", "api_url": "http://evil" } }),
        json!({ "telegram": { "bot_token_secret": "not a name" } }),
    ] {
        let (code, body) = patch(&s, "/v1/agents/other", bad.clone()).await;
        assert_eq!(code, 400, "{bad}: {body}");
    }

    // Pair once on the main bot...
    let out = env.cmd(&["gateway", "pair"]).output().unwrap();
    main_bot
        .says(1, String::from_utf8_lossy(&out.stdout).trim())
        .await;
    main_bot.wait_for("Paired.").await;
    // ...and the agent's bot already knows you; a stranger gets nothing.
    research_bot.says(2, "Look into Acme").await;
    research_bot.wait_for("This bot is private").await;
    research_bot.says(1, "Look into Acme").await;
    // The bot is Research: no name prefix, and the chat is its own thread
    // of Research's DM.
    let m = research_bot.wait_for("Acme moved to Stripe.").await;
    assert_eq!(m["text"], "Acme moved to Stripe.", "{m}");
    let (channel, agent): (String, String) = env
        .db()
        .query_row(
            "SELECT channel_id, agent_id FROM sessions WHERE source = 'telegram-research'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(agent, "tern:research");
    let dm: String = env.query("SELECT id FROM channels WHERE slug = 'dm-research'");
    assert_eq!(channel, dm);
    // Nothing to switch on it.
    research_bot.says(1, "/agent tern").await;
    research_bot
        .wait_for("This bot is Research's own chat")
        .await;
    // The main bot is unchanged.
    main_bot.says(1, "Hello tern").await;
    main_bot.wait_for("tern here.").await;
    let main_sessions: i64 = env.query("SELECT COUNT(*) FROM sessions WHERE source = 'telegram'");
    assert_eq!(main_sessions, 1);
    // Removing the bot is recorded.
    let (code, a) = patch(&s, "/v1/agents/research", json!({ "telegram": null })).await;
    assert_eq!(code, 200, "{a}");
    assert_eq!(a.get("telegram"), None);
    let n: i64 = env.query("SELECT COUNT(*) FROM ledger WHERE kind = 'telegram.agent'");
    assert!(n >= 2, "ledger entries: {n}");
}
