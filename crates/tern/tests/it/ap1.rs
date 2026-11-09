//! Agent profiles AP1 end-to-end (docs/agent-profiles-design.md §6.4):
//! Telegram's `/agents` and `/agent`, with forum topics where available.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use subagents_env::{Env, post};

/// The fake Bot API (`tests/fixtures/fake_telegram.py`).
struct FakeTelegram {
    _proc: subagents_env::Killed,
    url: String,
    http: reqwest::Client,
}

const CHAT: i64 = 7070;
const GROUP: i64 = -100_200;

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

    async fn says(&self, body: Value) {
        self.http
            .post(format!("{}/control/message", self.url))
            .json(&body)
            .send()
            .await
            .unwrap();
    }

    async fn user_says(&self, text: &str) {
        self.says(json!({ "chat_id": CHAT, "user_id": 1, "text": text }))
            .await;
    }

    /// The first bot message containing `needle`.
    async fn wait_for(&self, needle: &str) -> Value {
        let end = Instant::now() + Duration::from_secs(15);
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
                Instant::now() < end,
                "no bot message with {needle:?}: {all:#?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn telegram_switches_agents_and_gives_each_a_forum_topic() {
    let tg = FakeTelegram::start();
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Look into Acme", "turns": [{ "text": "Acme moved to Stripe." }] },
            { "matches": "From the topic", "turns": [{ "text": "Topic answer." }] },
            { "matches": "Hello tern", "turns": [{ "text": "tern here." }] }
        ]}),
        &format!(
            "[gateway.telegram]\nenabled = true\nbot_token_secret = \"TELEGRAM_BOT_TOKEN\"\napi_url = \"{}\"\n",
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
    let (_, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Research", "template": "research" }),
    )
    .await;
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();
    let out = env.cmd(&["gateway", "pair"]).output().unwrap();
    tg.user_says(String::from_utf8_lossy(&out.stdout).trim())
        .await;
    tg.wait_for("Paired.").await;

    tg.user_says("/agents").await;
    tg.wait_for("Research (/agent research)").await;
    tg.user_says("/agent research").await;
    tg.wait_for("You're talking to Research now").await;
    tg.user_says("Look into Acme").await;
    // Replies say who answers.
    tg.wait_for("Research · Acme moved to Stripe.").await;
    // That chat is a thread of Research's DM, acting as Research.
    let (channel, agent): (String, String) = env
        .db()
        .query_row(
            "SELECT channel_id, agent_id FROM sessions WHERE source = 'telegram' AND agent_id IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (channel.as_str(), agent.as_str()),
        (dm.as_str(), "tern:research")
    );

    tg.user_says("/agent tern").await;
    tg.wait_for("Back to tern.").await;
    tg.user_says("Hello tern").await;
    let m = tg.wait_for("tern here.").await;
    assert!(!m["text"].as_str().unwrap().contains("·"), "{m}");

    // In a forum group, /agent opens a topic that is that agent's chat.
    tg.says(json!({ "chat_id": GROUP, "user_id": 1, "text": "/agent research", "forum": true }))
        .await;
    let m = tg.wait_for("This topic is my chat").await;
    let topic = m["thread"].as_i64().unwrap();
    tg.says(json!({
        "chat_id": GROUP, "user_id": 1, "text": "From the topic", "forum": true, "thread": topic
    }))
    .await;
    let m = tg.wait_for("Research · Topic answer.").await;
    assert_eq!(m["thread"].as_i64(), Some(topic));
    let refs: i64 = env.query(&format!(
        "SELECT count(*) FROM sessions WHERE external_ref = '{GROUP}/{topic}|tern:research'"
    ));
    assert_eq!(refs, 1);
}
