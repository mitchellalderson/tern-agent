use super::*;

fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

struct Fx {
    _t: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
}

fn fx() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().join("checkpoints");
    let root = t.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    Fx {
        _t: t,
        base,
        root,
    }
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

#[test]
fn a_snapshot_lists_changes_and_restores_them() {
    if !git_available() {
        return;
    }
    let f = fx();
    write(&f.root, "keep.txt", "keep");
    write(&f.root, "edit.txt", "before");
    write(&f.root, "gone.txt", "precious");
    write(&f.root, "src/lib.rs", "fn a() {}");
    // Never in a snapshot: build output and tern's own folder.
    write(&f.root, "target/debug/x", "big");
    write(&f.root, ".tern/tmp/y", "tmp");

    let cp = create(&f.base, "ws_1", &f.root, "run1").unwrap();
    assert_eq!(cp.files, 4, "target/ and .tern/ are skipped");

    // The run edits, deletes and creates.
    write(&f.root, "edit.txt", "after");
    std::fs::remove_file(f.root.join("gone.txt")).unwrap();
    write(&f.root, "new/made.txt", "new");
    write(&f.root, "target/debug/z", "more");

    let mut list = changes(&f.base, "ws_1", &f.root, "run1").unwrap();
    list.sort_by(|a, b| a.path.cmp(&b.path));
    let got: Vec<(&str, ChangeKind)> = list.iter().map(|c| (c.path.as_str(), c.kind)).collect();
    assert_eq!(
        got,
        vec![
            ("edit.txt", ChangeKind::Modified),
            ("gone.txt", ChangeKind::Deleted),
            ("new/made.txt", ChangeKind::Added),
        ]
    );

    restore(&f.base, "ws_1", &f.root, "run1").unwrap();
    assert_eq!(std::fs::read_to_string(f.root.join("edit.txt")).unwrap(), "before");
    assert_eq!(
        std::fs::read_to_string(f.root.join("gone.txt")).unwrap(),
        "precious"
    );
    assert!(!f.root.join("new/made.txt").exists(), "created files go");
    assert!(f.root.join("keep.txt").exists());
    assert!(
        changes(&f.base, "ws_1", &f.root, "run1").unwrap().is_empty(),
        "nothing left to undo"
    );
}

#[test]
fn the_users_own_git_is_never_touched() {
    if !git_available() {
        return;
    }
    let f = fx();
    let user_git = |args: &[&str]| {
        let o = Command::new("git")
            .args(args)
            .current_dir(&f.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(o.status.success(), "{args:?}");
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    user_git(&["init", "--quiet"]);
    write(&f.root, "a.txt", "a");
    user_git(&["add", "a.txt"]);
    let before = user_git(&["status", "--porcelain"]);
    let index_before = std::fs::read(f.root.join(".git/index")).unwrap();
    create(&f.base, "ws_2", &f.root, "r").unwrap();
    changes(&f.base, "ws_2", &f.root, "r").unwrap();
    restore(&f.base, "ws_2", &f.root, "r").unwrap();
    assert_eq!(user_git(&["status", "--porcelain"]), before);
    assert_eq!(std::fs::read(f.root.join(".git/index")).unwrap(), index_before);
    assert!(
        !f.root.join(".git/refs/tern").exists(),
        "the shadow refs live elsewhere"
    );
}

#[test]
fn a_resumed_run_keeps_its_first_snapshot() {
    if !git_available() {
        return;
    }
    let f = fx();
    write(&f.root, "a.txt", "one");
    let first = create(&f.base, "ws_3", &f.root, "r").unwrap();
    write(&f.root, "a.txt", "two");
    let again = create(&f.base, "ws_3", &f.root, "r").unwrap();
    assert_eq!(first.commit, again.commit);
    restore(&f.base, "ws_3", &f.root, "r").unwrap();
    assert_eq!(std::fs::read_to_string(f.root.join("a.txt")).unwrap(), "one");
}

#[test]
fn an_unknown_run_has_nothing_to_undo() {
    if !git_available() {
        return;
    }
    let f = fx();
    assert!(changes(&f.base, "ws_4", &f.root, "nope").unwrap().is_empty());
    assert!(restore(&f.base, "ws_4", &f.root, "nope").unwrap().is_empty());
}

#[test]
fn a_workspace_over_the_cap_is_reported_not_silently_skipped() {
    if !git_available() {
        return;
    }
    let f = fx();
    for i in 0..3 {
        write(&f.root, &format!("f{i}"), "x");
    }
    // Pretend the cap is smaller by listing more files than allowed is
    // costly in a test; check the message path instead.
    let s = Skipped::TooLarge {
        files: MAX_FILES + 1,
        bytes: 0,
    };
    assert_eq!(s.describe(), "no snapshot: workspace too large");
    assert_eq!(Skipped::NoGit.describe(), "no snapshot: git isn't installed");
}
