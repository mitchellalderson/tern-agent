//! Choosing a folder safely (docs/alpha-workspaces.md §5): one function that
//! judges the canonical path, used by the CLI, the API and the check at the
//! start of every run. It returns the words to show the user; nothing here
//! persists anything.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{DenySet, FsFlags, canonicalize_lenient};
use crate::home::user_home;

/// What validation needs to know about this machine and this tern.
#[derive(Debug, Clone)]
pub struct ValidateCtx {
    /// The user's home directory (canonical).
    pub home: Option<PathBuf>,
    /// tern's data directory and protected files.
    pub deny: DenySet,
    /// Directories every sandbox can write (`/tmp`, `$TMPDIR`, ...): not
    /// isolated from each other, so not a workspace.
    pub scratch: Vec<PathBuf>,
    /// Tests (and the integration harness) put workspaces in temp dirs.
    /// Never a user setting.
    pub allow_scratch: bool,
}

impl ValidateCtx {
    /// This machine's home and temp dirs, with `deny` protecting tern's state.
    pub fn for_machine(deny: DenySet) -> Self {
        let canon = |p: PathBuf| canonicalize_lenient(&p);
        let mut scratch = vec![
            canon("/tmp".into()),
            canon("/var/tmp".into()),
            canon("/dev/shm".into()),
            canon("/private/var/folders".into()),
            canon(std::env::temp_dir()),
        ];
        if let Some(t) = std::env::var_os("TMPDIR").filter(|t| !t.is_empty()) {
            scratch.push(canon(PathBuf::from(t)));
        }
        scratch.sort();
        scratch.dedup();
        Self {
            home: user_home().map(canon),
            deny,
            scratch,
            allow_scratch: false,
        }
    }

    pub fn allowing_scratch(mut self) -> Self {
        self.allow_scratch = true;
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    /// The canonical path the folder resolves to (what is stored and what the
    /// sandbox is told). `None` if it doesn't exist.
    pub canonical: Option<String>,
    /// The path as given, when it differs (a shortcut): shown to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut_of: Option<String>,
    /// `dir` or `file`.
    pub kind: String,
    /// Reasons it can't be used. No override.
    pub errors: Vec<String>,
    /// Things to confirm.
    pub warnings: Vec<String>,
    /// For a folder inside a repository: the repository root to use instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_root: Option<String>,
    pub flags: FsFlags,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ino: Option<i64>,
    /// The daemon couldn't open it because the OS refused (macOS Files and
    /// Folders permission): the `denied` status.
    #[serde(default)]
    pub denied: bool,
}

impl Verdict {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty() && self.canonical.is_some()
    }

    /// The message for a refused folder: the first reason.
    pub fn refusal(&self) -> Option<&str> {
        self.errors.first().map(String::as_str)
    }

    fn err(&mut self, s: impl Into<String>) {
        self.errors.push(s.into());
    }
}

const SYSTEM_DIRS: &[&str] = &[
    "/System",
    "/Library",
    "/usr",
    "/bin",
    "/sbin",
    "/etc",
    "/var",
    "/private",
    "/dev",
    "/proc",
    "/sys",
    "/run",
    "/boot",
    "/Applications",
];

const SECRET_HOMES: &[&str] = &[
    ".ssh", ".aws", ".gnupg", ".kube", ".docker", ".config", "Library",
];

/// The macOS message when the daemon isn't allowed into a folder.
pub const TCC_MESSAGE: &str = "macOS didn't let Tern open this folder. Allow it in System Settings > Privacy & Security > Files and Folders.";

fn under(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir)
}

#[cfg(unix)]
fn ident(path: &Path) -> Option<(i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| (m.dev() as i64, m.ino() as i64))
}

#[cfg(not(unix))]
fn ident(_: &Path) -> Option<(i64, i64)> {
    None
}

/// Judge `raw` as a workspace folder (read-write for the agent).
pub fn validate_workspace(raw: &str, ctx: &ValidateCtx) -> Verdict {
    validate(raw, ctx, true)
}

/// Judge `raw` as a read-only extra folder or file: the same rules about
/// where it may be, but a file is fine and so is a folder you can't write.
pub fn validate_read_root(raw: &str, ctx: &ValidateCtx) -> Verdict {
    validate(raw, ctx, false)
}

fn validate(raw: &str, ctx: &ValidateCtx, workspace: bool) -> Verdict {
    let mut v = Verdict {
        kind: "dir".into(),
        ..Verdict::default()
    };
    if raw.trim().is_empty() {
        v.err("Choose a folder.");
        return v;
    }
    if raw.contains('\0') {
        v.err("That path isn't valid.");
        return v;
    }
    let given = crate::home::expand_tilde(Path::new(raw.trim()));
    if !given.is_absolute() {
        v.err("Give the full path of the folder.");
        return v;
    }
    let canon = match std::fs::canonicalize(&given) {
        Ok(c) => c,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                v.denied = true;
                v.err(macos_or(&format!(
                    "Tern isn't allowed to open {}.",
                    given.display()
                )));
            } else {
                v.err(format!("{} doesn't exist.", given.display()));
            }
            return v;
        }
    };
    let Some(canon_str) = canon.to_str() else {
        v.err(
            "That folder's name isn't valid UTF-8, which Tern needs to sandbox commands there. Rename it first.",
        );
        return v;
    };
    v.canonical = Some(canon_str.to_owned());
    if canon != given {
        v.shortcut_of = Some(given.display().to_string());
    }
    let meta = match std::fs::metadata(&canon) {
        Ok(m) => m,
        Err(e) => {
            v.canonical = None;
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                v.denied = true;
                v.err(macos_or(&format!(
                    "Tern isn't allowed to open {}.",
                    canon.display()
                )));
            } else {
                v.err(format!("{} can't be read: {e}.", canon.display()));
            }
            return v;
        }
    };
    if meta.is_file() {
        v.kind = "file".into();
        if workspace {
            v.err("A workspace is a folder, not a file.");
        }
    } else if !meta.is_dir() {
        v.err(format!("{} isn't a folder.", canon.display()));
    }
    if let Some((d, i)) = ident(&canon) {
        v.dev = Some(d);
        v.ino = Some(i);
    }

    rules(&canon, ctx, workspace, &mut v);
    if !v.errors.is_empty() {
        return v;
    }

    if meta.is_dir() {
        probe(&canon, workspace, &mut v);
        if v.errors.is_empty() {
            git_notes(&canon, &mut v);
            sync_notes(&canon, &mut v);
        }
    }
    v
}

fn macos_or(plain: &str) -> String {
    if cfg!(target_os = "macos") {
        TCC_MESSAGE.to_owned()
    } else {
        plain.to_owned()
    }
}

/// The refuse rules (design §5), on the canonical path.
fn rules(canon: &Path, ctx: &ValidateCtx, workspace: bool, v: &mut Verdict) {
    let home = ctx.home.as_deref();
    let in_scratch = ctx.scratch.iter().any(|s| under(canon, s));
    if canon == Path::new("/") {
        v.err("That's the whole disk. Choose a folder for this work.");
        return;
    }
    if let Some(h) = home {
        if canon == h {
            v.err("That's your whole home folder. Choose a folder inside it.");
        } else if h.starts_with(canon) {
            v.err(format!(
                "{} contains your home folder. Choose a folder inside it.",
                canon.display()
            ));
        }
        for s in SECRET_HOMES {
            let secret = h.join(s);
            if under(canon, &secret) {
                v.err(format!(
                    "{} holds credentials or app data and can't be used.",
                    secret.display()
                ));
            }
        }
    }
    if in_scratch {
        if workspace && !ctx.allow_scratch {
            v.err(
                "Temporary folders are shared by every command and cleaned out by the system. Choose a folder of your own.",
            );
        }
    } else {
        for s in SYSTEM_DIRS {
            if under(canon, Path::new(s)) {
                v.err(format!("{s} is a system folder and can't be used."));
            }
        }
        if canon == Path::new("/opt") {
            v.err("/opt itself can't be used; choose a folder inside it.");
        }
        let vol = Path::new("/Volumes");
        if under(canon, vol) {
            let depth = canon
                .strip_prefix(vol)
                .map_or(0, |r| r.components().count());
            if depth <= 1 {
                v.err("That's a whole drive. Choose a folder inside it.");
            }
        }
    }
    // tern's own state, in both directions.
    let dirs = ctx.deny.dirs();
    if ctx.deny.protects_path(canon)
        || dirs.iter().any(|d| d.starts_with(canon))
        || ctx.deny.files().iter().any(|f| f.starts_with(canon))
    {
        v.err("That folder holds Tern's own data (settings, secrets or the database) and can't be used.");
    }
}

/// Look inside as the daemon: can it list the folder (and write, for a
/// workspace)? Records what the filesystem does with names.
fn probe(canon: &Path, workspace: bool, v: &mut Verdict) {
    if let Err(e) = std::fs::read_dir(canon) {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            v.denied = true;
            v.err(macos_or(&format!(
                "Tern isn't allowed to open {}.",
                canon.display()
            )));
        } else {
            v.err(format!("{} can't be opened: {e}.", canon.display()));
        }
        return;
    }
    if !workspace {
        return;
    }
    let tag = format!("{:08x}", fastrand::u32(..));
    let probe = canon.join(format!(".tern-probe-Aé-{tag}"));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            // Case: does the lower-case spelling name the same file?
            let lower = canon.join(format!(".tern-probe-aé-{tag}"));
            v.flags.case_insensitive = lower.exists();
            // Unicode: does the decomposed spelling name the same file?
            let nfd = canon.join(format!(".tern-probe-Ae\u{301}-{tag}"));
            v.flags.normalizes_unicode = nfd.exists();
            let _ = std::fs::remove_file(&probe);
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // Readable but not writable: usable, with the agent read-only.
            v.warnings.push(
                "Tern can read this folder but not change it, so the agent can only read here."
                    .into(),
            );
        }
        Err(e) => v.err(format!("{} can't be written to: {e}.", canon.display())),
    }
}

/// A worktree (`.git` is a file) or a subdirectory of a repository makes git
/// look for files outside the folder.
fn git_notes(canon: &Path, v: &mut Verdict) {
    let dot_git = canon.join(".git");
    if dot_git.is_file() {
        v.warnings.push(
            "This is a git worktree or submodule: git keeps its data in another folder, so git commands that need it won't work inside Tern's sandbox. Using the main repository's folder works better.".into(),
        );
        return;
    }
    for anc in canon.ancestors().skip(1) {
        if anc.join(".git").exists() {
            v.warnings.push(format!(
                "This folder is inside the git repository at {}. git commands look there, outside the folder; using the repository's own folder works better.",
                anc.display()
            ));
            v.suggested_root = anc.to_str().map(str::to_owned);
            return;
        }
    }
}

/// Folders another service syncs: names and times behave less predictably.
fn sync_notes(canon: &Path, v: &mut Verdict) {
    let s = canon.to_string_lossy();
    let synced = [
        "/Library/Mobile Documents/",
        "/Library/CloudStorage/",
        "/Dropbox",
        "/OneDrive",
        "/Google Drive",
    ];
    if synced.iter().any(|m| s.contains(m)) {
        v.warnings.push(
            "This folder looks synced by a cloud service. Files may be evicted or change under Tern while it works.".into(),
        );
    }
}

/// Why a stored workspace can't be used now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unusable {
    /// `missing`, `replaced`, `invalid`, `denied`.
    pub status: &'static str,
    pub detail: String,
}

/// The check at the start of a run: canonicalize again, re-run the refuse
/// rules, compare identity with what was chosen. Returns the canonical path.
pub fn revalidate(
    path: &str,
    dev: Option<i64>,
    ino: Option<i64>,
    ctx: &ValidateCtx,
) -> Result<PathBuf, Unusable> {
    let canon = match std::fs::canonicalize(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(Unusable {
                status: "denied",
                detail: macos_or(&format!("Tern isn't allowed to open {path}.")),
            });
        }
        Err(_) => {
            return Err(Unusable {
                status: "missing",
                detail: format!(
                    "{path} isn't there any more (moved, deleted, or its drive isn't connected)."
                ),
            });
        }
    };
    if !canon.is_dir() {
        return Err(Unusable {
            status: "invalid",
            detail: format!("{} isn't a folder any more.", canon.display()),
        });
    }
    let mut v = Verdict::default();
    rules(
        &canon,
        &ValidateCtx {
            allow_scratch: true,
            ..ctx.clone()
        },
        true,
        &mut v,
    );
    if let Some(e) = v.errors.first() {
        return Err(Unusable {
            status: "invalid",
            detail: e.clone(),
        });
    }
    if canon.to_str() != Some(path) {
        // It resolves somewhere else now (a symlink swapped in, or the old
        // path became a shortcut): judge by identity below, say so.
    }
    if let (Some(d), Some(i)) = (dev, ino)
        && ident(&canon) != Some((d, i))
    {
        return Err(Unusable {
            status: "replaced",
            detail: format!(
                "{} is a different folder from the one that was chosen (replaced, or another drive is mounted there).",
                canon.display()
            ),
        });
    }
    Ok(canon)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rig {
        _tmp: tempfile::TempDir,
        base: PathBuf,
        ctx: ValidateCtx,
    }

    fn rig() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let home = base.join("home");
        let data = home.join(".tern");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("secrets.toml"), "k").unwrap();
        let ctx = ValidateCtx {
            home: Some(home),
            deny: DenySet::for_data_dir(&data),
            // The rig lives in a temp dir, which is "scratch": the harness
            // allows it, so everything else is judged as a real folder is.
            scratch: vec![base.clone()],
            allow_scratch: true,
        };
        Rig {
            _tmp: tmp,
            base,
            ctx,
        }
    }

    fn mk(r: &Rig, rel: &str) -> String {
        let p = r.base.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p.to_str().unwrap().to_owned()
    }

    fn refused(r: &Rig, path: &str) -> String {
        let v = validate_workspace(path, &r.ctx);
        assert!(!v.is_ok(), "{path} should be refused: {v:?}");
        v.refusal().unwrap().to_owned()
    }

    #[test]
    fn a_normal_folder_is_accepted_and_identified() {
        let r = rig();
        let p = mk(&r, "home/code/proj");
        let v = validate_workspace(&p, &r.ctx);
        assert!(v.is_ok(), "{v:?}");
        assert_eq!(v.canonical.as_deref(), Some(p.as_str()));
        assert!(v.dev.is_some() && v.ino.is_some());
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
        // The probe left nothing behind.
        assert_eq!(std::fs::read_dir(&p).unwrap().count(), 0);
    }

    #[test]
    fn root_home_and_its_ancestors_are_refused() {
        let r = rig();
        assert!(refused(&r, "/").contains("whole disk"));
        let home = r.ctx.home.clone().unwrap();
        assert!(refused(&r, home.to_str().unwrap()).contains("home folder"));
        let parent = home.parent().unwrap().to_str().unwrap().to_owned();
        assert!(refused(&r, &parent).contains("contains your home"));
    }

    #[test]
    fn system_volume_and_secret_folders_are_refused() {
        let r = rig();
        for p in [
            "/etc",
            "/usr/bin",
            "/System",
            "/Applications",
            "/opt",
            "/Volumes",
            "/dev",
        ] {
            if Path::new(p).exists() {
                let m = refused(&r, p);
                assert!(!m.is_empty(), "{p}");
            }
        }
        for s in [".ssh", ".aws", ".gnupg", ".config", "Library/Keychains"] {
            let p = mk(&r, &format!("home/{s}"));
            assert!(refused(&r, &p).contains("credentials"), "{s}");
            let inner = mk(&r, &format!("home/{s}/inner"));
            assert!(refused(&r, &inner).contains("credentials"), "{s}/inner");
        }
    }

    #[test]
    fn shared_temp_folders_are_refused_unless_the_harness_allows_them() {
        let r = rig();
        let p = mk(&r, "scratchy/proj");
        let strict = ValidateCtx {
            allow_scratch: false,
            ..r.ctx.clone()
        };
        let v = validate_workspace(&p, &strict);
        assert!(!v.is_ok());
        assert!(v.refusal().unwrap().contains("Temporary"), "{v:?}");
        assert!(validate_workspace(&p, &r.ctx).is_ok());
        // The real machine's /tmp is scratch.
        let machine = ValidateCtx::for_machine(DenySet::default());
        if Path::new("/tmp").exists() {
            let v = validate_workspace("/tmp", &machine);
            assert!(!v.is_ok(), "{v:?}");
        }
    }

    #[test]
    fn terns_own_state_is_refused_in_both_directions_and_through_links() {
        let r = rig();
        let home = r.ctx.home.clone().unwrap();
        let data = home.join(".tern");
        assert!(refused(&r, data.to_str().unwrap()).contains("Tern's own data"));
        let inside = mk(&r, "home/.tern/skills");
        assert!(refused(&r, &inside).contains("Tern's own data"));
        #[cfg(unix)]
        {
            // A folder that is a symlink into the data dir is judged by where
            // it lands.
            let proj = mk(&r, "home/proj");
            let link = format!("{proj}/state");
            std::os::unix::fs::symlink(&data, &link).unwrap();
            let v = validate_workspace(&link, &r.ctx);
            assert!(!v.is_ok(), "{v:?}");
            assert_eq!(v.shortcut_of.as_deref(), Some(link.as_str()));
        }
        // Moving the secrets file elsewhere protects its folder too.
        let other = mk(&r, "home/vault");
        std::fs::write(format!("{other}/secrets.toml"), "k").unwrap();
        let ctx = ValidateCtx {
            deny: r
                .ctx
                .deny
                .clone()
                .also_protect_files([PathBuf::from(&other).join("secrets.toml")]),
            ..r.ctx.clone()
        };
        assert!(!validate_workspace(&other, &ctx).is_ok());
    }

    #[test]
    fn files_missing_paths_and_odd_input() {
        let r = rig();
        let f = r.base.join("home/file.txt");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "x").unwrap();
        assert!(refused(&r, f.to_str().unwrap()).contains("not a file"));
        assert!(
            validate_read_root(f.to_str().unwrap(), &r.ctx).is_ok(),
            "a pinned file is fine read-only"
        );
        assert!(refused(&r, r.base.join("home/nope").to_str().unwrap()).contains("doesn't exist"));
        assert!(refused(&r, "").contains("Choose"));
        assert!(refused(&r, "relative/dir").contains("full path"));
        assert!(refused(&r, "a\0b").contains("isn't valid"));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_folder_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let r = rig();
        let name = std::ffi::OsStr::from_bytes(b"bad-\xff-name");
        let p = r.base.join("home").join(name);
        if std::fs::create_dir_all(&p).is_ok() {
            let m = refused_path(&r, &p);
            assert!(m.contains("UTF-8"), "{m}");
        }
    }

    #[cfg(unix)]
    fn refused_path(r: &Rig, p: &Path) -> String {
        // `validate_workspace` takes text; a non-UTF-8 path can't even be
        // typed, so go through canonicalize the way a symlink to it would.
        let link = r.base.join("home/link-to-bad");
        std::os::unix::fs::symlink(p, &link).unwrap();
        refused(r, link.to_str().unwrap())
    }

    #[test]
    fn a_repository_subfolder_and_a_worktree_warn() {
        let r = rig();
        let repo = mk(&r, "home/repo");
        std::fs::create_dir(format!("{repo}/.git")).unwrap();
        let sub = mk(&r, "home/repo/crates/x");
        let v = validate_workspace(&sub, &r.ctx);
        assert!(v.is_ok());
        assert!(
            v.warnings
                .iter()
                .any(|w| w.contains("inside the git repository"))
        );
        assert_eq!(v.suggested_root.as_deref(), Some(repo.as_str()));
        let wt = mk(&r, "home/wt");
        std::fs::write(format!("{wt}/.git"), "gitdir: /elsewhere").unwrap();
        let v = validate_workspace(&wt, &r.ctx);
        assert!(
            v.warnings.iter().any(|w| w.contains("worktree")),
            "{:?}",
            v.warnings
        );
    }

    #[test]
    fn revalidation_catches_missing_replaced_and_swapped_folders() {
        let r = rig();
        let p = mk(&r, "home/proj");
        let v = validate_workspace(&p, &r.ctx);
        let (d, i) = (v.dev, v.ino);
        assert!(revalidate(&p, d, i, &r.ctx).is_ok());
        // Deleted and recreated: the same name, another directory.
        std::fs::remove_dir(&p).unwrap();
        assert_eq!(revalidate(&p, d, i, &r.ctx).unwrap_err().status, "missing");
        std::fs::create_dir(&p).unwrap();
        // Directory inode numbers can be reused; force a mismatch.
        let wrong = i.map(|n| n + 1_000_003);
        assert_eq!(
            revalidate(&p, d, wrong, &r.ctx).unwrap_err().status,
            "replaced"
        );
        #[cfg(unix)]
        {
            // Swapped for a symlink to `/`.
            std::fs::remove_dir(&p).unwrap();
            std::os::unix::fs::symlink("/", &p).unwrap();
            let e = revalidate(&p, d, i, &r.ctx).unwrap_err();
            assert_eq!(e.status, "invalid", "{e:?}");
            // ... or into the data dir.
            std::fs::remove_file(&p).unwrap();
            let data = r.ctx.home.clone().unwrap().join(".tern");
            std::os::unix::fs::symlink(&data, &p).unwrap();
            assert_eq!(revalidate(&p, d, i, &r.ctx).unwrap_err().status, "invalid");
        }
    }

    #[test]
    fn the_probe_records_how_the_filesystem_treats_names() {
        let r = rig();
        let p = mk(&r, "home/names");
        let v = validate_workspace(&p, &r.ctx);
        let lower_exists = {
            let a = Path::new(&p).join("CaseX");
            std::fs::write(&a, "").unwrap();
            let e = Path::new(&p).join("casex").exists();
            std::fs::remove_file(a).unwrap();
            e
        };
        assert_eq!(v.flags.case_insensitive, lower_exists);
    }
}
