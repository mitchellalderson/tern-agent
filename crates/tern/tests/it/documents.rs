//! Documents end to end (docs/documents-design.md): a scripted model creates
//! and edits a document, the API lists it with its revisions, the file is in
//! the workspace, and a path that escapes it makes nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use crate::subagents_env;

use serde_json::{Value, json};
use subagents_env::{Env, get, post};

async fn say(s: &std::path::Path, dm: &str, text: &str) {
    let (code, sent) = post(
        s,
        &format!("/v1/channels/{dm}/posts"),
        json!({ "text": text }),
    )
    .await;
    assert_eq!(code, 200, "{sent}");
    let run = sent["run_id"].as_str().unwrap().to_owned();
    get(s, &format!("/v1/runs/{run}/wait")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_writes_a_document_the_api_lists_with_its_revisions() {
    let env = Env::with_config(
        &json!({ "turns": [
            { "tool_calls": [{ "name": "create_document", "input": {
                "title": "Plan", "content": "# Plan\n\nstep one\nstep two\n", "path": "documents/plan.md" } }] },
            { "text": "Created it." },
            { "tool_calls": [{ "name": "edit_document", "input": {
                "doc": "documents/plan.md", "base_rev": 1,
                "edits": [{ "find": "step two", "replace": "step 2" }] } }] },
            { "text": "Edited it." },
            { "tool_calls": [{ "name": "create_document", "input": {
                "title": "Escape", "content": "x", "path": "../escaped.md" } }] },
            { "text": "Tried." },
        ] }),
        "",
    );
    let _d = env.daemon_with(None);
    let s = env.socket();
    let (_, a) = post(&s, "/v1/agents", json!({ "name": "Writer" })).await;
    let dm = a["main_channel_id"].as_str().unwrap().to_owned();

    say(&s, &dm, "write a plan").await;
    let (code, list) = get(&s, "/v1/documents").await;
    assert_eq!(code, 200, "{list}");
    let docs = list.as_array().unwrap();
    assert_eq!(docs.len(), 1, "{list}");
    let id = docs[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(docs[0]["title"], "Plan");
    assert_eq!(docs[0]["rev"], 1);
    assert_eq!(docs[0]["kind"], "markdown");
    assert_eq!(docs[0]["tainted"], false);

    say(&s, &dm, "tighten step two").await;
    let (_, doc) = get(&s, &format!("/v1/documents/{id}")).await;
    assert_eq!(doc["rev"], 2);
    assert!(doc["content"].as_str().unwrap().contains("step 2"));

    let (_, revs) = get(&s, &format!("/v1/documents/{id}/revisions")).await;
    let revs = revs.as_array().unwrap();
    assert_eq!(revs.len(), 2);
    assert_eq!(revs[0]["rev"], 2);
    assert_eq!(revs[0]["author_kind"], "agent");
    let (_, old) = get(&s, &format!("/v1/documents/{id}/revisions/1")).await;
    assert!(old["content"].as_str().unwrap().contains("step two"));

    // The file is a real file in the default workspace.
    let file = env.home.join("tern-workspace/documents/plan.md");
    let on_disk =
        std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    assert!(on_disk.contains("step 2"));

    // The ledger has a line per write.
    let kinds: i64 = env.query("SELECT count(*) FROM ledger WHERE kind LIKE 'document.%'");
    assert_eq!(kinds, 2);

    // A path outside the workspace makes no document and no file.
    say(&s, &dm, "write outside").await;
    let (_, list) = get(&s, "/v1/documents").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(!env.home.join("escaped.md").exists());
    assert!(!env.home.join("tern-workspace/../escaped.md").exists());

    let (code, _) = get(&s, "/v1/documents/nope").await;
    assert_eq!(code, 404);
    let (code, none): (u16, Value) = get(&s, "/v1/documents?q=zzz").await;
    assert_eq!(code, 200);
    assert!(none.as_array().unwrap().is_empty());
}
