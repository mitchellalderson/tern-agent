//! Defaults for a new install (autonomous, fetch from any public site) and
//! that an existing config.toml is not changed by them.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env::Env;
use serde_json::json;

fn get(env: &Env, key: &str) -> String {
    let o = env.cmd(&["config", "get", key]).output().unwrap();
    assert!(
        o.status.success(),
        "{key}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn fresh() -> Env {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    std::fs::remove_file(env.tern_home.join("config.toml")).unwrap();
    env
}

#[test]
fn a_fresh_install_is_autonomous_and_fetches_from_any_public_site() {
    let env = fresh();
    assert_eq!(get(&env, "autonomy"), "autonomous");
    assert_eq!(get(&env, "fetch.allow_domains"), "[]");
    assert_eq!(get(&env, "fetch.taint"), "strict");
    assert_eq!(get(&env, "taint.egress"), "ask");

    // `tern config init` writes the same, explicitly, and the sandbox is on.
    let o = env.cmd(&["config", "init"]).output().unwrap();
    assert!(o.status.success());
    let text = std::fs::read_to_string(env.tern_home.join("config.toml")).unwrap();
    assert!(text.contains("\nautonomy = \"autonomous\""), "{text}");
    assert_eq!(get(&env, "autonomy"), "autonomous");
    assert_eq!(
        get(&env, "sandbox.confine"),
        get(&fresh(), "sandbox.confine")
    );
}

#[test]
fn an_existing_config_keeps_its_autonomy() {
    for (body, want) in [
        ("", "supervised"),
        ("default_model = \"anthropic:x\"\n", "supervised"),
        ("autonomy = \"supervised\"\n", "supervised"),
        ("autonomy = \"readonly\"\n", "readonly"),
        ("autonomy = \"yolo\"\n", "yolo"),
    ] {
        let env = Env::with_config(&json!({ "turns": [] }), body);
        assert_eq!(get(&env, "autonomy"), want, "{body:?}");
        // A fetch allow list someone set is theirs.
        let env = Env::with_config(
            &json!({ "turns": [] }),
            &format!("{body}[fetch]\nallow_domains = [\"example.com\"]\n"),
        );
        assert_eq!(get(&env, "autonomy"), want, "{body:?}");
        assert!(get(&env, "fetch.allow_domains").contains("example.com"));
    }
}
