//! Python script grants end to end, through the daemon with a scripted
//! provider and the real OS sandbox: the first `python3 a.py` asks (offering
//! "this run"), the identical second one runs on the grant without asking,
//! and after the script is rewritten the same command asks again. Where the
//! sandbox can't be confirmed active on this machine (a sandbox inside a
//! sandbox), the same flow must instead ask every time and say why.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod subagents_env;

use serde_json::{Value, json};
use subagents_env::{Env, Events, get, post, start};

fn shell(cmd: &str) -> Value {
    json!({ "name": "shell", "input": { "command": cmd } })
}

fn script() -> Value {
    json!({ "turns": [
        { "tool_calls": [shell("python3 a.py x")] },
        { "tool_calls": [shell("python3 a.py x")] },
        { "tool_calls": [{ "name": "write_file", "input": { "path": "a.py", "content": "print('edited')" } }] },
        { "tool_calls": [shell("python3 a.py x")] },
        { "tool_calls": [shell("python3 a.py x | head -3")] },
        { "text": "Done." }
    ] })
}

fn approvals(evs: &[Value]) -> Vec<Value> {
    evs.iter()
        .filter_map(|e| e.get("approval_requested").cloned())
        .collect()
}

async fn next_approval(events: &mut Events, already: usize) -> Value {
    let evs = events.until(40, |evs| approvals(evs).len() > already).await;
    approvals(&evs)[already].clone()
}

async fn answer(sock: &std::path::Path, card: &Value, decision: &str) {
    let (status, _) = post(
        sock,
        &format!("/v1/approvals/{}", card["id"].as_str().unwrap()),
        json!({ "decision": decision }),
    )
    .await;
    assert_eq!(status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_identical_script_runs_on_the_run_grant_and_an_edited_one_asks_again() {
    if !std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let env = Env::new(&script());
    let ws = env.home.join("tern-workspace");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("a.py"), "import sys; print('v1', sys.argv[1:])").unwrap();
    let _daemon = env.daemon_with(None);
    let sock = env.socket();

    // What the desktop shows: is the sandbox confirmed active here?
    let (_, info) = get(&sock, "/v1/info").await;
    let verified = info["sandbox"]["verified"].as_bool().expect("sandbox info");
    assert!(
        info["sandbox"]["detail"].as_str().unwrap().len() > 8,
        "{info}"
    );
    eprintln!("sandbox: {}", info["sandbox"]);

    let mut events = Events::open(&sock).await;
    let started = start(&sock, "analyse").await;
    let run = started["run_id"].as_str().unwrap().to_owned();

    let first = next_approval(&mut events, 0).await;
    assert_eq!(first["tool"], "shell");
    if !verified {
        // Fail closed: no grant offered, the card says why, and every call asks.
        assert!(first.get("grant").is_none_or(Value::is_null), "{first}");
        let notice = first["notice"].as_str().unwrap();
        assert!(
            notice.starts_with("Not auto-approved: the sandbox isn't confirmed active"),
            "{notice}"
        );
        for n in 0..3 {
            let card = next_approval(&mut events, n).await;
            answer(&sock, &card, "once").await;
        }
        let card = next_approval(&mut events, 3).await;
        assert!(card["notice"].as_str().unwrap().contains("pipe, redirect"));
        answer(&sock, &card, "deny").await;
        return;
    }

    // The offer names the script, its version and arguments, and says what
    // it does not cover.
    let label = first["grant"]["label"].as_str().unwrap();
    assert!(
        label.contains("a.py") && label.contains("version") && label.contains("arguments x"),
        "{label}"
    );
    assert!(
        first["notice"]
            .as_str()
            .unwrap()
            .contains("modules next to it won't import")
    );
    answer(&sock, &first, "run").await;

    // The second identical call ran without a card; the write_file didn't ask;
    // the edited script asks again, with a new key.
    let third = next_approval(&mut events, 1).await;
    assert_ne!(third["grant"]["key"], first["grant"]["key"], "{third}");
    answer(&sock, &third, "once").await;

    // A pipe is not the covered shape: it asks, and says why.
    let piped = next_approval(&mut events, 2).await;
    assert!(piped.get("grant").is_none_or(Value::is_null), "{piped}");
    assert!(
        piped["notice"].as_str().unwrap().contains("pipe, redirect"),
        "{piped}"
    );
    answer(&sock, &piped, "deny").await;

    let evs = events
        .until(40, |evs| {
            evs.iter()
                .any(|e| e["type"] == "run_finished" && e["run_id"] == run.as_str())
        })
        .await;
    assert_eq!(approvals(&evs).len(), 3, "first, edited, piped");
    // Nothing standing was stored by a run grant.
    let stored: i64 = env.query("SELECT COUNT(*) FROM approval_allowlist");
    assert_eq!(stored, 0);

    // The ledger shows the auto-run: grant key, script hash, sandbox state.
    let detail: String = env.query(
        "SELECT detail FROM ledger WHERE kind = 'approval' AND detail LIKE '%approved_by_grant%'",
    );
    let d: Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(d["decision"], "approved_by_grant");
    assert!(
        d["grant"]["key"]
            .as_str()
            .unwrap()
            .starts_with("python script a.py#")
    );
    assert_eq!(d["audit"]["sha256"].as_str().unwrap().len(), 64);
    assert!(
        d["audit"]["sandbox"]
            .as_str()
            .unwrap()
            .starts_with("verified")
    );
    assert_eq!(d["audit"]["script"], "a.py");

    // The agent saw v1 on the first two runs and the edit on the third.
    let outputs: Vec<String> = evs
        .iter()
        .filter(|e| e["type"] == "tool_finished" && e["name"] == "shell")
        .filter_map(|e| e["output"].as_str().map(str::to_owned))
        .collect();
    assert!(
        outputs.iter().filter(|o| o.contains("v1 ['x']")).count() == 2,
        "{outputs:?}"
    );
    assert!(outputs.iter().any(|o| o.contains("edited")), "{outputs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_always_grant_is_stored_readably_and_revoked_with_tern_grants() {
    if !std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let env = Env::new(&script());
    let ws = env.home.join("tern-workspace");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("a.py"), "print('v1')").unwrap();
    let _daemon = env.daemon_with(None);
    let sock = env.socket();
    let (_, info) = get(&sock, "/v1/info").await;
    if info["sandbox"]["verified"] != true {
        return; // Covered by the fail-closed branch of the other test.
    }
    let mut events = Events::open(&sock).await;
    start(&sock, "analyse").await;
    let first = next_approval(&mut events, 0).await;
    answer(&sock, &first, "always").await;
    let key = first["grant"]["key"].as_str().unwrap().to_owned();
    let pattern: String = env.query("SELECT pattern FROM approval_allowlist WHERE tool = 'shell'");
    assert_eq!(pattern, format!("key:{key}"));

    let list = env.cmd(&["grants"]).output().unwrap();
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(
        text.contains("python script a.py#") && text.contains("this exact python script"),
        "{text}"
    );
    let rm = env
        .cmd(&["grants", "remove", "shell", &format!("key:{key}")])
        .output()
        .unwrap();
    assert!(
        rm.status.success(),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
    let left: i64 = env.query("SELECT COUNT(*) FROM approval_allowlist");
    assert_eq!(left, 0);
}

/// With the OS sandbox switched off nothing can be verified: the same script
/// asks, offers no grant, and the card says why.
#[tokio::test(flavor = "multi_thread")]
async fn without_a_confirmed_sandbox_the_script_asks_every_time_and_says_why() {
    if !std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let env = Env::with_config(
        &script(),
        "[subagents]\nenabled = true\n[sandbox]\nconfine = \"off\"\n",
    );
    let ws = env.home.join("tern-workspace");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("a.py"), "print('v1')").unwrap();
    let _daemon = env.daemon_with(None);
    let sock = env.socket();
    let (_, info) = get(&sock, "/v1/info").await;
    assert_eq!(info["sandbox"]["verified"], false, "{info}");
    assert!(
        info["sandbox"]["detail"].as_str().unwrap().contains("off"),
        "{info}"
    );
    let mut events = Events::open(&sock).await;
    start(&sock, "analyse").await;
    let first = next_approval(&mut events, 0).await;
    assert!(first.get("grant").is_none_or(Value::is_null), "{first}");
    let notice = first["notice"].as_str().unwrap();
    assert!(
        notice.starts_with("Not auto-approved: the sandbox isn't confirmed active"),
        "{notice}"
    );
    answer(&sock, &first, "once").await;
    // Its output is untrusted without a sandbox, so the run is now tainted
    // and asks again whatever grants exist.
    let second = next_approval(&mut events, 1).await;
    assert!(second.get("grant").is_none_or(Value::is_null), "{second}");
    answer(&sock, &second, "deny").await;
}
