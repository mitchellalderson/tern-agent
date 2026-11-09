//! The escape matrix (docs/alpha-workspaces.md §12) through the file tools:
//! a sentinel outside the workspace and one inside; every attack runs against
//! every tool. Pass = the outside sentinel unread and unchanged, and the
//! refusal reported to the ledger/event sink.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tern_core::tool::{Tool, ToolCtx};
use tern_core::workspace::{DenySet, FsFlags, RefusalRecord, WorkspaceScope};
use tern_tools::fs::{EditFile, ListDir, ReadFile, SearchFiles, WriteFile};
use tokio_util::sync::CancellationToken;

struct Rig {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    ws: PathBuf,
    ctx: ToolCtx,
    refused: Arc<Mutex<Vec<RefusalRecord>>>,
}

fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let ws = base.join("ws");
    let data = base.join(".tern");
    std::fs::create_dir(&ws).unwrap();
    std::fs::create_dir(&data).unwrap();
    std::fs::write(data.join("secrets.toml"), "token = \"hunter2\"").unwrap();
    std::fs::write(base.join("SECRET"), "outside-sentinel").unwrap();
    std::fs::write(ws.join("inside.txt"), "inside-sentinel").unwrap();
    let refused = Arc::new(Mutex::new(Vec::new()));
    let sink = refused.clone();
    let scope = WorkspaceScope::open(
        "default",
        &ws,
        None,
        Arc::new(DenySet::for_data_dir(&data)),
        Vec::new(),
        FsFlags::default(),
    )
    .unwrap()
    .with_sink(Arc::new(move |r| sink.lock().unwrap().push(r)));
    Rig {
        ctx: ToolCtx {
            scope: Arc::new(scope),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        },
        _tmp: tmp,
        base,
        ws,
        refused,
    }
}

/// Every tool, called with `path`. Returns (tool, is_error, content).
async fn all_tools(ctx: &ToolCtx, path: &str) -> Vec<(&'static str, bool, String)> {
    let calls: Vec<(&'static str, Box<dyn Tool>, Value)> = vec![
        ("read_file", Box::new(ReadFile), json!({ "path": path })),
        ("list_dir", Box::new(ListDir), json!({ "path": path })),
        (
            "search_files",
            Box::new(SearchFiles),
            json!({ "pattern": "sentinel", "path": path }),
        ),
        (
            "write_file",
            Box::new(WriteFile),
            json!({ "path": path, "content": "pwned" }),
        ),
        (
            "edit_file",
            Box::new(EditFile),
            json!({ "path": path, "old_string": "sentinel", "new_string": "pwned" }),
        ),
    ];
    let mut out = Vec::new();
    for (name, tool, args) in calls {
        let r = tool.call(ctx, args).await;
        out.push((name, r.is_error, r.content));
    }
    out
}

fn assert_sealed(r: &Rig, what: &str, results: &[(&str, bool, String)]) {
    for (tool, is_error, content) in results {
        // A write to a symlink *replaces the link* with a file in the
        // workspace; what it pointed at is checked unchanged below.
        assert!(
            *is_error
                || *tool == "write_file"
                // A chain through a link an earlier write replaced.
                || content == "pwned"
                || content == "no matches"
                || content == "(empty directory)",
            "{what}: {tool} succeeded: {content}"
        );
        assert!(
            !content.contains("outside-sentinel") && !content.contains("hunter2"),
            "{what}: {tool} leaked: {content}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(r.base.join("SECRET")).unwrap(),
        "outside-sentinel",
        "{what}: the outside file changed"
    );
    assert_eq!(
        std::fs::read_to_string(r.base.join(".tern/secrets.toml")).unwrap(),
        "token = \"hunter2\"",
        "{what}: the secrets file changed"
    );
}

#[tokio::test]
async fn dotdot_and_absolute_paths_are_refused_by_every_tool() {
    let r = rig();
    std::fs::create_dir(r.ws.join("a")).unwrap();
    let abs_secret = r.base.join("SECRET").display().to_string();
    let cases = [
        "../SECRET".to_owned(),
        "a/../../SECRET".to_owned(),
        "a/..//..".to_owned(),
        "./a/../../SECRET".to_owned(),
        format!("{}/a/../../SECRET", r.ws.display()),
        "a/..".to_owned() + "/../",
        abs_secret,
        "/etc/hosts".to_owned(),
        "/proc/self/root/etc/hosts".to_owned(),
        "~".to_owned(),
        "~/x".to_owned(),
        r.base.join(".tern/secrets.toml").display().to_string(),
    ];
    for p in &cases {
        let res = all_tools(&r.ctx, p).await;
        assert_sealed(&r, p, &res);
        for (tool, is_error, content) in &res {
            if *tool == "list_dir" || *tool == "search_files" || *tool == "read_file" {
                assert!(*is_error, "{p}: {tool}: {content}");
            }
        }
    }
    let seen = r.refused.lock().unwrap();
    assert!(
        seen.len() >= cases.len() * 3,
        "refusals were logged: {}",
        seen.len()
    );
    assert!(seen.iter().all(|x| x.workspace_id == "default"));
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_hardlinks_and_the_data_dir() {
    use std::os::unix::fs::symlink;
    let r = rig();
    symlink(r.base.join("SECRET"), r.ws.join("file_link")).unwrap();
    symlink(&r.base, r.ws.join("dir_link")).unwrap();
    symlink("file_link", r.ws.join("c1")).unwrap();
    symlink("c1", r.ws.join("c2")).unwrap();
    symlink("c2", r.ws.join("c3")).unwrap();
    symlink("/", r.ws.join("root_link")).unwrap();
    symlink(r.base.join(".tern/secrets.toml"), r.ws.join("to_data")).unwrap();
    symlink(r.base.join("missing/x"), r.ws.join("dangling")).unwrap();
    std::fs::hard_link(r.base.join(".tern/secrets.toml"), r.ws.join("hard")).unwrap();
    for p in [
        "file_link",
        "dir_link",
        "dir_link/SECRET",
        "c3",
        "root_link/etc/hosts",
        "root_link",
        "to_data",
        "dangling",
        "hard",
    ] {
        let res = all_tools(&r.ctx, p).await;
        assert_sealed(&r, p, &res);
    }
    assert!(!r.base.join("missing").exists());
    // A hardlink to an unrelated outside file: a write replaces the link and
    // never modifies the other name.
    std::fs::write(r.base.join("OUT"), "orig").unwrap();
    std::fs::hard_link(r.base.join("OUT"), r.ws.join("alias")).unwrap();
    let w = WriteFile
        .call(&r.ctx, json!({"path": "alias", "content": "new"}))
        .await;
    assert!(!w.is_error, "{}", w.content);
    assert_eq!(std::fs::read_to_string(r.base.join("OUT")).unwrap(), "orig");
}

#[tokio::test]
async fn unicode_lookalikes_and_nul_are_plain_names_or_invalid() {
    let r = rig();
    for name in [
        "\u{ff0e}\u{ff0e}/x",
        "\u{ff0f}etc\u{ff0f}passwd",
        "a\u{202e}txt",
    ] {
        let w = WriteFile
            .call(&r.ctx, json!({"path": name, "content": "ok"}))
            .await;
        assert!(!w.is_error, "{name:?}: {}", w.content);
    }
    assert_eq!(
        std::fs::read_to_string(r.base.join("SECRET")).unwrap(),
        "outside-sentinel"
    );
    for p in ["", "a\u{0}b"] {
        let res = all_tools(&r.ctx, p).await;
        assert!(res.iter().all(|(_, e, _)| *e), "{p:?}: {res:?}");
    }
}

#[tokio::test]
async fn git_metadata_and_special_files() {
    let r = rig();
    for p in [".git/config", ".GIT/hooks/pre-commit", "x/.Git/config"] {
        let w = WriteFile
            .call(&r.ctx, json!({"path": p, "content": "x"}))
            .await;
        assert!(
            w.is_error && w.content.contains("git metadata"),
            "{p}: {}",
            w.content
        );
    }
    let d = ReadFile.call(&r.ctx, json!({"path": "."})).await;
    assert!(
        d.is_error && d.content.contains("directory"),
        "{}",
        d.content
    );
    let z = ReadFile.call(&r.ctx, json!({"path": "/dev/zero"})).await;
    assert!(z.is_error);
}

#[tokio::test]
async fn a_moved_folder_stops_every_tool() {
    let r = rig();
    let ok = ReadFile.call(&r.ctx, json!({"path": "inside.txt"})).await;
    assert!(!ok.is_error);
    std::fs::rename(&r.ws, r.base.join("elsewhere")).unwrap();
    let res = all_tools(&r.ctx, "inside.txt").await;
    for (tool, is_error, content) in res {
        assert!(is_error && content.contains("moved"), "{tool}: {content}");
    }
    assert!(
        !r.base
            .join("elsewhere/inside.txt")
            .to_string_lossy()
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(r.base.join("elsewhere/inside.txt")).unwrap(),
        "inside-sentinel"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn racing_a_directory_swap_never_reaches_outside() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let r = rig();
    std::fs::create_dir(r.base.join("outdir")).unwrap();
    std::fs::write(r.base.join("outdir/f"), "outside-sentinel").unwrap();
    std::fs::create_dir(r.ws.join("d")).unwrap();
    std::fs::write(r.ws.join("d/f"), "inside-sentinel").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flip = {
        let (ws, out, stop) = (r.ws.clone(), r.base.join("outdir"), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = std::fs::rename(ws.join("d"), ws.join("d_real"));
                let _ = std::os::unix::fs::symlink(&out, ws.join("d"));
                let _ = std::fs::remove_file(ws.join("d"));
                let _ = std::fs::rename(ws.join("d_real"), ws.join("d"));
            }
        })
    };
    for _ in 0..400 {
        let rd = ReadFile.call(&r.ctx, json!({"path": "d/f"})).await;
        assert!(!rd.content.contains("outside-sentinel"));
        let _ = WriteFile
            .call(&r.ctx, json!({"path": "d/w", "content": "w"}))
            .await;
        let _ = EditFile
            .call(
                &r.ctx,
                json!({"path": "d/f", "old_string": "sentinel", "new_string": "x"}),
            )
            .await;
    }
    stop.store(true, Ordering::Relaxed);
    flip.join().unwrap();
    assert!(!Path::new(&r.base.join("outdir/w")).exists());
    assert_eq!(
        std::fs::read_to_string(r.base.join("outdir/f")).unwrap(),
        "outside-sentinel"
    );
}

#[tokio::test]
async fn read_only_folders_are_readable_and_never_writable() {
    let r = rig();
    let extra = r.base.join("extra");
    std::fs::create_dir(&extra).unwrap();
    std::fs::write(extra.join("doc.txt"), "docs-sentinel").unwrap();
    let scope = WorkspaceScope::open(
        "default",
        &r.ws,
        None,
        Arc::new(DenySet::default()),
        vec![Arc::new(
            tern_core::workspace::ReadRoot::open(&extra).unwrap(),
        )],
        FsFlags::default(),
    )
    .unwrap();
    let ctx = ToolCtx {
        scope: Arc::new(scope),
        run_id: "r".into(),
        session_id: "s".into(),
        cancel: CancellationToken::new(),
    };
    let p = extra.join("doc.txt");
    let read = ReadFile.call(&ctx, json!({"path": p})).await;
    assert_eq!(read.content, "docs-sentinel");
    let found = SearchFiles
        .call(&ctx, json!({"pattern": "docs", "path": extra}))
        .await;
    assert!(found.content.contains("doc.txt"), "{}", found.content);
    for res in [
        WriteFile
            .call(&ctx, json!({"path": p, "content": "x"}))
            .await,
        EditFile
            .call(
                &ctx,
                json!({"path": p, "old_string": "docs", "new_string": "x"}),
            )
            .await,
    ] {
        assert!(
            res.is_error && res.content.contains("read-only"),
            "{}",
            res.content
        );
    }
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "docs-sentinel");
}

#[test]
fn no_file_tool_asks_about_a_path() {
    // The approve-outside path is gone: no policy depends on the arguments.
    let r = rig();
    for t in tern_tools::fs::all() {
        for path in ["inside.txt", "/etc/hosts", "../x"] {
            let p = t.policy(&r.ctx, &json!({ "path": path }));
            assert!(!p.requires_approval, "{} {path}", t.spec().name);
        }
    }
}
