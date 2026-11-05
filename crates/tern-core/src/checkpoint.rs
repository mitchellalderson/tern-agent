//! Checkpoints for code auto mode (docs/alpha-coding-agent.md §3.5).
//!
//! Before a run's first automatic change, tern snapshots the workspace into
//! a shadow git repository of its own (`<home>/checkpoints/<workspace id>`,
//! work tree = the workspace folder). The user's own `.git` is never read or
//! written, and a folder that is not a repository works the same. One commit
//! per run, named by `refs/tern/run/<run id>`. [`changes`] lists what the run
//! changed since, and [`restore`] puts the tracked files back and removes the
//! ones the run created.
//!
//! It respects `.gitignore`, always skips `.tern/`, `target/`, `node_modules/`
//! and `.venv/`, and gives up past a size cap, saying so: a run with no
//! snapshot is reported as such, never silently.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

/// Most files a snapshot may hold.
pub const MAX_FILES: usize = 50_000;
/// Most bytes a snapshot may hold.
pub const MAX_BYTES: u64 = 500 * 1024 * 1024;

const EXCLUDES: &str = ".tern/\ntarget/\nnode_modules/\n.venv/\n.DS_Store\n";

/// A snapshot that exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub commit: String,
    pub files: usize,
}

/// Why there is no snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Skipped {
    /// `git` isn't installed (or won't run).
    NoGit,
    /// More than [`MAX_FILES`] files or [`MAX_BYTES`] bytes.
    TooLarge {
        files: usize,
        bytes: u64,
    },
    Failed {
        message: String,
    },
}

impl Skipped {
    /// The sentence the run summary shows.
    pub fn describe(&self) -> String {
        match self {
            Skipped::NoGit => "no snapshot: git isn't installed".to_owned(),
            Skipped::TooLarge { .. } => "no snapshot: workspace too large".to_owned(),
            Skipped::Failed { message } => format!("no snapshot: {message}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

/// The ref a run's snapshot is kept under.
pub fn run_ref(run_id: &str) -> String {
    format!("refs/tern/run/{run_id}")
}

fn shadow_dir(base: &Path, workspace_id: &str) -> PathBuf {
    // The id names a directory: keep it to a safe alphabet.
    let safe: String = workspace_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    base.join(safe)
}

struct Repo {
    git_dir: PathBuf,
    root: PathBuf,
}

impl Repo {
    fn git(&self) -> Command {
        let mut c = Command::new("git");
        c.env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_INDEX_FILE")
            .args([
                "--literal-pathspecs",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.quotepath=off",
                "-c",
                "user.name=tern",
                "-c",
                "user.email=tern@localhost",
            ])
            .current_dir(&self.root)
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>, Skipped> {
        let out = self.git().args(args).output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Skipped::NoGit
            } else {
                Skipped::Failed {
                    message: e.to_string(),
                }
            }
        })?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(Skipped::Failed {
                message: format!(
                    "git {}: {}",
                    args.first().copied().unwrap_or_default(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            })
        }
    }

    fn open(base: &Path, workspace_id: &str, root: &Path) -> Result<Self, Skipped> {
        let git_dir = shadow_dir(base, workspace_id);
        let repo = Repo {
            git_dir,
            root: root.to_path_buf(),
        };
        if !repo.git_dir.join("HEAD").exists() {
            std::fs::create_dir_all(&repo.git_dir).map_err(|e| Skipped::Failed {
                message: e.to_string(),
            })?;
            repo.run(&["init", "--quiet"])?;
        }
        let info = repo.git_dir.join("info");
        let _ = std::fs::create_dir_all(&info);
        let _ = std::fs::write(info.join("exclude"), EXCLUDES);
        Ok(repo)
    }

    fn resolve(&self, run_id: &str) -> Option<String> {
        let out = self
            .run(&["rev-parse", "--verify", "--quiet", &run_ref(run_id)])
            .ok()?;
        let s = String::from_utf8_lossy(&out).trim().to_owned();
        (!s.is_empty()).then_some(s)
    }
}

/// Snapshot `root` for `run_id`. A run that already has one (a resumed run)
/// keeps it. Blocks on `git`: call from `spawn_blocking`.
pub fn create(
    base: &Path,
    workspace_id: &str,
    root: &Path,
    run_id: &str,
) -> Result<Checkpoint, Skipped> {
    let repo = Repo::open(base, workspace_id, root)?;
    if let Some(commit) = repo.resolve(run_id) {
        return Ok(Checkpoint { commit, files: 0 });
    }
    let listing = repo.run(&[
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ])?;
    let (mut files, mut bytes) = (0usize, 0u64);
    for rel in listing.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        files += 1;
        if let Ok(rel) = std::str::from_utf8(rel)
            && let Ok(m) = std::fs::symlink_metadata(root.join(rel))
        {
            bytes += m.len();
        }
        if files > MAX_FILES || bytes > MAX_BYTES {
            return Err(Skipped::TooLarge { files, bytes });
        }
    }
    repo.run(&["add", "-A"])?;
    repo.run(&[
        "commit",
        "--quiet",
        "--allow-empty",
        "--no-verify",
        "-m",
        &format!("tern checkpoint before run {run_id}"),
    ])?;
    let commit = String::from_utf8_lossy(&repo.run(&["rev-parse", "HEAD"])?)
        .trim()
        .to_owned();
    repo.run(&["update-ref", &run_ref(run_id), &commit])?;
    Ok(Checkpoint { commit, files })
}

/// What changed in `root` since the run's snapshot. Empty when the run has
/// no snapshot.
pub fn changes(
    base: &Path,
    workspace_id: &str,
    root: &Path,
    run_id: &str,
) -> Result<Vec<Change>, Skipped> {
    let repo = Repo::open(base, workspace_id, root)?;
    let Some(commit) = repo.resolve(run_id) else {
        return Ok(vec![]);
    };
    repo.run(&["add", "-A"])?;
    let out = repo.run(&[
        "diff",
        "--cached",
        "--name-status",
        "--no-renames",
        "-z",
        &commit,
    ])?;
    let mut parts = out.split(|b| *b == 0).filter(|p| !p.is_empty());
    let mut list = vec![];
    while let (Some(status), Some(path)) = (parts.next(), parts.next()) {
        let kind = match status {
            b"A" => ChangeKind::Added,
            b"D" => ChangeKind::Deleted,
            _ => ChangeKind::Modified,
        };
        list.push(Change {
            path: String::from_utf8_lossy(path).into_owned(),
            kind,
        });
    }
    Ok(list)
}

/// Put `root` back as the run's snapshot had it: modified and deleted files
/// are restored, files the run created are removed. Returns what it did.
/// Nothing outside `root` and nothing in the user's `.git` is touched.
pub fn restore(
    base: &Path,
    workspace_id: &str,
    root: &Path,
    run_id: &str,
) -> Result<Vec<Change>, Skipped> {
    let list = changes(base, workspace_id, root, run_id)?;
    let repo = Repo::open(base, workspace_id, root)?;
    let Some(commit) = repo.resolve(run_id) else {
        return Ok(vec![]);
    };
    for c in &list {
        let rel = Path::new(&c.path);
        if rel.is_absolute() || rel.components().any(|p| matches!(p, Component::ParentDir)) {
            continue;
        }
        match c.kind {
            ChangeKind::Added => {
                let _ = std::fs::remove_file(root.join(rel));
            }
            ChangeKind::Modified | ChangeKind::Deleted => {
                repo.run(&["checkout", "--quiet", &commit, "--", &c.path])?;
            }
        }
    }
    // The index follows the snapshot again for the next diff.
    repo.run(&["add", "-A"])?;
    Ok(list)
}

#[cfg(test)]
mod tests;
