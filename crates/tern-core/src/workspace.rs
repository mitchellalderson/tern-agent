//! The folder an agent can reach (docs/alpha-workspaces.md).
//!
//! A run has exactly one [`WorkspaceScope`], frozen when it starts. Every
//! file path a tool is given goes through it, and it resolves the path with
//! `openat`-style handles (`cap-std`) rather than by comparing strings, so
//! there is no gap between checking a path and using it, and no spelling
//! (`..`, a symlink, another case, another Unicode form, a hardlink) that
//! reaches a file outside the folder. Outside the folder the answer is a
//! [`Refusal`], never a question.
//!
//! What this does not bound is the shell: a command can name a path in
//! endless ways. [`ShellBound`] says what the OS sandbox does for it.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

pub mod validate;

use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, MetadataExt, OpenOptions, OpenOptionsExt};
use serde::{Deserialize, Serialize};

use crate::home::expand_tilde;

/// `(device, inode)`: who a file is, whatever it is called.
pub type Ident = (u64, u64);

/// What the OS sandbox does for `shell` in a workspace (design §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellBound {
    /// A verified OS sandbox: writes limited to the folder and temp dirs.
    Bounded,
    /// Docker, or a probe still pending: bounded by mounts, not proven.
    BoundedUnverified,
    /// No OS sandbox: a command runs as the user, the folder is its `cwd`.
    Unbounded,
}

impl ShellBound {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bounded => "bounded",
            Self::BoundedUnverified => "bounded_unverified",
            Self::Unbounded => "unbounded",
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Bounded => 0,
            Self::BoundedUnverified => 1,
            Self::Unbounded => 2,
        }
    }

    fn from_u8(n: u8) -> Self {
        match n {
            0 => Self::Bounded,
            1 => Self::BoundedUnverified,
            _ => Self::Unbounded,
        }
    }
}

/// Why a file call was refused (design §6.2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The path leads outside the folder (and its read-only extras).
    Outside,
    /// A file tern protects (its secrets, database, data directory), or git
    /// metadata for a write.
    Denied,
    /// Not a regular file or directory (a fifo, a device, a socket).
    Special,
    /// The folder was renamed, deleted or replaced while the run held it.
    Moved,
    /// A write aimed at a read-only extra folder.
    ReadOnly,
    /// Not a usable path (empty, NUL).
    Invalid,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Outside => "outside",
            Self::Denied => "denied",
            Self::Special => "special",
            Self::Moved => "moved",
            Self::ReadOnly => "read_only",
            Self::Invalid => "invalid",
        }
    }
}

/// A scope operation that failed: refused by the boundary, or an ordinary
/// I/O error (not found, permissions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeError {
    Refused { kind: Refusal, message: String },
    Io(String),
}

impl ScopeError {
    pub fn message(&self) -> &str {
        match self {
            Self::Refused { message, .. } | Self::Io(message) => message,
        }
    }

    pub fn refusal(&self) -> Option<Refusal> {
        match self {
            Self::Refused { kind, .. } => Some(*kind),
            Self::Io(_) => None,
        }
    }
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ScopeError {}

pub type ScopeResult<T> = Result<T, ScopeError>;

/// One refused call, for the ledger and the run stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusalRecord {
    pub tool: String,
    /// The path as the model wrote it.
    pub path: String,
    pub kind: Refusal,
    pub message: String,
    pub workspace_id: String,
    pub workspace_root: PathBuf,
}

pub type RefusalSink = Arc<dyn Fn(RefusalRecord) + Send + Sync>;

fn ident_of(meta: &cap_std::fs::Metadata) -> Ident {
    (meta.dev(), meta.ino())
}

#[cfg(unix)]
fn ident_at(path: &Path) -> Option<Ident> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn ident_at(_path: &Path) -> Option<Ident> {
    None
}

/// Canonicalize the longest existing ancestor and append the rest.
pub fn canonicalize_lenient(p: &Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&existing) {
            let mut out = canon;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (
            existing.file_name().map(|n| n.to_os_string()),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
}

// ---- the deny set ----

/// What file tools must never touch, decided by identity (`dev`, `ino`)
/// rather than spelling: on a case-insensitive or Unicode-normalizing
/// filesystem `.TERN/SECRETS.TOML` is `.tern/secrets.toml`, and a hardlink is
/// the same file under any name.
#[derive(Debug, Clone, Default)]
pub struct DenySet {
    /// No reads of these files (secrets, the database).
    no_read: Vec<PathBuf>,
    /// No writes anywhere under these (tern's own data directory).
    no_write: Vec<PathBuf>,
}

impl DenySet {
    /// For a data directory: all of it is protected from writes, and its
    /// secrets and database from reads.
    pub fn for_data_dir(dir: &Path) -> Self {
        let dir = canonicalize_lenient(dir);
        let no_read = ["secrets.toml", "tern.db", "tern.db-wal", "tern.db-shm"]
            .iter()
            .map(|f| dir.join(f))
            .collect();
        Self {
            no_write: vec![dir],
            no_read,
        }
    }

    /// Also protect these (a relocated secrets file or database).
    pub fn also_protect_files(mut self, files: impl IntoIterator<Item = PathBuf>) -> Self {
        self.no_read
            .extend(files.into_iter().map(|f| canonicalize_lenient(&f)));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.no_read.is_empty() && self.no_write.is_empty()
    }

    /// The protected directories (canonical), for validation.
    pub fn dirs(&self) -> &[PathBuf] {
        &self.no_write
    }

    /// The protected files (canonical), for validation.
    pub fn files(&self) -> &[PathBuf] {
        &self.no_read
    }

    fn file_ids(&self) -> Vec<Ident> {
        self.no_read.iter().filter_map(|p| ident_at(p)).collect()
    }

    fn dir_ids(&self) -> Vec<Ident> {
        self.no_write.iter().filter_map(|p| ident_at(p)).collect()
    }

    /// Whether the file with this identity may not be read.
    pub fn denies_read(&self, id: Ident) -> bool {
        self.file_ids().contains(&id)
    }

    /// Whether this directory is, or is inside, a protected one, given the
    /// identities of it and each of its ancestors.
    pub fn denies_dir_chain(&self, chain: &[Ident]) -> bool {
        let dirs = self.dir_ids();
        chain.iter().any(|i| dirs.contains(i))
    }

    /// Whether a file (given its identity and link count) is a second name
    /// for a protected file or for any file in the data directory.
    pub fn is_protected_hardlink(&self, id: Ident, nlink: u64) -> bool {
        if nlink < 2 {
            return false;
        }
        if self.file_ids().contains(&id) {
            return true;
        }
        let mut budget = 20_000usize;
        self.no_write
            .iter()
            .any(|d| tree_has_inode(d, id, 6, &mut budget))
    }

    /// Whether `path` (an ambient path, for validation) is, or is inside, a
    /// protected directory or one of the protected files, by identity.
    pub fn protects_path(&self, path: &Path) -> bool {
        let dirs = self.dir_ids();
        let files = self.file_ids();
        path.ancestors()
            .filter_map(ident_at)
            .any(|i| dirs.contains(&i) || files.contains(&i))
    }
}

#[cfg(unix)]
fn tree_has_inode(dir: &Path, id: Ident, depth: u32, budget: &mut usize) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    for e in rd.flatten() {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let Ok(m) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        if m.is_file() && (m.dev(), m.ino()) == id {
            return true;
        }
        if m.is_dir() && depth > 0 && tree_has_inode(&e.path(), id, depth - 1, budget) {
            return true;
        }
    }
    false
}

#[cfg(not(unix))]
fn tree_has_inode(_: &Path, _: Ident, _: u32, _: &mut usize) -> bool {
    false
}

// ---- read-only extra roots ----

/// An extra place the agent may read, added by the user. It has no write
/// methods: the type, not a flag, keeps it read-only.
pub struct ReadRoot {
    path: PathBuf,
    id: Ident,
    kind: ReadRootKind,
}

enum ReadRootKind {
    Dir(Dir),
    /// A single file, pinned: its directory handle and name.
    File {
        parent: Dir,
        name: OsString,
    },
}

impl std::fmt::Debug for ReadRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadRoot")
            .field("path", &self.path)
            .field("file", &self.is_file())
            .finish()
    }
}

impl ReadRoot {
    /// Open `path` (a directory or a single file) as read-only.
    pub fn open(path: &Path) -> ScopeResult<Self> {
        let canon = std::fs::canonicalize(path).map_err(|e| io_err(path, &e))?;
        let meta = std::fs::metadata(&canon).map_err(|e| io_err(&canon, &e))?;
        if meta.is_dir() {
            let dir = Dir::open_ambient_dir(&canon, ambient_authority())
                .map_err(|e| io_err(&canon, &e))?;
            let id = dir
                .dir_metadata()
                .map(|m| ident_of(&m))
                .map_err(|e| io_err(&canon, &e))?;
            Ok(Self {
                path: canon,
                id,
                kind: ReadRootKind::Dir(dir),
            })
        } else if meta.is_file() {
            let (Some(parent), Some(name)) = (
                canon.parent().map(Path::to_path_buf),
                canon.file_name().map(|n| n.to_owned()),
            ) else {
                return Err(ScopeError::Io(format!("{}: not a file", canon.display())));
            };
            let parent_dir = Dir::open_ambient_dir(&parent, ambient_authority())
                .map_err(|e| io_err(&parent, &e))?;
            let id = ident_at(&canon)
                .ok_or_else(|| ScopeError::Io(format!("{}: can't identify", canon.display())))?;
            Ok(Self {
                path: canon,
                id,
                kind: ReadRootKind::File {
                    parent: parent_dir,
                    name,
                },
            })
        } else {
            Err(ScopeError::Io(format!(
                "{}: not a file or directory",
                canon.display()
            )))
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_file(&self) -> bool {
        matches!(self.kind, ReadRootKind::File { .. })
    }

    /// The name it appears under in Python (`/ro/<name>`).
    pub fn mount_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".into())
    }
}

// ---- filesystem facts ----

/// What the folder's filesystem does with names, recorded when the folder is
/// chosen (the stored `fs_flags`). The handle, not these, decides access.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsFlags {
    pub case_insensitive: bool,
    pub normalizes_unicode: bool,
}

fn io_err(path: &Path, e: &std::io::Error) -> ScopeError {
    ScopeError::Io(format!("{}: {}", path.display(), strip_os_code(e)))
}

fn strip_os_code(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

/// Whether a `cap-std` error is the capability refusing an escape.
fn is_escape(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
        && e.to_string().contains("outside of the filesystem")
}

#[cfg(unix)]
const NONBLOCK: i32 = libc::O_NONBLOCK;
#[cfg(not(unix))]
const NONBLOCK: i32 = 0;

const MAX_WALK_ENTRIES: usize = 200_000;

// ---- the scope ----

/// The folder one run may reach, frozen when the run started.
pub struct WorkspaceScope {
    id: String,
    root: PathBuf,
    subdir: Option<PathBuf>,
    cwd: PathBuf,
    /// `None` for a scope that couldn't be opened: every call is `Moved`.
    dir: Option<Dir>,
    unavailable: Option<String>,
    ro_roots: Vec<Arc<ReadRoot>>,
    deny: Arc<DenySet>,
    flags: FsFlags,
    bound: AtomicU8,
    code_mode: AtomicBool,
    sink: Mutex<Option<RefusalSink>>,
}

impl std::fmt::Debug for WorkspaceScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceScope")
            .field("id", &self.id)
            .field("root", &self.root)
            .field("subdir", &self.subdir)
            .finish()
    }
}

/// Where a located path lives.
enum Place<'a> {
    Root,
    Ro(&'a ReadRoot),
}

/// What opening a path found.
pub enum Opened {
    File(File, u64),
    Dir,
}

impl WorkspaceScope {
    /// Open the scope on `root` (the canonical folder), narrowed to `subdir`
    /// (relative, must exist inside it).
    pub fn open(
        id: &str,
        root: &Path,
        subdir: Option<&Path>,
        deny: Arc<DenySet>,
        ro_roots: Vec<Arc<ReadRoot>>,
        flags: FsFlags,
    ) -> ScopeResult<Self> {
        let root = std::fs::canonicalize(root).map_err(|e| moved(root, &e))?;
        let base =
            Dir::open_ambient_dir(&root, ambient_authority()).map_err(|e| moved(&root, &e))?;
        let (dir, cwd, subdir) = match subdir.filter(|s| !s.as_os_str().is_empty()) {
            None => (base, root.clone(), None),
            Some(s) => {
                let dir = base.open_dir(s).map_err(|e| {
                    if is_escape(&e) {
                        refused(
                            Refusal::Outside,
                            format!("{} leaves the folder {}", s.display(), root.display()),
                        )
                    } else {
                        io_err(s, &e)
                    }
                })?;
                let cwd = std::fs::canonicalize(root.join(s)).map_err(|e| io_err(s, &e))?;
                let rel = cwd
                    .strip_prefix(&root)
                    .map(Path::to_path_buf)
                    .map_err(|_| {
                        refused(
                            Refusal::Outside,
                            format!("{} leaves the folder {}", s.display(), root.display()),
                        )
                    })?;
                (dir, cwd, Some(rel))
            }
        };
        Ok(Self {
            id: id.to_owned(),
            root,
            subdir,
            cwd,
            dir: Some(dir),
            unavailable: None,
            ro_roots,
            deny,
            flags,
            bound: AtomicU8::new(ShellBound::Unbounded.to_u8()),
            code_mode: AtomicBool::new(false),
            sink: Mutex::new(None),
        })
    }

    /// A scope on `dir` with nothing protected and no extra roots, for the
    /// default workspace in tests and for tools run outside a run. If the
    /// directory can't be opened every file call answers `Moved`.
    pub fn for_dir(dir: PathBuf) -> Arc<Self> {
        let canon = canonicalize_lenient(&dir);
        match Self::open(
            "default",
            &canon,
            None,
            Arc::new(DenySet::default()),
            Vec::new(),
            FsFlags::default(),
        ) {
            Ok(s) => Arc::new(s),
            Err(e) => Arc::new(Self::closed("default", canon, e.message().to_owned())),
        }
    }

    /// [`WorkspaceScope::for_dir`] with a deny set.
    pub fn for_dir_guarded(dir: PathBuf, deny: Arc<DenySet>) -> Arc<Self> {
        let canon = canonicalize_lenient(&dir);
        match Self::open(
            "default",
            &canon,
            None,
            deny.clone(),
            Vec::new(),
            FsFlags::default(),
        ) {
            Ok(s) => Arc::new(s),
            Err(e) => Arc::new(Self::closed("default", canon, e.message().to_owned())),
        }
    }

    /// A scope whose folder isn't available: tools refuse with the reason.
    pub fn closed(id: &str, root: PathBuf, why: String) -> Self {
        Self {
            id: id.to_owned(),
            cwd: root.clone(),
            root,
            subdir: None,
            dir: None,
            unavailable: Some(why),
            ro_roots: Vec::new(),
            deny: Arc::new(DenySet::default()),
            flags: FsFlags::default(),
            bound: AtomicU8::new(ShellBound::Unbounded.to_u8()),
            code_mode: AtomicBool::new(false),
            sink: Mutex::new(None),
        }
    }

    /// Report refusals to `sink` (the agent's ledger and run stream).
    pub fn with_sink(self, sink: RefusalSink) -> Self {
        if let Ok(mut s) = self.sink.lock() {
            *s = Some(sink);
        }
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The canonical folder, frozen.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The narrowed sub-agent folder, relative to `root`.
    pub fn subdir(&self) -> Option<&Path> {
        self.subdir.as_deref()
    }

    /// Where commands start: the root, or the narrowed folder.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn flags(&self) -> FsFlags {
        self.flags
    }

    pub fn deny(&self) -> &Arc<DenySet> {
        &self.deny
    }

    pub fn is_available(&self) -> bool {
        self.dir.is_some()
    }

    /// The extra read-only roots.
    pub fn read_roots(&self) -> &[Arc<ReadRoot>] {
        &self.ro_roots
    }

    /// What the OS sandbox does for `shell` here. Set by the shell tool from
    /// its backend; `Unbounded` until it has said.
    pub fn shell_bound(&self) -> ShellBound {
        ShellBound::from_u8(self.bound.load(Ordering::Relaxed))
    }

    pub fn set_shell_bound(&self, b: ShellBound) {
        self.bound.store(b.to_u8(), Ordering::Relaxed);
    }

    /// Whether the run is in code auto mode right now (set by the agent
    /// loop before each call): only used to word approval notices.
    pub fn code_mode(&self) -> bool {
        self.code_mode.load(Ordering::Relaxed)
    }

    pub fn set_code_mode(&self, on: bool) {
        self.code_mode.store(on, Ordering::Relaxed);
    }

    // ---- refusals ----

    fn refuse(&self, kind: Refusal, detail: &str) -> ScopeError {
        let folder = self.cwd.display();
        let message = match kind {
            Refusal::Outside => format!(
                "refused: {detail} is outside this conversation's folder, {folder}. Ask the user to add it as a read-only folder or to move the file in; tools can't reach it."
            ),
            Refusal::Denied => format!("refused: {detail} is off limits to file tools"),
            Refusal::Special => {
                format!("refused: {detail} is not a regular file (a device, pipe or socket)")
            }
            Refusal::Moved => match &self.unavailable {
                Some(why) => format!(
                    "refused: this conversation's folder, {folder}, isn't available ({why}). Nothing was done."
                ),
                None => format!(
                    "refused: this conversation's folder, {folder}, was moved, deleted or replaced while the run was using it. Nothing was done."
                ),
            },
            Refusal::ReadOnly => format!(
                "refused: {detail} is in a read-only folder; tools can read it but not change it. Write inside {folder} instead."
            ),
            Refusal::Invalid => format!("refused: {detail}"),
        };
        ScopeError::Refused { kind, message }
    }

    /// Tell the sink about a refusal (the tool names itself and the raw path).
    pub fn note_refusal(&self, tool: &str, raw: &str, e: &ScopeError) {
        let ScopeError::Refused { kind, message } = e else {
            return;
        };
        let sink = self.sink.lock().ok().and_then(|s| s.clone());
        if let Some(sink) = sink {
            sink(RefusalRecord {
                tool: tool.to_owned(),
                path: raw.chars().take(500).collect(),
                kind: *kind,
                message: message.clone(),
                workspace_id: self.id.clone(),
                workspace_root: self.cwd.clone(),
            });
        }
    }

    /// The error text for a tool result, after reporting any refusal.
    pub fn fail(&self, tool: &str, raw: &str, e: ScopeError) -> String {
        self.note_refusal(tool, raw, &e);
        e.message().to_owned()
    }

    // ---- the identity check and path location ----

    /// The folder's handle, after checking it is still the folder that was
    /// chosen: a rename, delete or replace mid-run refuses everything.
    fn live(&self) -> ScopeResult<&Dir> {
        let Some(dir) = self.dir.as_ref() else {
            return Err(self.refuse(Refusal::Moved, ""));
        };
        let held = dir.dir_metadata().map(|m| ident_of(&m)).ok();
        let now = ident_at(&self.cwd);
        if held.is_none() || held != now {
            return Err(self.refuse(Refusal::Moved, ""));
        }
        Ok(dir)
    }

    /// Re-check the folder now (per call), without opening anything.
    pub fn check_alive(&self) -> ScopeResult<()> {
        self.live().map(|_| ())
    }

    /// Decide which handle a path is for, and the path relative to it. This
    /// only *chooses*; the handle makes the security decision.
    fn locate(&self, raw: &str) -> ScopeResult<(Place<'_>, PathBuf)> {
        if raw.is_empty() {
            return Err(self.refuse(Refusal::Invalid, "the path is empty"));
        }
        if raw.contains('\0') {
            return Err(self.refuse(Refusal::Invalid, "the path contains a NUL byte"));
        }
        let given = expand_tilde(Path::new(raw));
        if given.is_relative() {
            return Ok((Place::Root, given));
        }
        let Some(root_id) = self
            .dir
            .as_ref()
            .and_then(|d| d.dir_metadata().ok())
            .map(|m| ident_of(&m))
        else {
            return Err(self.refuse(Refusal::Moved, ""));
        };
        if let Some(rel) = rel_under(root_id, &given) {
            return Ok((Place::Root, rel));
        }
        for ro in &self.ro_roots {
            match &ro.kind {
                ReadRootKind::Dir(_) => {
                    if let Some(rel) = rel_under(ro.id, &given) {
                        return Ok((Place::Ro(ro), rel));
                    }
                }
                ReadRootKind::File { .. } => {
                    if ident_at(&given) == Some(ro.id) {
                        return Ok((Place::Ro(ro), PathBuf::new()));
                    }
                }
            }
        }
        Err(self.refuse(Refusal::Outside, &format!("{raw:?}")))
    }

    fn map_open(&self, raw: &str, rel: &Path, e: std::io::Error) -> ScopeError {
        if is_escape(&e) {
            return self.refuse(Refusal::Outside, &format!("{raw:?}"));
        }
        // A symlink loop, a missing file, permissions: plain errors.
        let _ = rel;
        ScopeError::Io(format!("{raw}: {}", strip_os_code(&e)))
    }

    /// Whether a write to `raw` lands inside this scope's own folder by every
    /// check we can make before running it: the folder is live, the path is
    /// in the folder (not a read-only root), has no `..`, and neither it nor
    /// any folder on the way is a symlink, and the resolved path is not
    /// protected. Used only to decide whether a tainted run must ask; it
    /// never widens what the write itself may do (the handle still decides).
    /// Any doubt answers `false`, which asks.
    pub fn confined_write_target(&self, raw: &str) -> bool {
        if self.live().is_err() {
            return false;
        }
        let Ok((Place::Root, rel)) = self.locate(raw) else {
            return false;
        };
        if rel.as_os_str().is_empty()
            || !rel
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return false;
        }
        let mut walk = self.cwd.clone();
        for part in rel.components() {
            walk.push(part);
            match std::fs::symlink_metadata(&walk) {
                Ok(m) if m.file_type().is_symlink() => return false,
                Ok(_) => {}
                // The rest doesn't exist yet: nothing more to be a link.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Err(_) => return false,
            }
        }
        let resolved = canonicalize_lenient(&self.cwd.join(&rel));
        resolved.starts_with(&self.cwd) && !self.deny.protects_path(&resolved)
    }

    /// Whether a read of `raw` (a path as a command would name it: relative to
    /// the folder, or absolute) stays inside this scope's folder once symlinks
    /// are resolved, is not under a read-only root, and is not protected. Used
    /// to decide that a read-only command needs no question; it opens nothing
    /// and never widens what a tool may read. Any doubt answers `false`.
    pub fn read_target_inside(&self, raw: &str) -> bool {
        if self.live().is_err() || raw.contains('\0') {
            return false;
        }
        let given = Path::new(raw);
        if given
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
            || raw.starts_with('~')
        {
            return false;
        }
        let joined = if given.is_absolute() {
            given.to_path_buf()
        } else {
            self.cwd.join(given)
        };
        let resolved = canonicalize_lenient(&joined);
        resolved.starts_with(&self.cwd) && !self.deny.protects_path(&resolved)
    }

    // ---- reading ----

    /// Open `raw` for reading through the handle: a regular file (not
    /// protected) or a directory.
    pub fn open_read(&self, raw: &str) -> ScopeResult<Opened> {
        let root = self.live()?;
        let (place, rel) = self.locate(raw)?;
        match place {
            Place::Root => self.open_read_in(root, raw, &rel),
            Place::Ro(ro) => match &ro.kind {
                ReadRootKind::Dir(d) => self.open_read_in(d, raw, &rel),
                ReadRootKind::File { parent, name } => {
                    let file = open_nonblocking(parent, Path::new(name))
                        .map_err(|e| self.map_open(raw, Path::new(name), e))?;
                    let meta = file.metadata().map_err(|e| io_err(Path::new(raw), &e))?;
                    if ident_of(&meta) != ro.id {
                        return Err(self.refuse(Refusal::Moved, ""));
                    }
                    self.finish_open(raw, file, &meta)
                }
            },
        }
    }

    fn open_read_in(&self, dir: &Dir, raw: &str, rel: &Path) -> ScopeResult<Opened> {
        let target = if rel.as_os_str().is_empty() {
            Path::new(".")
        } else {
            rel
        };
        let file = open_nonblocking(dir, target).map_err(|e| self.map_open(raw, rel, e))?;
        let meta = file.metadata().map_err(|e| io_err(Path::new(raw), &e))?;
        self.finish_open(raw, file, &meta)
    }

    fn finish_open(
        &self,
        raw: &str,
        file: File,
        meta: &cap_std::fs::Metadata,
    ) -> ScopeResult<Opened> {
        if meta.is_dir() {
            return Ok(Opened::Dir);
        }
        if !meta.is_file() {
            return Err(self.refuse(Refusal::Special, &format!("{raw:?}")));
        }
        if self.deny.denies_read(ident_of(meta)) {
            return Err(self.refuse(Refusal::Denied, &format!("{raw:?}")));
        }
        Ok(Opened::File(file, meta.len()))
    }

    /// Read a regular file, at most `max` bytes; larger is an `Io` error.
    pub fn read_file(&self, raw: &str, max: u64) -> ScopeResult<Vec<u8>> {
        match self.open_read(raw)? {
            Opened::Dir => Err(ScopeError::Io(format!(
                "{raw} is a directory; use list_dir"
            ))),
            Opened::File(_, len) if len > max => Err(ScopeError::Io(format!(
                "{raw} is {len} bytes; too large to read"
            ))),
            Opened::File(mut f, _) => {
                let mut buf = Vec::new();
                Read::by_ref(&mut f)
                    .take(max + 1)
                    .read_to_end(&mut buf)
                    .map_err(|e| io_err(Path::new(raw), &e))?;
                if buf.len() as u64 > max {
                    return Err(ScopeError::Io(format!("{raw}: too large to read")));
                }
                Ok(buf)
            }
        }
    }

    /// The entries of a directory: `(name, is_dir)`, sorted. Symlinked
    /// entries are listed as what they are (not followed).
    pub fn list_dir(&self, raw: &str) -> ScopeResult<Vec<(String, bool)>> {
        let root = self.live()?;
        let (place, rel) = self.locate(raw)?;
        let base = match &place {
            Place::Root => root,
            Place::Ro(ro) => match &ro.kind {
                ReadRootKind::Dir(d) => d,
                ReadRootKind::File { .. } => {
                    return Err(ScopeError::Io(format!("{raw} is a file; use read_file")));
                }
            },
        };
        let target = if rel.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &rel
        };
        let dir = base
            .open_dir(target)
            .map_err(|e| self.map_open(raw, &rel, e))?;
        let mut out = Vec::new();
        for entry in dir
            .entries()
            .map_err(|e| io_err(Path::new(raw), &e))?
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            out.push((name, is_dir));
        }
        out.sort();
        Ok(out)
    }

    /// Visit every readable text-candidate file under `raw` (a directory or
    /// one file), through handles, honouring `.gitignore` and `.ignore`
    /// files and an optional glob. `visit(shown, bytes)` returns `false` to
    /// stop. Files over `max_file` bytes, protected files, special files and
    /// symlinked entries are skipped; `.git` directories are never entered.
    pub fn search(
        &self,
        raw: &str,
        glob: Option<&str>,
        max_file: u64,
        mut visit: impl FnMut(&str, &[u8]) -> bool,
    ) -> ScopeResult<()> {
        let root = self.live()?;
        let (place, rel) = self.locate(raw)?;
        let base = match &place {
            Place::Root => root,
            Place::Ro(ro) => match &ro.kind {
                ReadRootKind::Dir(d) => d,
                ReadRootKind::File { .. } => {
                    // One pinned file.
                    return match self.open_read(raw)? {
                        Opened::File(mut f, len) if len <= max_file => {
                            let mut buf = Vec::new();
                            Read::by_ref(&mut f)
                                .take(max_file)
                                .read_to_end(&mut buf)
                                .map_err(|e| io_err(Path::new(raw), &e))?;
                            let name = ro.mount_name();
                            visit(&name, &buf);
                            Ok(())
                        }
                        _ => Ok(()),
                    };
                }
            },
        };
        let overrides = match glob {
            Some(g) => {
                let mut ov = ignore::overrides::OverrideBuilder::new("");
                ov.add(g)
                    .map_err(|e| ScopeError::Io(format!("invalid glob: {e}")))?;
                Some(
                    ov.build()
                        .map_err(|e| ScopeError::Io(format!("invalid glob: {e}")))?,
                )
            }
            None => None,
        };
        let target = if rel.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &rel
        };
        let meta = base
            .metadata(target)
            .map_err(|e| self.map_open(raw, &rel, e))?;
        if meta.is_file() {
            return match self.open_read(raw)? {
                Opened::File(mut f, len) if len <= max_file => {
                    let mut buf = Vec::new();
                    Read::by_ref(&mut f)
                        .take(max_file)
                        .read_to_end(&mut buf)
                        .map_err(|e| io_err(Path::new(raw), &e))?;
                    let shown = Path::new(raw)
                        .file_name()
                        .map_or_else(|| raw.to_owned(), |n| n.to_string_lossy().into_owned());
                    visit(&shown, &buf);
                    Ok(())
                }
                _ => Ok(()),
            };
        }
        let start = base
            .open_dir(target)
            .map_err(|e| self.map_open(raw, &rel, e))?;
        let mut state = Walk {
            scope: self,
            overrides,
            max_file,
            budget: MAX_WALK_ENTRIES,
            stop: false,
        };
        state.dir(&start, &PathBuf::new(), &mut Vec::new(), &mut visit);
        Ok(())
    }

    // ---- writing ----

    /// Write `data` to `raw`, which must be inside the folder (a read-only
    /// extra folder refuses). Creates parent directories through the handle,
    /// writes a temp file in the target's directory and renames it over the
    /// target, so a symlink or hardlink at the target is replaced, never
    /// followed or modified, and a reader never sees a half-written file.
    pub fn write_file(&self, raw: &str, data: &[u8]) -> ScopeResult<()> {
        let root = self.live()?;
        let (place, rel) = self.locate(raw)?;
        if let Place::Ro(_) = place {
            return Err(self.refuse(Refusal::ReadOnly, &format!("{raw:?}")));
        }
        let Some(name) = rel.file_name().map(|n| n.to_owned()) else {
            return Err(self.refuse(Refusal::Invalid, &format!("{raw:?} doesn't name a file")));
        };
        if matches!(rel.components().next_back(), Some(Component::ParentDir)) {
            return Err(self.refuse(Refusal::Invalid, &format!("{raw:?} doesn't name a file")));
        }
        let parent_rel: PathBuf = rel.parent().map(Path::to_path_buf).unwrap_or_default();
        self.check_write_target(root, raw, &rel, &parent_rel, &name)?;
        if !parent_rel.as_os_str().is_empty() {
            root.create_dir_all(&parent_rel)
                .map_err(|e| self.map_open(raw, &parent_rel, e))?;
        }
        let parent_target = if parent_rel.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent_rel.as_path()
        };
        let parent = root
            .open_dir(parent_target)
            .map_err(|e| self.map_open(raw, &parent_rel, e))?;
        // Re-check what the parent really is now that it exists.
        self.check_parent_handle(root, raw, &parent_rel, &parent)?;
        let mut mode = 0o644u32;
        match parent.symlink_metadata(&name) {
            Ok(m) if m.is_dir() => {
                return Err(ScopeError::Io(format!("{raw} is a directory")));
            }
            Ok(m) if m.is_file() => {
                #[cfg(unix)]
                {
                    use cap_std::fs::PermissionsExt as _;
                    mode = m.permissions().mode() & 0o7777;
                }
            }
            Ok(_) | Err(_) => {}
        }
        let tmp = PathBuf::from(format!(
            ".tern-write-{}-{:08x}.tmp",
            std::process::id(),
            fastrand::u32(..)
        ));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(mode);
        let mut f = parent
            .open_with(&tmp, &opts)
            .map_err(|e| self.map_open(raw, &tmp, e))?;
        let wrote = f.write_all(data).and_then(|()| f.flush());
        #[cfg(unix)]
        if wrote.is_ok() {
            let _ = f.set_permissions(cap_std::fs::Permissions::from_std(
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(mode),
            ));
        }
        drop(f);
        if let Err(e) = wrote {
            let _ = parent.remove_file(&tmp);
            return Err(io_err(Path::new(raw), &e));
        }
        if let Err(e) = parent.rename(&tmp, &parent, &name) {
            let _ = parent.remove_file(&tmp);
            return Err(self.map_open(raw, &rel, e));
        }
        Ok(())
    }

    /// Checks on a write target before anything is created.
    fn check_write_target(
        &self,
        root: &Dir,
        raw: &str,
        rel: &Path,
        parent_rel: &Path,
        name: &std::ffi::OsStr,
    ) -> ScopeResult<()> {
        // Git reads `.git/config` and hooks as code (`core.fsmonitor`), so a
        // file tool writing there is a way to run commands. Checked on the
        // typed path and on the path with symlinks resolved.
        let git = |p: &Path| {
            p.components()
                .any(|c| c.as_os_str().eq_ignore_ascii_case(".git"))
        };
        if git(rel) {
            return Err(self.refuse(
                Refusal::Denied,
                &format!("{raw:?} (git metadata; file tools can't modify it)"),
            ));
        }
        if let Ok(canon) = root.canonicalize(if parent_rel.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent_rel
        }) && git(&canon)
        {
            return Err(self.refuse(
                Refusal::Denied,
                &format!("{raw:?} (git metadata; file tools can't modify it)"),
            ));
        }
        // The data directory: every directory along the path, by identity.
        if !self.deny.is_empty() {
            let mut chain = Vec::new();
            if let Ok(m) = root.dir_metadata() {
                chain.push(ident_of(&m));
            }
            let mut acc = PathBuf::new();
            for c in parent_rel.components() {
                acc.push(c);
                if let Ok(d) = root.open_dir(&acc)
                    && let Ok(m) = d.dir_metadata()
                {
                    chain.push(ident_of(&m));
                }
            }
            if self.deny.denies_dir_chain(&chain) {
                return Err(self.refuse(
                    Refusal::Denied,
                    &format!("{raw:?} (inside tern's data directory; file tools can't modify it)"),
                ));
            }
            // A second name for a protected file.
            if let Ok(parent) = root.open_dir(if parent_rel.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent_rel
            }) && let Ok(m) = parent.symlink_metadata(name)
                && m.is_file()
                && self.deny.is_protected_hardlink(ident_of(&m), m.nlink())
            {
                return Err(self.refuse(
                    Refusal::Denied,
                    &format!("{raw:?} (a hardlink to a file in tern's data directory)"),
                ));
            }
        }
        Ok(())
    }

    fn check_parent_handle(
        &self,
        root: &Dir,
        raw: &str,
        parent_rel: &Path,
        parent: &Dir,
    ) -> ScopeResult<()> {
        if self.deny.is_empty() {
            return Ok(());
        }
        let _ = (root, parent_rel);
        if let Ok(m) = parent.dir_metadata()
            && self.deny.denies_dir_chain(&[ident_of(&m)])
        {
            return Err(self.refuse(
                Refusal::Denied,
                &format!("{raw:?} (inside tern's data directory; file tools can't modify it)"),
            ));
        }
        Ok(())
    }

    // ---- narrowing ----

    /// A scope on a subdirectory of this one, for a sub-agent: it keeps the
    /// same extra roots and deny set and cannot reach anything this scope
    /// can't. `sub` must be a relative path to an existing directory.
    pub fn narrow(&self, sub: &str) -> ScopeResult<Arc<Self>> {
        let root = self.live()?;
        if sub.is_empty() || sub == "." {
            return Err(ScopeError::Io("subdir is empty".into()));
        }
        let p = Path::new(sub);
        if !p.is_relative() || sub.contains('\0') {
            return Err(self.refuse(
                Refusal::Outside,
                &format!("{sub:?} (a sub-agent folder is a relative path inside this folder)"),
            ));
        }
        let canon = root.canonicalize(p).map_err(|e| self.map_open(sub, p, e))?;
        if !root.is_dir(&canon) {
            return Err(ScopeError::Io(format!("{sub} is not a directory")));
        }
        let joined = match &self.subdir {
            Some(s) => s.join(&canon),
            None => canon,
        };
        let scope = Self::open(
            &self.id,
            &self.root,
            Some(&joined),
            self.deny.clone(),
            self.ro_roots.clone(),
            self.flags,
        )?;
        scope.set_shell_bound(self.shell_bound());
        if let Ok(s) = self.sink.lock()
            && let Some(sink) = s.clone()
        {
            return Ok(Arc::new(scope.with_sink(sink)));
        }
        Ok(Arc::new(scope))
    }
}

fn refused(kind: Refusal, message: String) -> ScopeError {
    ScopeError::Refused { kind, message }
}

fn moved(path: &Path, e: &std::io::Error) -> ScopeError {
    refused(
        Refusal::Moved,
        format!(
            "refused: the folder {} isn't available ({})",
            path.display(),
            strip_os_code(e)
        ),
    )
}

fn open_nonblocking(dir: &Dir, rel: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    o.custom_flags(NONBLOCK);
    dir.open_with(rel, &o)
}

/// The part of absolute path `abs` below the directory with identity `id`,
/// found by asking the filesystem who each ancestor is (so case, Unicode
/// form and symlinked spellings all work). `None` if no ancestor is it.
fn rel_under(id: Ident, abs: &Path) -> Option<PathBuf> {
    for anc in abs.ancestors() {
        if ident_at(anc) == Some(id) {
            return abs.strip_prefix(anc).ok().map(Path::to_path_buf);
        }
    }
    None
}

// ---- the walk behind search_files ----

struct Walk<'a> {
    scope: &'a WorkspaceScope,
    overrides: Option<ignore::overrides::Override>,
    max_file: u64,
    budget: usize,
    stop: bool,
}

type Matchers = Vec<(ignore::gitignore::Gitignore, PathBuf)>;

impl Walk<'_> {
    fn load_ignore(dir: &Dir, name: &str, into: &mut ignore::gitignore::GitignoreBuilder) {
        let Ok(mut f) = open_nonblocking(dir, Path::new(name)) else {
            return;
        };
        let Ok(meta) = f.metadata() else { return };
        if !meta.is_file() || meta.len() > 1024 * 1024 {
            return;
        }
        let mut text = String::new();
        if f.read_to_string(&mut text).is_ok() {
            for line in text.lines() {
                let _ = into.add_line(None, line);
            }
        }
    }

    fn ignored(matchers: &Matchers, rel_in_dir: &Path, is_dir: bool) -> bool {
        for (gi, prefix) in matchers.iter().rev() {
            let Ok(sub) = rel_in_dir.strip_prefix(prefix) else {
                continue;
            };
            let m = gi.matched(sub, is_dir);
            if m.is_ignore() {
                return true;
            }
            if m.is_whitelist() {
                return false;
            }
        }
        false
    }

    fn dir(
        &mut self,
        dir: &Dir,
        rel: &Path,
        matchers: &mut Matchers,
        visit: &mut impl FnMut(&str, &[u8]) -> bool,
    ) {
        let mut b = ignore::gitignore::GitignoreBuilder::new("");
        Self::load_ignore(dir, ".gitignore", &mut b);
        Self::load_ignore(dir, ".ignore", &mut b);
        let pushed = match b.build() {
            Ok(gi) if !gi.is_empty() => {
                matchers.push((gi, rel.to_path_buf()));
                true
            }
            _ => false,
        };
        let mut entries: Vec<(OsString, bool, bool)> = Vec::new();
        if let Ok(rd) = dir.entries() {
            for e in rd.flatten() {
                let Ok(t) = e.file_type() else { continue };
                entries.push((e.file_name(), t.is_dir(), t.is_file()));
            }
        }
        entries.sort();
        for (name, is_dir, is_file) in entries {
            if self.stop {
                break;
            }
            if self.budget == 0 {
                self.stop = true;
                break;
            }
            self.budget -= 1;
            let child_rel = rel.join(&name);
            if is_dir {
                if name == ".git" || Self::ignored(matchers, &child_rel, true) {
                    continue;
                }
                if let Some(ov) = &self.overrides
                    && ov.matched(&child_rel, true).is_ignore()
                {
                    // A whitelist glob never prunes directories (it names files).
                }
                let Ok(sub) = dir.open_dir(&name) else {
                    continue;
                };
                // The directory may have been swapped for something else
                // since it was listed; the handle is what is searched.
                self.dir(&sub, &child_rel, matchers, visit);
            } else if is_file {
                if Self::ignored(matchers, &child_rel, false) {
                    continue;
                }
                if let Some(ov) = &self.overrides
                    && ov.matched(&child_rel, false).is_ignore()
                {
                    continue;
                }
                let Ok(mut f) = open_nonblocking(dir, Path::new(&name)) else {
                    continue;
                };
                let Ok(meta) = f.metadata() else { continue };
                if !meta.is_file()
                    || meta.len() > self.max_file
                    || self.scope.deny.denies_read(ident_of(&meta))
                {
                    continue;
                }
                let mut buf = Vec::new();
                if Read::by_ref(&mut f)
                    .take(self.max_file)
                    .read_to_end(&mut buf)
                    .is_err()
                {
                    continue;
                }
                if !visit(&child_rel.to_string_lossy(), &buf) {
                    self.stop = true;
                }
            }
        }
        if pushed {
            matchers.pop();
        }
    }
}

/// The names `WorkspaceScope` reserves inside a workspace for tern's own
/// per-workspace state (temp and caches). Never version-controlled.
pub const STATE_DIR: &str = ".tern";

/// Per-workspace environment for shell commands: temp and caches live in the
/// workspace's hidden state directory, so one workspace can't stage files for
/// or poison another's caches (design §8, decision 4).
pub fn shell_env(scope: &WorkspaceScope) -> Vec<(String, String)> {
    let state = scope.root().join(STATE_DIR);
    let tmp = state.join("tmp");
    let cache = state.join("cache");
    let p = |sub: &str| cache.join(sub).to_string_lossy().into_owned();
    let mut env = vec![
        ("TMPDIR".to_owned(), tmp.to_string_lossy().into_owned()),
        ("TMP".to_owned(), tmp.to_string_lossy().into_owned()),
        ("TEMP".to_owned(), tmp.to_string_lossy().into_owned()),
        ("CARGO_TARGET_DIR".to_owned(), p("cargo-target")),
        ("npm_config_cache".to_owned(), p("npm")),
        ("PIP_CACHE_DIR".to_owned(), p("pip")),
        ("GOCACHE".to_owned(), p("go-build")),
        ("GOMODCACHE".to_owned(), p("go-mod")),
        ("DENO_DIR".to_owned(), p("deno")),
        ("BUN_INSTALL_CACHE_DIR".to_owned(), p("bun")),
        ("COREPACK_HOME".to_owned(), p("corepack")),
        ("PYTHONPYCACHEPREFIX".to_owned(), p("pycache")),
        ("XDG_CACHE_HOME".to_owned(), p("xdg")),
        ("UV_CACHE_DIR".to_owned(), p("uv")),
        ("YARN_CACHE_FOLDER".to_owned(), p("yarn")),
        ("PNPM_HOME".to_owned(), p("pnpm")),
    ];
    // The registry (CARGO_HOME) is per workspace too: sharing it would let one
    // workspace's build scripts poison another's dependencies. Cargo's
    // toolchain binaries stay where rustup keeps them.
    env.push(("CARGO_HOME".to_owned(), p("cargo-home")));
    env
}

/// Create the per-workspace directories [`shell_env`] points at, through the
/// handle so a symlinked `.tern` can't redirect them. Best effort.
pub fn prepare_shell_dirs(scope: &WorkspaceScope) {
    let Some(dir) = scope.dir.as_ref() else {
        return;
    };
    for sub in ["tmp", "cache"] {
        let _ = dir.create_dir_all(Path::new(STATE_DIR).join(sub));
    }
    // Keep it out of version control without touching the user's files.
    let marker = Path::new(STATE_DIR).join(".gitignore");
    if !dir.exists(&marker) {
        let _ = dir.write(&marker, "*\n");
    }
}

#[cfg(test)]
mod tests;
