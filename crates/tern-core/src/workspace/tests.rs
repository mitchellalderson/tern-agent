//! The escape matrix (design §12) at the scope, plus the ported `Guard` cases.

use super::*;

struct Fixture {
    tmp: tempfile::TempDir,
    ws: PathBuf,
    data: PathBuf,
    scope: Arc<WorkspaceScope>,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let ws = base.join("ws");
        let data = base.join(".tern");
        std::fs::create_dir(&ws).unwrap();
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("secrets.toml"), "k = 1").unwrap();
        std::fs::write(base.join("SECRET"), "outside").unwrap();
        std::fs::write(ws.join("inside.txt"), "inside").unwrap();
        let deny = Arc::new(DenySet::for_data_dir(&data));
        let scope = WorkspaceScope::for_dir_guarded(ws.clone(), deny);
        Self {
            tmp,
            ws,
            data,
            scope,
        }
    }

    fn base(&self) -> PathBuf {
        std::fs::canonicalize(self.tmp.path()).unwrap()
    }

    fn read(&self, p: &str) -> ScopeResult<String> {
        self.scope
            .read_file(p, 1 << 20)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    fn outside_unchanged(&self) {
        assert_eq!(
            std::fs::read_to_string(self.base().join("SECRET")).unwrap(),
            "outside"
        );
    }
}

fn kind(r: ScopeResult<impl std::fmt::Debug>) -> Option<Refusal> {
    r.err().and_then(|e| e.refusal())
}

#[test]
fn reads_and_writes_inside() {
    let f = Fixture::new();
    assert_eq!(f.read("inside.txt").unwrap(), "inside");
    f.scope.write_file("a/b/new.txt", b"hello").unwrap();
    assert_eq!(f.read("a/b/new.txt").unwrap(), "hello");
    assert_eq!(
        f.read(f.ws.join("a/b/new.txt").to_str().unwrap()).unwrap(),
        "hello"
    );
    assert_eq!(f.read("./a/../inside.txt").unwrap(), "inside");
    // The rewrite is atomic: no temp files are left behind.
    let names: Vec<_> = f
        .scope
        .list_dir("a/b")
        .unwrap()
        .into_iter()
        .map(|e| e.0)
        .collect();
    assert_eq!(names, ["new.txt"]);
}

#[test]
fn dotdot_forms_are_outside() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.ws.join("a")).unwrap();
    for p in [
        "../SECRET",
        "a/../../SECRET",
        "./a/../../SECRET",
        "../ws/../SECRET",
    ] {
        assert_eq!(kind(f.read(p)), Some(Refusal::Outside), "{p}");
        assert_eq!(
            kind(f.scope.write_file(p, b"x")),
            Some(Refusal::Outside),
            "{p}"
        );
    }
    let abs = format!("{}/a/../../SECRET", f.ws.display());
    assert_eq!(kind(f.read(&abs)), Some(Refusal::Outside), "{abs}");
    assert_eq!(kind(f.scope.list_dir("..")), Some(Refusal::Outside));
    assert_eq!(kind(f.scope.list_dir("a/../..")), Some(Refusal::Outside));
    f.outside_unchanged();
}

#[test]
fn absolute_paths_outside_are_refused() {
    let f = Fixture::new();
    let secret = f.base().join("SECRET");
    for p in [
        "/etc/hosts",
        "/",
        secret.to_str().unwrap(),
        "/proc/self/root/etc/hosts",
        "~",
        "~/x",
    ] {
        assert_eq!(kind(f.read(p)), Some(Refusal::Outside), "{p}");
        assert!(f.scope.write_file(p, b"x").is_err(), "{p}");
    }
    f.outside_unchanged();
}

#[cfg(unix)]
#[test]
fn symlinks_out_are_refused() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let base = f.base();
    symlink(base.join("SECRET"), f.ws.join("file_link")).unwrap();
    symlink(&base, f.ws.join("dir_link")).unwrap();
    symlink("file_link", f.ws.join("chain1")).unwrap();
    symlink("chain1", f.ws.join("chain2")).unwrap();
    symlink("chain2", f.ws.join("chain3")).unwrap();
    symlink("/", f.ws.join("root_link")).unwrap();
    symlink(base.join("nope/new.txt"), f.ws.join("dangling")).unwrap();
    symlink(f.data.join("secrets.toml"), f.ws.join("to_secrets")).unwrap();
    symlink("../SECRET", f.ws.join("rel_out")).unwrap();
    for p in [
        "file_link",
        "dir_link/SECRET",
        "chain3",
        "root_link/etc/hosts",
        "dangling",
        "to_secrets",
        "rel_out",
    ] {
        let r = f.read(p);
        assert!(r.is_err(), "{p} read: {r:?}");
    }
    // A write through a link to a file replaces the link: what it pointed
    // at is untouched.
    for p in ["file_link", "to_secrets", "rel_out", "dangling"] {
        let _ = f.scope.write_file(p, b"pwn");
    }
    f.outside_unchanged();
    assert_eq!(
        std::fs::read_to_string(f.data.join("secrets.toml")).unwrap(),
        "k = 1"
    );
    assert!(!base.join("nope").exists());
    // Writing under a directory link out is refused outright.
    assert!(f.scope.write_file("dir_link/new.txt", b"x").is_err());
    assert!(f.scope.write_file("root_link/tmp/tern-x", b"x").is_err());
    assert!(!base.join("new.txt").exists());
    // A link that stays inside is fine.
    symlink("inside.txt", f.ws.join("good")).unwrap();
    assert_eq!(f.read("good").unwrap(), "inside");
}

#[cfg(unix)]
#[test]
fn hardlinks() {
    let f = Fixture::new();
    // A hardlink to a protected file is the same file under another name.
    std::fs::hard_link(f.data.join("secrets.toml"), f.ws.join("innocent.txt")).unwrap();
    assert_eq!(kind(f.read("innocent.txt")), Some(Refusal::Denied));
    assert_eq!(
        kind(f.scope.write_file("innocent.txt", b"x")),
        Some(Refusal::Denied)
    );
    // Another data file is readable but not writable.
    std::fs::create_dir(f.data.join("skills")).unwrap();
    std::fs::write(f.data.join("skills/a.md"), "x").unwrap();
    std::fs::hard_link(f.data.join("skills/a.md"), f.ws.join("a.md")).unwrap();
    assert_eq!(f.read("a.md").unwrap(), "x");
    assert_eq!(
        kind(f.scope.write_file("a.md", b"y")),
        Some(Refusal::Denied)
    );
    // A hardlink to an unrelated outside file: writing replaces the link and
    // leaves the other name alone.
    let out = f.base().join("OUT");
    std::fs::write(&out, "orig").unwrap();
    std::fs::hard_link(&out, f.ws.join("linked")).unwrap();
    f.scope.write_file("linked", b"new").unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "orig");
    assert_eq!(f.read("linked").unwrap(), "new");
    // Same for two names inside.
    std::fs::write(f.ws.join("x"), "x").unwrap();
    std::fs::hard_link(f.ws.join("x"), f.ws.join("y")).unwrap();
    f.scope.write_file("y", b"yy").unwrap();
    assert_eq!(f.read("x").unwrap(), "x");
}

#[test]
fn the_data_dir_and_secrets_are_denied() {
    let f = Fixture::new();
    // The data directory is outside this folder anyway.
    let s = f.data.join("secrets.toml");
    assert_eq!(kind(f.read(s.to_str().unwrap())), Some(Refusal::Outside));
    // A scope on the data directory's parent has it inside, and the deny
    // set (identity) is what stops it.
    let deny = Arc::new(DenySet::for_data_dir(&f.data));
    let wide = WorkspaceScope::for_dir_guarded(f.base(), deny);
    assert_eq!(
        kind(wide.read_file(".tern/secrets.toml", 100)),
        Some(Refusal::Denied)
    );
    assert_eq!(
        kind(wide.write_file(".tern/skills/x/SKILL.md", b"x")),
        Some(Refusal::Denied)
    );
    assert_eq!(
        kind(wide.write_file(".tern/connectors/x.luau", b"x")),
        Some(Refusal::Denied)
    );
    assert!(wide.write_file("ok.txt", b"x").is_ok());
    // A search can't reach a secret either.
    let mut seen = Vec::new();
    wide.search(".", None, 1 << 20, |p, _| {
        seen.push(p.to_owned());
        true
    })
    .unwrap();
    assert!(
        !seen.iter().any(|p| p.ends_with("secrets.toml")),
        "{seen:?}"
    );
}

fn case_insensitive(p: &Path) -> bool {
    let probe = p.join("CaseProbe");
    std::fs::write(&probe, "").unwrap();
    let r = p.join("caseprobe").exists();
    std::fs::remove_file(&probe).unwrap();
    r
}

#[test]
fn case_variants_of_protected_paths_are_denied() {
    let f = Fixture::new();
    let deny = Arc::new(DenySet::for_data_dir(&f.data));
    let wide = WorkspaceScope::for_dir_guarded(f.base(), deny);
    if !case_insensitive(&f.base()) {
        // Case-sensitive: another spelling is another (missing) file.
        assert!(wide.read_file(".TERN/SECRETS.TOML", 100).is_err());
        assert_eq!(
            wide.read_file(".tern/secrets.toml", 100)
                .unwrap_err()
                .refusal(),
            Some(Refusal::Denied)
        );
        return;
    }
    assert_eq!(
        kind(wide.read_file(".TERN/SECRETS.TOML", 100)),
        Some(Refusal::Denied)
    );
    assert_eq!(
        kind(wide.read_file(".Tern/Secrets.Toml", 100)),
        Some(Refusal::Denied)
    );
    assert_eq!(
        kind(wide.write_file(".Tern/new.txt", b"x")),
        Some(Refusal::Denied)
    );
    // The typed absolute spelling of the root in another case still maps to it.
    let upper = f.base().join("WS").join("inside.txt");
    assert_eq!(f.read(upper.to_str().unwrap()).unwrap(), "inside");
    let up_secret = f.base().join(".TERN/SECRETS.TOML");
    let r = f.scope.read_file(up_secret.to_str().unwrap(), 100);
    assert_eq!(kind(r), Some(Refusal::Outside));
}

#[cfg(target_os = "macos")]
#[test]
fn unicode_normalization_variants_are_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let data = base.join("d\u{e9}");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(data.join("secrets.toml"), "k").unwrap();
    let deny = Arc::new(DenySet::for_data_dir(&data));
    let wide = WorkspaceScope::for_dir_guarded(base, deny);
    assert_eq!(
        kind(wide.read_file("de\u{301}/secrets.toml", 100)),
        Some(Refusal::Denied)
    );
    assert_eq!(
        kind(wide.write_file("de\u{301}/secrets.toml", b"x")),
        Some(Refusal::Denied)
    );
}

#[test]
fn lookalike_and_odd_names_are_ordinary_names() {
    let f = Fixture::new();
    // Fullwidth solidus and full stops are not path syntax.
    for name in [
        "\u{ff0f}etc\u{ff0f}hosts",
        "\u{ff0e}\u{ff0e}/x",
        "a\u{202e}b.txt",
    ] {
        let r = f.scope.write_file(name, b"x");
        assert!(r.is_ok(), "{name:?}: {r:?}");
    }
    f.outside_unchanged();
    assert_eq!(kind(f.read("")), Some(Refusal::Invalid));
    assert_eq!(kind(f.read("a\0b")), Some(Refusal::Invalid));
    // Very long names are an ordinary error, not an escape.
    let long = "x".repeat(5000);
    assert!(f.read(&long).is_err());
}

#[cfg(unix)]
#[test]
fn special_files_are_refused_without_hanging() {
    let f = Fixture::new();
    let fifo = f.ws.join("pipe");
    let status = std::process::Command::new("mkfifo").arg(&fifo).status();
    if status.is_ok_and(|s| s.success()) {
        assert_eq!(kind(f.read("pipe")), Some(Refusal::Special));
    }
    assert_eq!(kind(f.read("/dev/zero")), Some(Refusal::Outside));
    assert!(
        f.scope.read_file(".", 10).is_err(),
        "a directory is not a file"
    );
}

#[test]
fn git_metadata_is_not_writable_in_any_case() {
    let f = Fixture::new();
    for p in [".git/config", ".GIT/config", "sub/.Git/hooks/pre-commit"] {
        assert_eq!(
            kind(f.scope.write_file(p, b"x")),
            Some(Refusal::Denied),
            "{p}"
        );
    }
    assert!(f.scope.write_file(".github/ci.yml", b"x").is_ok());
    #[cfg(unix)]
    {
        std::fs::create_dir(f.ws.join(".git")).unwrap();
        std::os::unix::fs::symlink(".git", f.ws.join("g")).unwrap();
        assert_eq!(
            kind(f.scope.write_file("g/config", b"x")),
            Some(Refusal::Denied)
        );
    }
}

#[test]
fn per_call_identity_check_catches_a_moved_folder() {
    let f = Fixture::new();
    assert!(f.read("inside.txt").is_ok());
    let moved = f.base().join("ws2");
    std::fs::rename(&f.ws, &moved).unwrap();
    assert_eq!(kind(f.read("inside.txt")), Some(Refusal::Moved));
    assert_eq!(kind(f.scope.write_file("x", b"x")), Some(Refusal::Moved));
    // A different directory with the same name does not revive it.
    std::fs::create_dir(&f.ws).unwrap();
    std::fs::write(f.ws.join("inside.txt"), "impostor").unwrap();
    assert_eq!(kind(f.read("inside.txt")), Some(Refusal::Moved));
    assert!(!f.ws.join("x").exists());
}

#[cfg(unix)]
#[test]
fn replacing_the_root_with_a_symlink_is_caught() {
    let f = Fixture::new();
    std::fs::rename(&f.ws, f.base().join("moved")).unwrap();
    std::os::unix::fs::symlink("/", &f.ws).unwrap();
    assert_eq!(kind(f.read("etc/hosts")), Some(Refusal::Moved));
}

#[test]
fn read_only_roots_read_but_never_write() {
    let f = Fixture::new();
    let extra = f.base().join("extra");
    std::fs::create_dir(&extra).unwrap();
    std::fs::write(extra.join("doc.txt"), "docs").unwrap();
    let pin = f.base().join("pinned.txt");
    std::fs::write(&pin, "pinned").unwrap();
    std::fs::write(f.base().join("neighbour.txt"), "no").unwrap();
    let roots = vec![
        Arc::new(ReadRoot::open(&extra).unwrap()),
        Arc::new(ReadRoot::open(&pin).unwrap()),
    ];
    let s = WorkspaceScope::open(
        "default",
        &f.ws,
        None,
        Arc::new(DenySet::default()),
        roots,
        FsFlags::default(),
    )
    .unwrap();
    assert_eq!(
        s.read_file(extra.join("doc.txt").to_str().unwrap(), 100)
            .unwrap(),
        b"docs"
    );
    assert_eq!(s.read_file(pin.to_str().unwrap(), 100).unwrap(), b"pinned");
    assert_eq!(
        kind(s.read_file(f.base().join("neighbour.txt").to_str().unwrap(), 100)),
        Some(Refusal::Outside)
    );
    assert_eq!(
        kind(s.write_file(extra.join("doc.txt").to_str().unwrap(), b"x")),
        Some(Refusal::ReadOnly)
    );
    assert_eq!(
        kind(s.write_file(pin.to_str().unwrap(), b"x")),
        Some(Refusal::ReadOnly)
    );
    assert_eq!(
        std::fs::read_to_string(extra.join("doc.txt")).unwrap(),
        "docs"
    );
    // `..` out of a read root is still outside.
    let esc = format!("{}/../neighbour.txt", extra.display());
    assert_eq!(kind(s.read_file(&esc, 100)), Some(Refusal::Outside));
    assert_eq!(
        s.list_dir(extra.to_str().unwrap()).unwrap(),
        [("doc.txt".to_owned(), false)]
    );
}

#[test]
fn search_honours_ignore_files_and_skips_git() {
    let f = Fixture::new();
    let ws = &f.ws;
    std::fs::create_dir_all(ws.join(".github/workflows")).unwrap();
    std::fs::write(ws.join(".github/workflows/ci.yml"), "needle in ci").unwrap();
    std::fs::create_dir_all(ws.join(".git")).unwrap();
    std::fs::write(ws.join(".git/config"), "needle in meta").unwrap();
    std::fs::create_dir_all(ws.join("sub/build")).unwrap();
    std::fs::write(ws.join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(ws.join("sub/.gitignore"), "build/\n").unwrap();
    std::fs::write(ws.join("ignored.txt"), "needle ignored").unwrap();
    std::fs::write(ws.join("sub/build/out.txt"), "needle built").unwrap();
    std::fs::write(ws.join("sub/keep.txt"), "needle kept").unwrap();
    let mut seen = Vec::new();
    f.scope
        .search(".", None, 1 << 20, |p, b| {
            if String::from_utf8_lossy(b).contains("needle") {
                seen.push(p.to_owned());
            }
            true
        })
        .unwrap();
    seen.sort();
    assert_eq!(seen, [".github/workflows/ci.yml", "sub/keep.txt"]);
    let mut globbed = Vec::new();
    f.scope
        .search(".", Some("*.yml"), 1 << 20, |p, _| {
            globbed.push(p.to_owned());
            true
        })
        .unwrap();
    assert_eq!(globbed, [".github/workflows/ci.yml"]);
}

#[cfg(unix)]
#[test]
fn search_does_not_follow_symlinks() {
    let f = Fixture::new();
    std::os::unix::fs::symlink(f.base(), f.ws.join("up")).unwrap();
    std::os::unix::fs::symlink(f.base().join("SECRET"), f.ws.join("s")).unwrap();
    let mut seen = Vec::new();
    f.scope
        .search(".", None, 1 << 20, |p, b| {
            seen.push((p.to_owned(), b.to_vec()));
            true
        })
        .unwrap();
    assert!(seen.iter().all(|(_, b)| b != b"outside"), "{seen:?}");
}

#[test]
fn narrowing_never_widens() {
    let f = Fixture::new();
    f.scope.write_file("sub/in.txt", b"in").unwrap();
    f.scope.write_file("top.txt", b"top").unwrap();
    let n = f.scope.narrow("sub").unwrap();
    assert_eq!(n.cwd(), f.ws.join("sub"));
    assert_eq!(n.read_file("in.txt", 10).unwrap(), b"in");
    assert_eq!(kind(n.read_file("../top.txt", 10)), Some(Refusal::Outside));
    let abs_top = f.ws.join("top.txt");
    assert_eq!(
        kind(n.read_file(abs_top.to_str().unwrap(), 10)),
        Some(Refusal::Outside)
    );
    assert!(f.scope.narrow("..").is_err());
    assert!(f.scope.narrow("/etc").is_err());
    assert!(f.scope.narrow("top.txt").is_err(), "a file is not a folder");
    assert!(f.scope.narrow("missing").is_err());
    n.write_file("deeper/x.txt", b"x").unwrap();
    let nn = n.narrow("deeper").unwrap();
    assert_eq!(nn.subdir(), Some(Path::new("sub/deeper")));
}

#[test]
fn refusals_reach_the_sink() {
    let f = Fixture::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let scope = WorkspaceScope::open(
        "default",
        &f.ws,
        None,
        Arc::new(DenySet::default()),
        Vec::new(),
        FsFlags::default(),
    )
    .unwrap()
    .with_sink(Arc::new(move |r| s2.lock().unwrap().push(r)));
    let e = scope.read_file("/etc/hosts", 10).unwrap_err();
    let msg = scope.fail("read_file", "/etc/hosts", e);
    assert!(msg.contains("outside this conversation's folder"), "{msg}");
    // An ordinary I/O error is not a refusal.
    let e = scope.read_file("missing.txt", 10).unwrap_err();
    scope.fail("read_file", "missing.txt", e);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].kind, Refusal::Outside);
    assert_eq!(seen[0].tool, "read_file");
}

#[cfg(unix)]
#[test]
fn toctou_a_directory_flipped_to_a_symlink_never_reaches_outside() {
    use std::sync::atomic::AtomicBool;
    let f = Fixture::new();
    let base = f.base();
    std::fs::create_dir(base.join("outdir")).unwrap();
    std::fs::write(base.join("outdir/f"), "outside-secret").unwrap();
    std::fs::create_dir(f.ws.join("d")).unwrap();
    std::fs::write(f.ws.join("d/f"), "inside").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flipper = {
        let ws = f.ws.clone();
        let out = base.join("outdir");
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = std::fs::rename(ws.join("d"), ws.join("d_real"));
                let _ = std::os::unix::fs::symlink(&out, ws.join("d"));
                let _ = std::fs::remove_file(ws.join("d"));
                let _ = std::fs::rename(ws.join("d_real"), ws.join("d"));
            }
        })
    };
    for _ in 0..3000 {
        if let Ok(b) = f.scope.read_file("d/f", 100) {
            assert_ne!(b, b"outside-secret");
        }
        let _ = f.scope.write_file("d/w", b"written");
    }
    stop.store(true, Ordering::Relaxed);
    flipper.join().unwrap();
    assert!(!base.join("outdir/w").exists(), "wrote outside");
}

#[test]
fn shell_env_is_per_workspace_and_inside_it() {
    let f = Fixture::new();
    let env = shell_env(&f.scope);
    let get = |k: &str| {
        env.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert!(get("TMPDIR").starts_with(f.ws.to_str().unwrap()));
    assert!(get("CARGO_TARGET_DIR").starts_with(f.ws.to_str().unwrap()));
    assert!(get("npm_config_cache").contains(".tern/cache"));
    prepare_shell_dirs(&f.scope);
    assert!(f.ws.join(".tern/tmp").is_dir());
    assert!(f.ws.join(".tern/.gitignore").is_file());
}
