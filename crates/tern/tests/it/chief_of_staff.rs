//! The default agent as a Chief of Staff: `@chief` on a new install (an
//! existing install keeps its name), a persona that plans, delegates and
//! briefs, and the channel lead's explicit list of teammates and lanes.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use serde_json::json;
use subagents_env::{Env, get, post};

fn run(env: &Env, args: &[&str]) {
    let o = env
        .cmd(args)
        .env_remove("TERN_SEED_AGENTS")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn a_new_install_names_its_default_agent_chief_and_an_existing_one_keeps_its_name() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    run(&env, &["agents", "list"]);
    let (id, slug, name): (String, String, String) = env
        .db()
        .query_row(
            "SELECT id, slug, name FROM agents WHERE id = 'tern'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    // The id never changes; the handle does.
    assert_eq!(
        (id.as_str(), slug.as_str(), name.as_str()),
        ("tern", "chief", "Chief")
    );
    let dm: String = env.query("SELECT title FROM channels WHERE id = 'home'");
    assert_eq!(dm, "Home", "Home keeps its name");

    // An install that existed before keeps `tern`.
    let old = Env::with_config(&json!({ "turns": [] }), "");
    let o = old
        .cmd(&["agents", "new", "Mine", "--template", "marketing"])
        .output()
        .unwrap();
    assert!(o.status.success());
    run(&old, &["agents", "list"]);
    let slug: String = old.query("SELECT slug FROM agents WHERE id = 'tern'");
    assert_eq!(slug, "tern");
}

#[test]
fn the_persona_plans_delegates_and_briefs() {
    let p = tern_core::defaults::PERSONA_MD;
    assert!(p.starts_with("You are the user's Chief of Staff"), "{p}");
    for needle in [
        "hand the rest to the teammate",
        "`brief_team`",
        "`inspect_work`",
    ] {
        assert!(p.contains(needle), "{needle}: {p}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_channel_lead_is_told_who_is_in_which_lane_and_to_split_work() {
    let env = Env::new(&json!({ "conversations": [
        { "matches": "Plan the launch", "turns": [ { "text": "On it." } ] }
    ]}));
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (code, a) = post(
        &s,
        "/v1/agents",
        json!({ "name": "Scout", "persona": "Finds competitor pricing and cites it.\nMore." }),
    )
    .await;
    assert_eq!(code, 201, "{a}");
    let (code, _) = post(&s, "/v1/channels", json!({ "slug": "launch" })).await;
    assert_eq!(code, 201);
    post(
        &s,
        "/v1/channels/launch/agents",
        json!({ "agent": "scout" }),
    )
    .await;

    let (code, sent) = post(
        &s,
        "/v1/channels/launch/posts",
        json!({ "text": "Plan the launch" }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let lead_run = sent["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{lead_run}/wait")).await;
    let sys: String = env.query(&format!(
        "SELECT system_prompt FROM runs WHERE id = '{lead_run}'"
    ));
    assert!(sys.contains("### Your team in #launch"), "{sys}");
    assert!(
        sys.contains("- @scout (Scout): Finds competitor pricing and cites it."),
        "{sys}"
    );
    assert!(sys.contains("`delegate_team`"), "{sys}");
    // Once it has a description, that is its lane.
    let (code, _) = crate::subagents_env::api(&s)
        .patch("http://tern/v1/agents/scout")
        .json(&json!({ "description": "Competitor pricing." }))
        .send()
        .await
        .map(|r| (r.status().as_u16(), ()))
        .unwrap();
    assert_eq!(code, 200);
    let (_, sent) = post(
        &s,
        "/v1/channels/launch/posts",
        json!({ "text": "Plan more" }),
    )
    .await;
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(&s, &format!("/v1/runs/{run}/wait")).await;
    let sys: String = env.query(&format!(
        "SELECT system_prompt FROM runs WHERE id = '{run}'"
    ));
    assert!(
        sys.contains("- @scout (Scout): Competitor pricing."),
        "{sys}"
    );
    assert!(sys.contains("answer once"), "{sys}");
}
