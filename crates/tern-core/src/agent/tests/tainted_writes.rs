//! Tainted runs and file writes: `write_file` and `edit_file` whose target
//! resolves inside the run's own workspace don't ask because of taint (and
//! are recorded with it); everything else still asks. Escapes by absolute
//! path, `..` and symlinks keep asking.

use std::path::{Path, PathBuf};

use super::*;
use crate::approval::TAINTED_WRITE_REASON;
use crate::workspace::WorkspaceScope;

/// A stand-in for `tern_tools::fs`'s write tools: the same policy (the scope
/// decides `taint_safe`) and a real write through the scope.
pub(super) struct WriteFs {
    pub name: &'static str,
    pub calls: AtomicU32,
}

impl WriteFs {
    pub fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name,
            calls: AtomicU32::new(0),
        })
    }
}

#[async_trait]
impl Tool for WriteFs {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "write".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        Effect::Write
    }
    fn policy(&self, ctx: &ToolCtx, args: &Value) -> ToolPolicy {
        let confined = args["path"]
            .as_str()
            .is_some_and(|p| ctx.scope.confined_write_target(p));
        ToolPolicy::granted_write(false).taint_safe(confined)
    }
    async fn call(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let path = args["path"].as_str().unwrap_or("");
        match ctx.scope.write_file(path, b"written") {
            Ok(()) => ToolResult::ok("wrote"),
            Err(e) => ToolResult::error(e.message().to_owned()),
        }
    }
}

pub(super) struct Dirs {
    _tmp: tempfile::TempDir,
    pub ws: PathBuf,
    pub other: PathBuf,
}

pub(super) fn dirs() -> Dirs {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (ws, other) = (root.join("ws"), root.join("other"));
    std::fs::create_dir_all(ws.join("notes")).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("o.txt"), "original").unwrap();
    Dirs {
        _tmp: tmp,
        ws,
        other,
    }
}

pub(super) fn agent_in(
    h: &Harness,
    ws: &Path,
    script: Arc<Script>,
    asks: Arc<dyn ApprovalHandler>,
    mut reg: ToolRegistry,
    tools: Vec<Arc<dyn Tool>>,
) -> Agent {
    for t in tools {
        reg.register(t);
    }
    let p: Arc<dyn Provider> = script;
    Agent::new(
        Journal::new(h.store.clone()),
        Arc::new(move |_| Ok(p.clone())),
        Arc::new(reg),
        asks,
        AgentConfig {
            retry_base: Duration::from_millis(1),
            max_iterations: 100,
            workspace: ws.to_path_buf(),
            ..AgentConfig::default()
        },
    )
}

fn tainted_run(h: &Harness, id: &str) {
    let _ = h.store.create_session("s", "cli", None);
    let model: ModelRef = "script:m".parse().unwrap();
    let input = Journal::new(h.store.clone())
        .to_row(&Message::user_text("go"))
        .unwrap();
    h.store
        .create_run(
            &tern_store::NewRun {
                id,
                session_id: "s",
                trigger: "user",
                model: &model.to_string(),
                system_prompt: "system",
                taint: Some("result of http_fetch"),
                ..tern_store::NewRun::default()
            },
            &input,
        )
        .unwrap();
}

fn write(path: impl AsRef<Path>) -> Reply {
    Reply::Tool("write_file", json!({"path": path.as_ref()}))
}

fn tainted_write_rows(h: &Harness) -> Vec<serde_json::Value> {
    h.store
        .ledger(&tern_store::LedgerFilter {
            kind: Some("approval".into()),
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .filter_map(|r| r.entry.detail)
        .filter(|d| d["decision"] == TAINTED_WRITE_REASON)
        .collect()
}

#[tokio::test]
async fn a_tainted_run_writes_and_edits_inside_its_workspace_without_asking() {
    let h = Harness::new();
    let d = dirs();
    let wf = WriteFs::new("write_file");
    let ef = WriteFs::new("edit_file");
    let asks = Answer::new(Decision::Deny);
    let script = Arc::new(Script::new(vec![
        write("vs-trigger-outline.md"),
        write(d.ws.join("notes/new/deep.md")),
        Reply::Tool("edit_file", json!({"path": "vs-trigger-outline.md"})),
        Reply::Text("done"),
    ]));
    let a = agent_in(
        &h,
        &d.ws,
        script,
        asks.clone(),
        ToolRegistry::new(),
        vec![wf.clone(), ef.clone()],
    );
    tainted_run(&h, "r");
    a.drive("r", CancellationToken::new()).await.unwrap();
    assert!(asks.asked().is_empty(), "{:?}", asks.asked());
    assert_eq!(wf.calls.load(Ordering::SeqCst), 2);
    assert_eq!(ef.calls.load(Ordering::SeqCst), 1);
    assert!(d.ws.join("vs-trigger-outline.md").exists());
    // Each write is in the ledger with the taint it ran under.
    let rows = tainted_write_rows(&h);
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(
        rows.iter()
            .all(|r| r["taint"] == "result of http_fetch"
                && r["taint_reason"] == "result of http_fetch"),
        "{rows:?}"
    );
}

#[tokio::test]
async fn escapes_by_path_dotdot_and_symlink_still_ask_and_never_write() {
    let h = Harness::new();
    let d = dirs();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(&d.other, d.ws.join("linkdir")).unwrap();
        symlink(d.other.join("o.txt"), d.ws.join("linkfile.txt")).unwrap();
        symlink(&d.other, d.ws.join("notes/inner")).unwrap();
    }
    let wf = WriteFs::new("write_file");
    let asks = Answer::new(Decision::Deny);
    let mut replies = vec![
        // Absolute path outside.
        write(d.other.join("x.txt")),
        // `..` out of the workspace, relative and absolute.
        write("../other/y.txt"),
        write(d.ws.join("notes/../../other/z.txt")),
    ];
    #[cfg(unix)]
    replies.extend([
        // A symlinked folder, a symlinked file, and a link below a folder.
        write("linkdir/s1.txt"),
        write("linkfile.txt"),
        write("notes/inner/s2.txt"),
    ]);
    let expect = replies.len();
    replies.push(Reply::Text("done"));
    let a = agent_in(
        &h,
        &d.ws,
        Arc::new(Script::new(replies)),
        asks.clone(),
        ToolRegistry::new(),
        vec![wf.clone()],
    );
    tainted_run(&h, "r");
    a.drive("r", CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    assert_eq!(asked.len(), expect, "{asked:?}");
    assert!(
        asked
            .iter()
            .all(|r| r.grant.is_none() && r.pattern.is_none())
    );
    assert_eq!(wf.calls.load(Ordering::SeqCst), 0, "denied, so none ran");
    assert_eq!(
        std::fs::read_to_string(d.other.join("o.txt")).unwrap(),
        "original"
    );
    assert!(tainted_write_rows(&h).is_empty());
}

/// Swaps the run's folder for a symlink to another folder, mid-run.
struct SwapForLink {
    ws: PathBuf,
    to: PathBuf,
}

#[async_trait]
impl Tool for SwapForLink {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "swap".into(),
            description: "swap".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }
    async fn call(&self, _: &ToolCtx, _: Value) -> ToolResult {
        #[cfg(unix)]
        {
            let moved = self.ws.with_file_name("ws-moved");
            std::fs::rename(&self.ws, moved).unwrap();
            std::os::unix::fs::symlink(&self.to, &self.ws).unwrap();
        }
        ToolResult::ok("swapped")
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_workspace_swapped_for_a_symlink_out_mid_run_asks_and_writes_nothing() {
    let h = Harness::new();
    let d = dirs();
    let wf = WriteFs::new("write_file");
    let asks = Answer::new(Decision::Deny);
    let a = agent_in(
        &h,
        &d.ws,
        Arc::new(Script::new(vec![
            write("before.md"),
            Reply::Tool("swap", json!({})),
            write("after.md"),
            Reply::Text("done"),
        ])),
        asks.clone(),
        ToolRegistry::new(),
        vec![
            wf.clone(),
            Arc::new(SwapForLink {
                ws: d.ws.clone(),
                to: d.other.clone(),
            }),
        ],
    );
    tainted_run(&h, "r");
    a.drive("r", CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    assert_eq!(
        asked.len(),
        1,
        "only the write after the swap asks: {asked:?}"
    );
    assert_eq!(wf.calls.load(Ordering::SeqCst), 1);
    assert!(!d.other.join("before.md").exists() && !d.other.join("after.md").exists());
}

#[tokio::test]
async fn with_the_setting_off_every_tainted_write_asks() {
    let h = Harness::new();
    let d = dirs();
    let wf = WriteFs::new("write_file");
    let asks = Answer::new(Decision::Deny);
    let mut reg = ToolRegistry::new();
    reg.set_taint_safe_writes(false);
    let a = agent_in(
        &h,
        &d.ws,
        Arc::new(Script::new(vec![write("a.md"), Reply::Text("done")])),
        asks.clone(),
        reg,
        vec![wf.clone()],
    );
    tainted_run(&h, "r");
    a.drive("r", CancellationToken::new()).await.unwrap();
    assert_eq!(asks.asked().len(), 1);
    assert_eq!(wf.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn shell_style_writes_and_other_tools_still_ask_under_taint() {
    let h = Harness::new();
    let d = dirs();
    let wf = WriteFs::new("write_file");
    // A host-granted write that isn't a file write (a shell command, a
    // connector write): `taint_safe` is never set, so taint asks.
    let shell = Arc::new(Counter::with_policy(
        "shell",
        ToolPolicy::granted_write(false),
    ));
    let asks = Answer::new(Decision::Deny);
    let a = agent_in(
        &h,
        &d.ws,
        Arc::new(Script::new(vec![
            Reply::Tool("shell", json!({"command": "echo hi > a.md"})),
            write("fine.md"),
            Reply::Text("done"),
        ])),
        asks.clone(),
        ToolRegistry::new(),
        vec![wf.clone(), shell.clone()],
    );
    tainted_run(&h, "r");
    a.drive("r", CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].call.name, "shell");
    assert_eq!(shell.calls.load(Ordering::SeqCst), 0);
    assert_eq!(wf.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn the_scope_confines_only_plain_workspace_targets() {
    let d = dirs();
    let scope = WorkspaceScope::for_dir(d.ws.clone());
    assert!(scope.confined_write_target("a.md"));
    assert!(scope.confined_write_target("notes/not-yet/a.md"));
    assert!(scope.confined_write_target(d.ws.join("notes/a.md").to_str().unwrap()));
    for bad in ["", "..", "../x", "notes/../../x", "/etc/hosts", "/"] {
        assert!(!scope.confined_write_target(bad), "{bad:?}");
    }
    assert!(!scope.confined_write_target(d.other.join("o.txt").to_str().unwrap()));
    assert!(!scope.confined_write_target("a\0b"));
}
