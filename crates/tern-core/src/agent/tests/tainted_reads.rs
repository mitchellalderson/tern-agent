//! Tainted runs and read-only file tools: reads of the workspace never ask,
//! reads outside it ask and may be allowed "for this run" (a folder, the
//! read-only tools, the run's sub-agents), and nothing else is loosened.

use std::path::{Path, PathBuf};

use super::*;
use crate::approval::READ_FS_TOOLS;

/// A stand-in for the file tools: a read asks (`requires_approval`) when its
/// absolute path is outside `ws`, like `tern_tools::fs`.
struct Fs {
    name: &'static str,
    effect: Effect,
    ws: PathBuf,
    calls: AtomicU32,
}

impl Fs {
    fn new(name: &'static str, effect: Effect, ws: &Path) -> Arc<Self> {
        Arc::new(Self {
            name,
            effect,
            ws: ws.canonicalize().unwrap(),
            calls: AtomicU32::new(0),
        })
    }
}

#[async_trait]
impl Tool for Fs {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "fs".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        self.effect
    }
    fn policy(&self, _: &ToolCtx, args: &Value) -> ToolPolicy {
        let p = Path::new(args["path"].as_str().unwrap_or(""));
        let outside = p.is_absolute() && !p.starts_with(&self.ws);
        if self.effect == Effect::Read {
            ToolPolicy::new(Effect::Read, true).forced(outside)
        } else {
            ToolPolicy::granted_write(false).forced(outside)
        }
    }
    async fn call(&self, _: &ToolCtx, _: Value) -> ToolResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolResult::ok("contents")
    }
}

struct Dirs {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    /// A git repository outside the workspace.
    repo: PathBuf,
    /// A plain folder outside the workspace.
    other: PathBuf,
}

fn dirs() -> Dirs {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (ws, repo, other) = (root.join("ws"), root.join("repo"), root.join("other"));
    for d in [
        &ws,
        &repo.join(".git"),
        &repo.join("docs"),
        &repo.join("crates/store"),
        &other,
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    for i in 0..30 {
        std::fs::write(ws.join(format!("f{i}.txt")), "x").unwrap();
    }
    std::fs::write(repo.join("docs/a.md"), "x").unwrap();
    std::fs::write(other.join("o.txt"), "x").unwrap();
    Dirs {
        _tmp: tmp,
        ws,
        repo,
        other,
    }
}

fn agent(h: &Harness, d: &Dirs, script: Script, asks: Arc<Answer>, tools: Vec<Arc<Fs>>) -> Agent {
    let mut reg = ToolRegistry::new();
    for t in tools {
        reg.register(t);
    }
    let p: Arc<dyn Provider> = Arc::new(script);
    Agent::new(
        Journal::new(h.store.clone()),
        Arc::new(move |_| Ok(p.clone())),
        Arc::new(reg),
        asks,
        AgentConfig {
            retry_base: Duration::from_millis(1),
            max_iterations: 100,
            workspace: d.ws.clone(),
            ..AgentConfig::default()
        },
    )
}

fn make_run(h: &Harness, id: &str, session: &str, parent: Option<&str>, taint: Option<&str>) {
    let _ = h.store.create_session(session, "cli", None);
    let model: ModelRef = "script:m".parse().unwrap();
    let input = Journal::new(h.store.clone())
        .to_row(&Message::user_text("go"))
        .unwrap();
    h.store
        .create_run(
            &tern_store::NewRun {
                id,
                session_id: session,
                trigger: "user",
                model: &model.to_string(),
                system_prompt: "system",
                parent_run_id: parent,
                taint,
                ..tern_store::NewRun::default()
            },
            &input,
        )
        .unwrap();
}

fn read(tool: &'static str, path: &Path) -> Reply {
    Reply::Tool(tool, json!({ "path": path }))
}

#[tokio::test]
async fn a_tainted_run_reading_30_workspace_files_asks_nothing_but_outside_and_writes_do() {
    let h = Harness::new();
    let d = dirs();
    let rf = Fs::new("read_file", Effect::Read, &d.ws);
    let wf = Fs::new("write_file", Effect::Write, &d.ws);
    let mut replies: Vec<Reply> = (0..30)
        .map(|i| read("read_file", &d.ws.join(format!("f{i}.txt"))))
        .collect();
    replies.push(read("read_file", &d.other.join("o.txt")));
    replies.push(Reply::Tool(
        "write_file",
        json!({"path": d.ws.join("new.txt")}),
    ));
    replies.push(Reply::Text("done"));
    let asks = Answer::new(Decision::Deny);
    let a = agent(
        &h,
        &d,
        Script::new(replies),
        asks.clone(),
        vec![rf.clone(), wf.clone()],
    );
    make_run(&h, "r", "s1", None, Some("result of http_fetch"));
    a.drive("r", CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    // The outside read and the write, nothing else.
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0].call.name, "read_file");
    assert_eq!(asked[1].call.name, "write_file");
    assert_eq!(rf.calls.load(Ordering::SeqCst), 30);
    assert_eq!(wf.calls.load(Ordering::SeqCst), 0);
    // The outside read is offered "for this run" and nothing wider.
    let g = asked[0].grant.as_ref().expect("a run-only read scope");
    assert!(g.run_only && g.key.starts_with("readscope:"), "{g:?}");
    assert!(asked[0].pattern.is_none());
    assert!(asked[1].grant.is_none() && asked[1].pattern.is_none());
    // The workspace reads are in the ledger once, as an aggregate.
    let rows = h
        .store
        .ledger(&tern_store::LedgerFilter {
            kind: Some("approval".into()),
            ..Default::default()
        })
        .unwrap();
    let notes: Vec<_> = rows
        .iter()
        .filter(|r| {
            r.entry
                .detail
                .as_ref()
                .is_some_and(|x| x["decision"] == crate::approval::TAINTED_READ_REASON)
        })
        .collect();
    assert_eq!(notes.len(), 1);
}

#[tokio::test]
async fn an_untainted_run_never_notes_or_offers_a_read_scope() {
    let h = Harness::new();
    let d = dirs();
    let rf = Fs::new("read_file", Effect::Read, &d.ws);
    let asks = Answer::new(Decision::AllowOnce);
    let script = Script::new(vec![
        read("read_file", &d.ws.join("f1.txt")),
        read("read_file", &d.other.join("o.txt")),
        Reply::Text("done"),
    ]);
    let a = agent(&h, &d, script, asks.clone(), vec![rf]);
    make_run(&h, "r", "s1", None, None);
    a.drive("r", CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].grant.is_none() && asked[0].taint.is_none());
}

#[tokio::test]
async fn allow_for_this_run_covers_the_repo_read_tools_and_sub_agents_only() {
    let h = Harness::new();
    let d = dirs();
    let rf = Fs::new("read_file", Effect::Read, &d.ws);
    let ld = Fs::new("list_dir", Effect::Read, &d.ws);
    let wf = Fs::new("write_file", Effect::Write, &d.ws);
    // Every run (parent and child) does the same, by turn.
    let script = Script::new(vec![
        read("read_file", &d.repo.join("docs/a.md")),
        read("list_dir", &d.repo.join("crates/store")),
        read(
            "list_dir",
            &d.repo.join(".git/../docs").canonicalize().unwrap(),
        ),
        // Another folder: asks again.
        read("read_file", &d.other.join("o.txt")),
        // A write into the granted folder: still asks.
        Reply::Tool("write_file", json!({"path": d.repo.join("docs/w.md")})),
        // A `..` path never gets a scope.
        read("read_file", &d.repo.join("docs/../docs/a.md")),
        Reply::Text("done"),
    ]);
    let asks = Answer::new(Decision::AllowRun);
    let a = agent(&h, &d, script, asks.clone(), vec![rf, ld, wf]);
    make_run(&h, "root", "s1", None, Some("result of http_fetch"));
    a.drive("root", CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    let names: Vec<_> = asked
        .iter()
        .map(|r| (r.call.name.as_str(), r.grant.is_some()))
        .collect();
    assert_eq!(
        names,
        [
            ("read_file", true),   // first read in the repo
            ("read_file", true),   // another folder
            ("write_file", false), // writes always ask
            ("read_file", false),  // `..`: once only
        ],
        "{asked:?}"
    );
    let key = asked[0].grant.as_ref().unwrap().key.clone();
    assert_eq!(
        key,
        crate::approval::read_scope_key(&d.repo.canonicalize().unwrap())
    );

    // A sub-agent of the same run is covered for the repo, not elsewhere.
    make_run(
        &h,
        "child",
        "s2",
        Some("root"),
        Some(tern_store::SPAWNED_BY_TAINTED),
    );
    let before = asked.len();
    a.drive("child", CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    let new: Vec<_> = asked[before..]
        .iter()
        .map(|r| r.call.name.as_str())
        .collect();
    assert_eq!(new, ["write_file", "read_file"], "{asked:?}");

    // Another run tree never inherits it.
    make_run(&h, "stranger", "s3", None, Some("result of http_fetch"));
    let before = asks.asked().len();
    a.drive("stranger", CancellationToken::new()).await.unwrap();
    assert_eq!(asks.asked().len() - before, 4, "{:?}", asks.asked());
    // Nothing standing was stored.
    for t in READ_FS_TOOLS {
        assert!(h.store.allowlist(t).unwrap().is_empty());
    }
}

#[tokio::test]
async fn session_and_always_answers_to_a_read_scope_mean_once() {
    for decision in [Decision::AllowSession, Decision::AlwaysAllow] {
        let h = Harness::new();
        let d = dirs();
        let rf = Fs::new("read_file", Effect::Read, &d.ws);
        let script = Script::new(vec![
            read("read_file", &d.repo.join("docs/a.md")),
            read("read_file", &d.repo.join("docs/a.md")),
            Reply::Text("done"),
        ]);
        let asks = Answer::new(decision);
        let a = agent(&h, &d, script, asks.clone(), vec![rf.clone()]);
        make_run(&h, "r", "s1", None, Some("result of http_fetch"));
        a.drive("r", CancellationToken::new()).await.unwrap();
        assert_eq!(asks.asked().len(), 2, "{decision:?} must not stick");
        assert_eq!(rf.calls.load(Ordering::SeqCst), 2);
        assert!(h.store.allowlist("read_file").unwrap().is_empty());
    }
}

#[test]
fn read_scope_rules() {
    let d = dirs();
    let c = |name: &str, p: &Path| ToolCall {
        id: "c".into(),
        name: name.into(),
        input: json!({ "path": p }),
    };
    let repo = d.repo.canonicalize().unwrap();
    // File and dir in a repo: the repo.
    let (t, s) =
        crate::approval::read_scope(&d.ws, &c("read_file", &repo.join("docs/a.md"))).unwrap();
    assert_eq!((t, s), (repo.join("docs/a.md"), repo.clone()));
    let (_, s) = crate::approval::read_scope(&d.ws, &c("list_dir", &repo.join("crates"))).unwrap();
    assert_eq!(s, repo);
    // Outside any repo: the target's own folder.
    let other = d.other.canonicalize().unwrap();
    let (_, s) = crate::approval::read_scope(&d.ws, &c("read_file", &other.join("o.txt"))).unwrap();
    assert_eq!(s, other);
    // Not a read-only file tool, `..`, empty.
    assert!(
        crate::approval::read_scope(&d.ws, &c("write_file", &repo.join("docs/a.md"))).is_none()
    );
    assert!(
        crate::approval::read_scope(&d.ws, &c("read_file", &repo.join("docs/../docs/a.md")))
            .is_none()
    );
    assert!(crate::approval::read_scope(&d.ws, &c("read_file", Path::new(""))).is_none());
    // A symlink out of the repo resolves to where it points, so a grant for
    // the repo never covers it.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&other, repo.join("docs/link")).unwrap();
        let (t, s) =
            crate::approval::read_scope(&d.ws, &c("read_file", &repo.join("docs/link/o.txt")))
                .unwrap();
        assert_eq!(t, other.join("o.txt"));
        assert!(!t.starts_with(&repo) && s == other);
    }
}
