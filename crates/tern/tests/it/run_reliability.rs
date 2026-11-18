//! Run reliability end-to-end: taint and the autonomy ceiling carry across
//! the runs of one conversation.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::Path;

use serde_json::json;

use crate::subagents_env::{Env, get, post};

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

fn col(env: &Env, run: &str, col: &str) -> Option<String> {
    env.db()
        .query_row(
            &format!("SELECT {col} FROM runs WHERE id = ?1"),
            [run],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn taint_and_the_ceiling_carry_to_later_runs_in_the_conversation() {
    let env = Env::with_config(
        &json!({ "conversations": [{ "matches": "hi", "turns": [{ "text": "hello" }] }] }),
        "autonomy = \"autonomous\"\n",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, agent) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Carry", "persona": "You are Carry." }),
    )
    .await;
    let channel = agent["main_channel_id"].as_str().unwrap();
    let first = say(&s, channel, "hi").await;
    assert_eq!(col(&env, &first, "taint"), None);
    // The first run read something untrusted and was held to `supervised`.
    env.db()
        .execute(
            "UPDATE runs SET taint = 'web page', autonomy = 'supervised' WHERE id = ?1",
            [&first],
        )
        .unwrap();
    let second = say(&s, channel, "hi again").await;
    assert_eq!(col(&env, &second, "taint").as_deref(), Some("web page"));
    assert_eq!(
        col(&env, &second, "autonomy").as_deref(),
        Some("supervised")
    );
}

/// R6: the one end-to-end check (daemon, channel, mock model, a tool, the
/// posted answer). The answer written next to a `memory_save` call survives
/// the model's short afterword.
#[tokio::test(flavor = "multi_thread")]
async fn smoke_the_answer_beside_a_tool_call_is_posted_before_the_afterword() {
    let answer = "Northwind renewed on 3 March for 24 months at $48k a year, with a 5% uplift in \
                  year two. Their champion is Dana Okafor, and legal wants the addendum signed \
                  before the 20th. Open risk: procurement asked about a price lock.";
    let env = Env::with_config(
        &json!({ "conversations": [{ "matches": "Northwind", "turns": [
            { "text": answer, "tool_calls": [{ "name": "memory_save",
                "input": { "content": "Northwind renewed 3 March, 24 months" } }] },
            { "text": "I saved the details." }
        ]}]}),
        "autonomy = \"autonomous\"\n",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let run = say(&s, "home", "What happened with Northwind?").await;
    let (code, done) = get(&s, &format!("/v1/runs/{run}/wait")).await;
    assert_eq!(code, 200, "{done}");
    let text = done["final_text"].as_str().unwrap();
    assert_eq!(text, format!("{answer}\n\nI saved the details."), "{done}");
}
