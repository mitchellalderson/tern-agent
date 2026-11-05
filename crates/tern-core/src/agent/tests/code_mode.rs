//! Code auto mode end to end: what runs without asking, what still asks, the
//! `code-mode` ledger rows, the snapshot and its undo.

use std::path::{Path, PathBuf};

use super::*;
use crate::tool::Reach;

/// A stand-in for `shell`: its policy is `auto_code` when the command doesn't
/// contain "push" (what the real classifier and sandbox state decide is
/// covered by their own tests), `unbounded` when the sandbox state says so.
struct FakeShell {
    unbounded: bool,
    ran: AtomicU32,
}

#[async_trait]
impl Tool for FakeShell {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "shell".into(),
            description: "shell".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        Effect::Destructive
    }
    fn policy(&self, _: &ToolCtx, args: &Value) -> ToolPolicy {
        let cmd = args["command"].as_str().unwrap_or_default();
        ToolPolicy::new(Effect::Destructive, false)
            .forced(true)
            .reaching(if self.unbounded {
                Reach::Unbounded
            } else {
                Reach::Workspace
            })
            .unconfined(self.unbounded)
            .auto_code(!self.unbounded && !cmd.contains("push"))
    }
    async fn call(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        self.ran.fetch_add(1, Ordering::SeqCst);
        // `rm <file>` really deletes, so undo has something to bring back.
        if let Some(f) = args["command"].as_str().and_then(|c| c.strip_prefix("rm ")) {
            let _ = std::fs::remove_file(ctx.workspace().join(f));
        }
        ToolResult::ok("done")
    }
}

/// A stand-in for `write_file`: granted inside the workspace.
struct FakeWrite;

#[async_trait]
impl Tool for FakeWrite {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "write".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        Effect::Write
    }
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::granted_write(false).reaching(Reach::Workspace)
    }
    async fn call(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let path = ctx.workspace().join(args["path"].as_str().unwrap_or("x"));
        let _ = std::fs::write(path, args["body"].as_str().unwrap_or(""));
        ToolResult::ok("written")
    }
}

struct Fx {
    h: Harness,
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    checkpoints: PathBuf,
    shell: Arc<FakeShell>,
}

fn fx(unbounded: bool) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("precious.txt"), "keep me").unwrap();
    std::fs::write(ws.join("lib.rs"), "fn old() {}").unwrap();
    Fx {
        h: Harness::new(),
        checkpoints: root.join("checkpoints"),
        ws,
        _tmp: tmp,
        shell: Arc::new(FakeShell {
            unbounded,
            ran: AtomicU32::new(0),
        }),
    }
}

fn agent(f: &Fx, script: Script, asks: Arc<Answer>) -> Agent {
    let mut reg = ToolRegistry::new();
    reg.register(f.shell.clone());
    reg.register(Arc::new(FakeWrite));
    let p: Arc<dyn Provider> = Arc::new(script);
    Agent::new(
        Journal::new(f.h.store.clone()),
        Arc::new(move |_| Ok(p.clone())),
        Arc::new(reg),
        asks,
        AgentConfig {
            retry_base: Duration::from_millis(1),
            max_iterations: 20,
            workspace: f.ws.clone(),
            checkpoints: Some(f.checkpoints.clone()),
            ..AgentConfig::default()
        },
    )
}

fn run_at(f: &Fx, id: &str, autonomy: &str, taint: Option<&str>) -> String {
    let model: ModelRef = "script:m".parse().unwrap();
    let input = Journal::new(f.h.store.clone())
        .to_row(&Message::user_text("go"))
        .unwrap();
    f.h.store
        .create_run(
            &tern_store::NewRun {
                id,
                session_id: &f.h.session,
                trigger: "user",
                model: &model.to_string(),
                system_prompt: "system",
                taint,
                ..tern_store::NewRun::default()
            },
            &input,
        )
        .unwrap();
    f.h.store.set_run_autonomy(id, autonomy).unwrap();
    id.into()
}

fn shell(cmd: &str) -> Reply {
    Reply::Tool("shell", json!({ "command": cmd }))
}

fn mode_rows(f: &Fx) -> Vec<tern_store::LedgerRow> {
    f.h.store
        .ledger(&tern_store::LedgerFilter {
            kind: Some("approval".into()),
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .filter(|r| r.entry.detail.as_ref().and_then(|d| d["reason"].as_str()) == Some("code-mode"))
        .collect()
}

fn git_ok() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok()
}

#[tokio::test]
async fn a_code_run_writes_and_builds_without_asking_and_can_be_undone() {
    if !git_ok() {
        return;
    }
    let f = fx(false);
    let asks = Answer::new(Decision::Deny);
    let script = Script::new(vec![
        Reply::Tool(
            "write_file",
            json!({ "path": "lib.rs", "body": "fn new() {}" }),
        ),
        shell("cargo test"),
        shell("rm precious.txt"),
        Reply::Text("done"),
    ]);
    let a = agent(&f, script, asks.clone());
    let run = run_at(&f, "r1", "code", None);
    a.drive(&run, CancellationToken::new()).await.unwrap();

    // Nobody was asked, and everything ran.
    assert!(asks.asked().is_empty(), "{:?}", asks.asked());
    assert_eq!(f.shell.ran.load(Ordering::SeqCst), 2);
    assert!(!f.ws.join("precious.txt").exists());

    // One ledger row per auto call, each with the reason `code-mode`.
    let rows = mode_rows(&f);
    assert_eq!(rows.len(), 3, "{rows:?}");
    let d = rows[0].entry.detail.as_ref().unwrap();
    assert_eq!(d["decision"], "approved_by_mode");
    assert_eq!(d["mode"], "code");
    assert!(rows.iter().any(|r| r.entry.subject == "shell"
        && r.entry.detail.as_ref().unwrap()["command"] == "cargo test"));

    // One snapshot per run, taken before the first change and in the ledger.
    let cps =
        f.h.store
            .ledger(&tern_store::LedgerFilter {
                kind: Some("code_auto.checkpoint".into()),
                ..Default::default()
            })
            .unwrap();
    assert_eq!(cps.len(), 1);
    let commit = cps[0].entry.detail.as_ref().unwrap()["commit"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(d["checkpoint"], commit.as_str());
    assert_eq!(f.h.store.ledger_verify().unwrap().broken, None);

    // The summary lists the changes; restore puts the folder back.
    let changes = crate::checkpoint::changes(&f.checkpoints, "default", &f.ws, &run).unwrap();
    let names: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    assert!(
        names.contains(&"lib.rs") && names.contains(&"precious.txt"),
        "{names:?}"
    );
    crate::checkpoint::restore(&f.checkpoints, "default", &f.ws, &run).unwrap();
    assert_eq!(read(&f.ws, "precious.txt"), "keep me");
    assert_eq!(read(&f.ws, "lib.rs"), "fn old() {}");
}

fn read(ws: &Path, f: &str) -> String {
    std::fs::read_to_string(ws.join(f)).unwrap()
}

#[tokio::test]
async fn what_the_mode_does_not_cover_still_asks() {
    let f = fx(false);
    let asks = Answer::new(Decision::Deny);
    let script = Script::new(vec![
        shell("git push origin main"),
        shell("cargo test"),
        Reply::Text("done"),
    ]);
    let a = agent(&f, script, asks.clone());
    let run = run_at(&f, "r2", "code", None);
    a.drive(&run, CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].summary.contains("push") || format!("{:?}", asked[0].call).contains("push"));
    assert_eq!(f.shell.ran.load(Ordering::SeqCst), 1, "only cargo test ran");
}

#[tokio::test]
async fn supervised_and_tainted_runs_get_nothing_from_the_mode() {
    for (autonomy, taint) in [("supervised", None), ("code", Some("web page"))] {
        let f = fx(false);
        let asks = Answer::new(Decision::Deny);
        let script = Script::new(vec![shell("cargo test"), Reply::Text("done")]);
        let a = agent(&f, script, asks.clone());
        let run = run_at(&f, "r3", autonomy, taint);
        a.drive(&run, CancellationToken::new()).await.unwrap();
        assert_eq!(asks.asked().len(), 1, "{autonomy} {taint:?} must ask");
        assert_eq!(f.shell.ran.load(Ordering::SeqCst), 0);
        assert!(mode_rows(&f).is_empty());
        assert!(
            !f.checkpoints.exists(),
            "no snapshot when the mode did not run anything"
        );
    }
}

#[tokio::test]
async fn an_unbounded_shell_asks_in_code_mode_but_file_edits_stay_automatic() {
    let f = fx(true);
    let asks = Answer::new(Decision::Deny);
    let script = Script::new(vec![
        Reply::Tool("write_file", json!({ "path": "a.txt", "body": "x" })),
        shell("cargo test"),
        Reply::Text("done"),
    ]);
    let a = agent(&f, script, asks.clone());
    let run = run_at(&f, "r4", "code", None);
    a.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(asks.asked().len(), 1, "the shell asks every time");
    assert_eq!(f.shell.ran.load(Ordering::SeqCst), 0);
    assert!(
        f.ws.join("a.txt").exists(),
        "file edits are still automatic"
    );
    assert_eq!(mode_rows(&f).len(), 1);
}
