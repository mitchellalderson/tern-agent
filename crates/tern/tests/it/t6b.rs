//! T6b end-to-end (docs/personal-agent-tooling-spec.md §8.1, §8.2, §8.4):
//! the activity ledger through the API and the CLI, with a tamper caught
//! by `tern ledger verify`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

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

#[tokio::test(flavor = "multi_thread")]
async fn the_tasks_view_collects_every_source() {
    let env = Env::with_config(
        &json!({ "conversations": [
            { "matches": "Tidy up", "turns": [
                { "text": "Checking.", "tool_calls": [{ "name": "shell",
                    "input": { "command": "echo hi" } }] },
                { "text": "Done." }
            ]}
        ]}),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();

    // A routine (scheduled), one paused by its cap (failed), and the
    // database rows the other sources read.
    let (_, brief) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 7 * * *", "prompt": "Morning brief\nwith the news" }),
    )
    .await;
    let (_, capped) = post(
        &s,
        "/v1/schedules",
        json!({ "cron": "0 9 * * *", "prompt": "Inbox sweep" }),
    )
    .await;
    let capped_id = capped["id"].as_str().unwrap();
    let far = tern_store::now_ms() + 3_600_000;
    env.db()
        .execute_batch(&format!(
            "UPDATE schedules SET paused_until = {far}, paused_reason = 'daily run cap reached'
               WHERE id = '{capped_id}';
             INSERT INTO sessions (id, source, created_at, updated_at, channel_id, title)
               VALUES ('s9', 'api', 0, 0, 'home', 'Old chat');
             INSERT INTO runs (id, session_id, status, trigger, started_at, updated_at, finished_at, error)
               VALUES ('broken', 's9', 'failed', 'user', {now}, {now}, {now}, 'provider said no'),
                      ('waiting', 's9', 'queued', 'user', {now}, {now}, NULL, NULL);
             INSERT INTO outbox (run_id, session_id, call_id, tool, input_json, summary, effect,
                                 created_at, expires_at)
               VALUES ('broken', 's9', 'c1', 'gmail_send', '{{}}', 'gmail_send to bob', 'write',
                       {now}, {far});
             INSERT INTO routine_events (routine_id, event_key, payload, state, created_at)
               VALUES ('{capped_id}', 'e1', '{{}}', 'pending', 0);
             INSERT INTO held_deliveries (target, run_id, label, text, release_at, created_at)
               VALUES ('telegram:1', 'broken', 'Morning brief', 'hi', {far}, 0);",
            now = tern_store::now_ms(),
        ))
        .unwrap();

    // A client listening, so the run's shell call waits for an answer
    // (needs you) while the run is running.
    let mut events = subagents_env::Events::open(&s).await;
    let r = subagents_env::start(&s, "Tidy up").await;
    let run = r["run_id"].as_str().unwrap().to_owned();
    events
        .until(10, |evs| {
            evs.iter().any(|e| e.get("approval_requested").is_some())
        })
        .await;

    let (code, tasks) = get(&s, "/v1/tasks").await;
    assert_eq!(code, 200, "{tasks}");
    let items = tasks["items"].as_array().unwrap();
    let has = |section: &str, kind: &str| {
        items
            .iter()
            .find(|t| t["section"] == section && t["kind"] == kind)
            .unwrap_or_else(|| panic!("no {section}/{kind} in {tasks:#}"))
    };
    let approval = has("needs_you", "approval");
    assert_eq!(approval["run_id"], run.as_str());
    assert!(approval["title"].as_str().unwrap().contains("echo hi"));
    assert_eq!(
        has("needs_you", "outbox")["title"],
        "write · gmail_send to bob"
    );
    assert_eq!(has("running", "run")["run_id"], run.as_str());
    assert_eq!(has("queued", "run")["id"], "waiting");
    assert_eq!(has("queued", "events")["routine_id"], capped_id);
    assert_eq!(has("queued", "delivery")["title"], "Morning brief");
    let scheduled = has("scheduled", "routine");
    assert_eq!(scheduled["routine_id"], brief["id"]);
    assert_eq!(scheduled["title"], "Morning brief");
    assert_eq!(
        has("failed", "routine_paused")["detail"],
        "daily run cap reached"
    );
    let failed = has("failed", "run");
    assert_eq!(failed["detail"], "provider said no");
    assert_eq!(failed["session_id"], "s9");
    assert_eq!(failed["channel_id"], "home");
    assert_eq!(tasks["counts"]["needs_you"], 2);

    // The CLI shows the same.
    let out = env.cmd(&["tasks"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    for heading in [
        "Needs you (2)",
        "Running (1)",
        "Queued (3)",
        "Scheduled (1)",
        "Failed recently (2)",
    ] {
        assert!(text.contains(heading), "{heading}: {text}");
    }
}

const LEGACY: &str = r#"return {
  name = "weather",
  description = "Forecasts",
  capabilities = { http = { "api.weather.example" }, secrets = { "WEATHER_API_KEY" } },
  tools = {
    forecast = {
      description = "Forecast",
      params = { type = "object", properties = {} },
      run = function(ctx, args) return ctx.secret("WEATHER_API_KEY") ~= nil end,
    },
  },
}"#;

/// Every condition `tern security audit` looks for, set up at once.
#[test]
fn the_security_audit_reports_each_condition() {
    let env = Env::with_config(
        &json!({}),
        r#"autonomy = "yolo"

[api]
listen = ["unix", "tcp://0.0.0.0:7420"]
remote_mcp_stdio = true

[webhooks]
listen = ["tcp://127.0.0.1:8712"]

[sandbox]
network = true
confine = "off"

[taint]
egress = "allow"

[gateway.connectors.ntfy]
enabled = true
strangers = "taint"
"#,
    );
    let t = &env.tern_home;
    std::fs::create_dir_all(t.join("connectors")).unwrap();
    std::fs::write(t.join("connectors/weather.luau"), LEGACY).unwrap();
    let secrets = t.join("secrets.toml");
    std::fs::write(&secrets, "").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let store = tern_store::Store::open(&t.join("tern.db")).unwrap();
    store
        .create_device(
            &tern_store::DeviceRow {
                id: "d1".into(),
                name: "old phone".into(),
                platform: None,
                scope: "owner".into(),
                created_at: 0,
                last_seen_at: None,
                last_ip: None,
                revoked_at: None,
            },
            "h",
        )
        .unwrap();
    store
        .add_connection(&tern_store::NewConnection {
            kind: "connector".into(),
            target: "gmail".into(),
            slug: "work".into(),
            auth_method: "oauth".into(),
            auth: Some(json!({ "scopes": ["openid", "https://mail.google.com/"] }).to_string()),
            ..Default::default()
        })
        .unwrap();
    store.add_allowlist("write_file", "*").unwrap();
    store
        .conn()
        .unwrap()
        .execute_batch(
            "UPDATE agents SET egress = 'allow' WHERE id = 'tern';
             INSERT INTO sessions (id, source, created_at, updated_at) VALUES ('s', 'cron', 0, 0);
             INSERT INTO schedules (id, cron, prompt, session_id, next_run_at, trigger_kind, autonomy)
               VALUES ('inbox', '', 'Inbox sweep', 's', 0, 'watch', 'yolo'),
                      ('hook', '', 'Deploy notes', 's', 0, 'webhook', NULL);",
        )
        .unwrap();
    drop(store);

    let out = env.cmd(&["security", "audit", "--json"]).output().unwrap();
    assert!(!out.status.success(), "high findings exit non-zero");
    let report: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)));
    let findings = report["findings"].as_array().unwrap();
    let find = |area: &str, needle: &str| -> &Value {
        findings
            .iter()
            .find(|f| f["area"] == area && f["title"].as_str().unwrap().contains(needle))
            .unwrap_or_else(|| panic!("no {area} finding with {needle:?} in {report:#}"))
    };
    assert_eq!(find("listeners", "tcp://0.0.0.0:7420")["severity"], "low");
    assert_eq!(find("listeners", "remote_mcp_stdio")["severity"], "high");
    assert_eq!(
        find("listeners", "webhooks on tcp://127.0.0.1:8712")["severity"],
        "info"
    );
    assert_eq!(
        find("devices", "old phone (owner scope)")["severity"],
        "low"
    );
    assert_eq!(
        find("autonomy", "autonomy = \"yolo\": nothing asks")["severity"],
        "high"
    );
    assert_eq!(
        find("autonomy", "routine \"Inbox sweep\"")["subject"],
        "inbox"
    );
    assert_eq!(
        find(
            "routines",
            "\"Inbox sweep\" starts tainted from what its watch finds"
        )["severity"],
        "high"
    );
    assert_eq!(
        find("routines", "\"Deploy notes\" starts tainted from a webhook")["severity"],
        "high"
    );
    assert_eq!(
        find("gateways", "ntfy: messages from anyone")["severity"],
        "low"
    );
    assert_eq!(
        find("connectors", "weather reads WEATHER_API_KEY")["severity"],
        "medium"
    );
    assert_eq!(find("sandbox", "network = true")["severity"], "high");
    assert_eq!(find("sandbox", "confine = \"off\"")["severity"], "medium");
    assert_eq!(
        find("connections", "https://mail.google.com/")["severity"],
        "medium"
    );
    assert_eq!(find("egress", "[taint] egress")["severity"], "medium");
    assert_eq!(find("egress", "agent tern")["severity"], "medium");
    assert_eq!(find("secrets", "readable by others")["severity"], "high");
    assert_eq!(find("grants", "write_file")["severity"], "low");
    // Most severe first.
    assert_eq!(findings[0]["severity"], "high");

    let human = env.cmd(&["security", "audit"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("[high  ] autonomy"), "{text}");

    let doctor = env.cmd(&["doctor", "--offline"]).output().unwrap();
    let text = String::from_utf8_lossy(&doctor.stdout);
    assert!(text.contains("\nSecurity\n  [warn] audit: "), "{text}");
    assert!(text.contains("See `tern security audit`."), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_security_audit_over_the_api() {
    let env = Env::with_config(&json!({}), "");
    let _d = env.daemon_with(None);
    let (code, report) = get(&env.socket(), "/v1/security/audit").await;
    assert_eq!(code, 200, "{report}");
    assert_eq!(report["high"], 0, "{report:#}");
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["title"] == "API: the local socket only (no TCP listener)"),
        "{report:#}"
    );
}
