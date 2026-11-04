//! T6b end-to-end (docs/personal-agent-tooling-spec.md §8.1, §8.2, §8.4):
//! the activity ledger through the API and the CLI, with a tamper caught
//! by `tern ledger verify`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use serde_json::{Value, json};
use subagents_env::{Env, api, get, post};

async fn patch(socket: &std::path::Path, path: &str, body: Value) -> (u16, Value) {
    let r = api(socket)
        .patch(format!("http://tern{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

fn kinds(entries: &Value) -> Vec<String> {
    entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ledger_records_changes_and_verify_catches_a_tamper() {
    let env = Env::with_config(
        &json!({ "conversations": [] }),
        "autonomy = \"autonomous\"\n",
    );
    let mut d = env.daemon_with(None);
    let s = env.socket();

    let (code, sch) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 8 * * *", "prompt": "Sweep the inbox" }),
    )
    .await;
    assert_eq!(code, 201, "{sch}");
    let id = sch["id"].as_str().unwrap();
    let (code, body) = patch(
        &s,
        &format!("/v1/schedules/{id}"),
        json!({ "autonomy": "yolo" }),
    )
    .await;
    assert_eq!(code, 200, "{body}");

    // The config's autonomy (read at start) and the routine's change.
    let (code, all) = get(&s, "/v1/ledger?kind=autonomy").await;
    assert_eq!(code, 200, "{all}");
    assert_eq!(kinds(&all), ["autonomy.routine", "autonomy.config"]);
    assert_eq!(all[1]["summary"], "autonomy is autonomous");
    assert_eq!(all[0]["routine_id"], id);
    let (_, by_routine) = get(&s, &format!("/v1/ledger?routine={id}")).await;
    assert_eq!(kinds(&by_routine), ["autonomy.routine"]);
    let (_, check) = get(&s, "/v1/ledger/verify").await;
    assert_eq!(check["ok"], true, "{check}");

    d.0.kill().unwrap();
    d.0.wait().unwrap();
    let out = env.cmd(&["ledger"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("autonomy.routine"), "{text}");
    let ok = env.cmd(&["ledger", "verify"]).output().unwrap();
    assert!(ok.status.success(), "{ok:?}");

    // Someone edits the database: lift the guard and soften an entry.
    env.db()
        .execute_batch(
            "DROP TRIGGER ledger_no_update;
             UPDATE ledger SET summary = 'nothing happened' WHERE kind = 'autonomy.routine';",
        )
        .unwrap();
    let bad = env.cmd(&["ledger", "verify"]).output().unwrap();
    assert!(!bad.status.success());
    let said = String::from_utf8_lossy(&bad.stdout);
    assert!(said.contains("changed outside tern at entry #"), "{said}");
    let json_out = env.cmd(&["ledger", "verify", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(v["ok"], false);
}

