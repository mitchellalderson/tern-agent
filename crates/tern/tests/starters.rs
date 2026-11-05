//! The starter agents catalog (`assets/starters.toml`) end to end: every
//! starter is created through the same `/v1/agents` path a user-created
//! agent takes, names only tools the registry really has, and lands with the
//! persona, scope and avatar it declares.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use serde_json::{Value, json};
use subagents_env::{Env, get, post};
use tern_core::agents::{self, Pick};
use tern_core::approval::Autonomy;

fn served_for<'a>(served: &'a [Value], id: &str) -> &'a Value {
    served.iter().find(|t| t["id"] == id).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_starter_creates_a_working_agent() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let _d = env.daemon_with(None);
    let s = env.socket();

    let (_, served) = get(&s, "/v1/agents/templates").await;
    let served = served.as_array().unwrap();
    let catalog = agents::templates();
    assert_eq!(served.len(), catalog.len());
    assert!(served.iter().all(|t| t["category"].is_string()));

    let (_, info) = get(&s, "/v1/info").await;
    let registry: Vec<&str> = info["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t[0].as_str().unwrap())
        .collect();

    for t in &catalog {
        // The starters' tool names are real tools.
        if let Some(Pick::Names(names)) = t.tools.as_ref().map(|t| t.builtins.clone()) {
            for n in names {
                assert!(registry.contains(&n.as_str()), "{}: no tool {n}", t.id);
            }
        }
        let name = if t.name.is_empty() { "Blank" } else { &t.name };
        let (code, a) = post(&s, "/v1/agents", json!({ "name": name, "template": t.id })).await;
        assert_eq!(code, 201, "{}: {a}", t.id);
        assert_eq!(a["memory"], t.memory, "{}", t.id);
        // The starter's ceiling lands on the agent, and no starter is above supervised.
        let ceiling = t.autonomy.expect("every starter has a ceiling");
        assert!(ceiling <= Autonomy::Supervised, "{}", t.id);
        assert_eq!(a["autonomy"], ceiling.as_str(), "{}", t.id);
        assert_eq!(served_for(served, &t.id)["autonomy"], ceiling.as_str());
        assert_eq!(a["avatar"]["glyph"], t.avatar.0, "{}", t.id);
        let expected = match &t.tools {
            Some(scope) => serde_json::to_value(scope).unwrap(),
            None => Value::Null,
        };
        if t.tools.is_some() {
            assert_eq!(a["tools"], expected, "{}", t.id);
        } else {
            // The narrow default: no shell, no connectors.
            assert!(
                a["tools"]["builtins"]
                    .as_array()
                    .is_some_and(|b| !b.contains(&json!("shell"))),
                "{}: {}",
                t.id,
                a["tools"]
            );
        }
        let (_, p) = get(
            &s,
            &format!("/v1/agents/{}/persona", a["slug"].as_str().unwrap()),
        )
        .await;
        let text = p["persona"].as_str().unwrap_or_default();
        assert!(text.starts_with("You are "), "{}: {p}", t.id);
        assert!(!text.contains("{name}"), "{}", t.id);
        assert!(
            text.contains(name) || t.persona.contains("{name}") || t.id == "blank",
            "{}",
            t.id
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chosen_autonomy_beats_the_starters() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    let _d = env.daemon_with(None);
    let s = env.socket();
    // Research is readonly; the user asks for supervised and gets it.
    let (code, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Looker", "template": "research", "autonomy": "supervised" }),
    )
    .await;
    assert_eq!(code, 201, "{a}");
    assert_eq!(a["autonomy"], "supervised");
    let (_, b) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Reader", "template": "research" }),
    )
    .await;
    assert_eq!(b["autonomy"], "readonly");
}
