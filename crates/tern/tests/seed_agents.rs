//! First-run starter agents (DECISIONS.md "Seeded starter agents"): a brand
//! new home gets a small set of ordinary agents, once, ever.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use serde_json::json;
use subagents_env::Env;
use tern_core::agents::SEEDED_STARTERS;

fn run(env: &Env, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn slugs(env: &Env) -> Vec<String> {
    let db = env.db();
    let mut stmt = db
        .prepare("SELECT slug FROM agents WHERE id <> 'tern' AND deleted_at IS NULL ORDER BY slug")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn expected() -> Vec<String> {
    let mut v: Vec<String> = SEEDED_STARTERS
        .iter()
        .map(|id| tern_core::agents::slug_for(&tern_core::agents::template(id).unwrap().name))
        .collect();
    v.sort();
    v
}

#[test]
fn a_clean_home_is_seeded_once_with_the_set_and_the_starter_ceilings() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    assert_eq!(SEEDED_STARTERS.len(), 6);
    run(&env, &["agents", "list"]);
    assert_eq!(slugs(&env), expected());
    // Ordinary agents with the starter's ceiling: seo is read-only.
    let seo: String = env.query("SELECT autonomy FROM agents WHERE slug = 'seo'");
    assert_eq!(seo, "readonly");
    let assistant: String = env.query("SELECT autonomy FROM agents WHERE slug = 'assistant'");
    assert_eq!(assistant, "supervised");
    // A second start doesn't add them again.
    run(&env, &["agents", "list"]);
    assert_eq!(slugs(&env), expected());
    let n: i64 = env.query("SELECT COUNT(*) FROM agents");
    assert_eq!(n, 7);
}

#[test]
fn deleting_the_seeded_agents_does_not_bring_them_back() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    run(&env, &["agents", "list"]);
    for slug in expected() {
        run(&env, &["agents", "delete", &slug, "--yes"]);
    }
    assert!(slugs(&env).is_empty());
    run(&env, &["agents", "list"]);
    assert!(slugs(&env).is_empty());
}

#[test]
fn an_existing_home_with_agents_is_left_alone() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    // An agent made while seeding was off (the flag never claimed).
    let o = env
        .cmd(&["agents", "new", "Mine", "--template", "marketing"])
        .output()
        .unwrap();
    assert!(o.status.success());
    run(&env, &["agents", "list"]);
    assert_eq!(slugs(&env), vec!["mine".to_owned()]);
}

#[test]
fn an_upgraded_home_is_never_seeded() {
    let env = Env::with_config(&json!({ "turns": [] }), "");
    // A database at the schema before seeding existed: only the default agent.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tern-store/migrations");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    files.sort();
    let db = env.db();
    let mut applied = 0;
    for f in files {
        if f.file_name().unwrap().to_str().unwrap() >= "0025" {
            break;
        }
        db.execute_batch(&std::fs::read_to_string(f).unwrap())
            .unwrap();
        applied += 1;
    }
    db.pragma_update(None, "user_version", applied).unwrap();
    drop(db);
    run(&env, &["agents", "list"]);
    assert!(slugs(&env).is_empty());
    let flag: String =
        env.query("SELECT value FROM kv WHERE namespace = 'tern' AND key = 'starters_seeded'");
    assert_eq!(flag, "existing install");
}
