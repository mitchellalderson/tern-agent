//! File tools: read_file, list_dir, search_files, write_file, edit_file.
//!
//! Every path goes through the run's [`WorkspaceScope`] (docs/alpha-workspaces.md
//! §6): handle-based, so there is no gap between checking a path and using
//! it, and outside the folder the answer is a refusal, never a question.
//! Nothing here touches `std::fs` or `tokio::fs`; a new file tool that did
//! would be unbounded, and `clippy.toml` bans it.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::tool::{Effect, Reach, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;
use tern_core::workspace::{ScopeError, ScopeResult, WorkspaceScope};

use crate::{args, spec};

const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;
const MAX_LIST_ENTRIES: usize = 1000;
const MAX_SEARCH_MATCHES: usize = 200;
const MAX_SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Reads never ask: they can't leave the folder, so there is nothing to ask
/// about. (Before workspaces a read outside the folder asked; now it is
/// refused.)
fn read_policy() -> ToolPolicy {
    ToolPolicy::new(Effect::Read, true).reaching(Reach::Workspace)
}

/// Writes inside the folder are granted by the host. A target the scope has
/// verified lies inside its own folder is also marked `taint_safe`, so a
/// tainted run needn't ask about it (`[taint] workspace_writes`).
fn write_policy(ctx: &ToolCtx, args: &Value) -> ToolPolicy {
    let confined = args
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|p| ctx.scope.confined_write_target(p));
    ToolPolicy::granted_write(false)
        .reaching(Reach::Workspace)
        .taint_safe(confined)
}

/// Run a blocking scope operation off the async threads.
pub(crate) async fn run_scope<T: Send + 'static>(
    ctx: &ToolCtx,
    f: impl FnOnce(&WorkspaceScope) -> ScopeResult<T> + Send + 'static,
) -> ScopeResult<T> {
    let scope = ctx.scope.clone();
    tokio::task::spawn_blocking(move || f(&scope))
        .await
        .unwrap_or_else(|e| Err(ScopeError::Io(format!("file call failed: {e}"))))
}

pub(crate) fn refusal(ctx: &ToolCtx, tool: &str, raw: &str, e: ScopeError) -> ToolResult {
    ToolResult::error(ctx.scope.fail(tool, raw, e))
}

// ---- read_file ----

pub struct ReadFile;

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    /// 1-based first line.
    offset: Option<usize>,
    limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadFile {
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "read_file",
            "Read a UTF-8 text file. Relative paths are inside the workspace; files outside it (and its read-only folders) can't be read. Optionally pass a 1-based line `offset` and a line `limit`.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"]
            }),
        )
    }

    fn policy(&self, _ctx: &ToolCtx, _args: &Value) -> ToolPolicy {
        read_policy()
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: ReadArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let path = a.path.clone();
        let bytes = match run_scope(ctx, move |s| s.read_file(&path, MAX_READ_BYTES)).await {
            Ok(b) => b,
            Err(e) => return refusal(ctx, "read_file", &a.path, e),
        };
        if bytes.contains(&0) {
            return ToolResult::error(format!("{} looks like a binary file", a.path));
        }
        let text = String::from_utf8_lossy(&bytes);
        if a.offset.is_none() && a.limit.is_none() {
            return ToolResult::ok(text);
        }
        let start = a.offset.unwrap_or(1).saturating_sub(1);
        let lines: Vec<&str> = text
            .lines()
            .skip(start)
            .take(a.limit.unwrap_or(usize::MAX))
            .collect();
        ToolResult::ok(lines.join("\n"))
    }
}

// ---- list_dir ----

pub struct ListDir;

#[derive(Deserialize)]
struct ListArgs {
    #[serde(default = "dot")]
    path: String,
}

fn dot() -> String {
    ".".into()
}

#[async_trait]
impl Tool for ListDir {
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "list_dir",
            "List a directory. Directories are shown with a trailing `/`. Defaults to the workspace root.",
            json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        )
    }

    fn policy(&self, _ctx: &ToolCtx, _args: &Value) -> ToolPolicy {
        read_policy()
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: ListArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let path = a.path.clone();
        let mut entries = match run_scope(ctx, move |s| s.list_dir(&path)).await {
            Ok(e) => e,
            Err(e) => return refusal(ctx, "list_dir", &a.path, e),
        };
        let total = entries.len();
        entries.truncate(MAX_LIST_ENTRIES);
        let mut out = entries
            .into_iter()
            .map(|(mut name, is_dir)| {
                if is_dir {
                    name.push('/');
                }
                name
            })
            .collect::<Vec<_>>()
            .join("\n");
        if total > MAX_LIST_ENTRIES {
            out.push_str(&format!(
                "\n[... {} more entries]",
                total - MAX_LIST_ENTRIES
            ));
        }
        if out.is_empty() {
            out = "(empty directory)".into();
        }
        ToolResult::ok(out)
    }
}

// ---- search_files ----

pub struct SearchFiles;

#[derive(Deserialize)]
struct SearchArgs {
    pattern: String,
    #[serde(default = "dot")]
    path: String,
    glob: Option<String>,
}

#[async_trait]
impl Tool for SearchFiles {
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "search_files",
            "Search file contents with a regular expression, respecting .gitignore. Returns `path:line: text` matches. Optionally restrict files with a glob like `*.rs`.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Rust regex syntax" },
                    "path": { "type": "string" },
                    "glob": { "type": "string" }
                },
                "required": ["pattern"]
            }),
        )
    }

    fn policy(&self, _ctx: &ToolCtx, _args: &Value) -> ToolPolicy {
        read_policy()
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: SearchArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let re = match regex::Regex::new(&a.pattern) {
            Ok(re) => re,
            Err(e) => return ToolResult::error(format!("invalid regex: {e}")),
        };
        let (path, glob) = (a.path.clone(), a.glob.clone());
        let found = run_scope(ctx, move |s| {
            let mut out = Vec::new();
            let mut truncated = false;
            s.search(
                &path,
                glob.as_deref(),
                MAX_SEARCH_FILE_BYTES,
                |shown, bytes| {
                    if bytes.contains(&0) {
                        return true;
                    }
                    let text = String::from_utf8_lossy(bytes);
                    for (i, line) in text.lines().enumerate() {
                        if re.is_match(line) {
                            if out.len() == MAX_SEARCH_MATCHES {
                                truncated = true;
                                return false;
                            }
                            let line: String = line.chars().take(300).collect();
                            out.push(format!("{shown}:{}: {line}", i + 1));
                        }
                    }
                    true
                },
            )?;
            Ok((out, truncated))
        })
        .await;
        match found {
            Ok((out, _)) if out.is_empty() => ToolResult::ok("no matches"),
            Ok((out, truncated)) => {
                let mut s = out.join("\n");
                if truncated {
                    s.push_str(&format!("\n[stopped after {MAX_SEARCH_MATCHES} matches]"));
                }
                ToolResult::ok(s)
            }
            Err(e) => refusal(ctx, "search_files", &a.path, e),
        }
    }
}

// ---- write_file ----

pub struct WriteFile;

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteFile {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "write_file",
            "Create or overwrite a file with `content`, creating parent directories. Relative paths are inside the workspace; nothing outside it can be written.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
                "required": ["path", "content"]
            }),
        )
    }

    fn policy(&self, ctx: &ToolCtx, args: &Value) -> ToolPolicy {
        write_policy(ctx, args)
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: WriteArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let (path, content) = (a.path.clone(), a.content.clone());
        match run_scope(ctx, move |s| s.write_file(&path, content.as_bytes())).await {
            Ok(()) => ToolResult::ok(format!("wrote {} bytes to {}", a.content.len(), a.path)),
            Err(e) => refusal(ctx, "write_file", &a.path, e),
        }
    }
}

// ---- edit_file ----

pub struct EditFile;

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for EditFile {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "edit_file",
            "Replace an exact string in a file. `old_string` must occur exactly once unless `replace_all` is true.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        )
    }

    fn policy(&self, ctx: &ToolCtx, args: &Value) -> ToolPolicy {
        write_policy(ctx, args)
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: EditArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        if a.old_string.is_empty() {
            return ToolResult::error("old_string must not be empty");
        }
        let path = a.path.clone();
        // A read-only folder is refused here rather than after the read.
        let bytes = match run_scope(ctx, move |s| s.read_file(&path, MAX_READ_BYTES)).await {
            Ok(b) => b,
            Err(e) => return refusal(ctx, "edit_file", &a.path, e),
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return ToolResult::error(format!("{}: stream did not contain valid UTF-8", a.path));
        };
        let count = text.matches(&a.old_string).count();
        let updated = match (count, a.replace_all) {
            (0, _) => return ToolResult::error(format!("old_string not found in {}", a.path)),
            (1, _) | (_, true) => text.replace(&a.old_string, &a.new_string),
            (n, false) => {
                return ToolResult::error(format!(
                    "old_string occurs {n} times in {}; add context to make it unique or set replace_all",
                    a.path
                ));
            }
        };
        let path = a.path.clone();
        match run_scope(ctx, move |s| s.write_file(&path, updated.as_bytes())).await {
            Ok(()) => ToolResult::ok(format!("replaced {count} occurrence(s) in {}", a.path)),
            Err(e) => refusal(ctx, "edit_file", &a.path, e),
        }
    }
}

/// Registered by `register_builtins`; kept as one list so a test can check
/// that every file tool declares `Reach::Workspace`.
pub fn all() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadFile),
        Arc::new(ListDir),
        Arc::new(SearchFiles),
        Arc::new(WriteFile),
        Arc::new(EditFile),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn ctx(dir: &std::path::Path) -> ToolCtx {
        ToolCtx {
            scope: WorkspaceScope::for_dir(dir.to_path_buf()),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn write_read_edit_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let w = WriteFile
            .call(
                &c,
                json!({"path": "d/hello.txt", "content": "hi\nthere\nhi"}),
            )
            .await;
        assert!(!w.is_error, "{}", w.content);
        let r = ReadFile.call(&c, json!({"path": "d/hello.txt"})).await;
        assert_eq!(r.content, "hi\nthere\nhi");
        let r = ReadFile
            .call(&c, json!({"path": "d/hello.txt", "offset": 2, "limit": 1}))
            .await;
        assert_eq!(r.content, "there");

        let dup = EditFile
            .call(
                &c,
                json!({"path": "d/hello.txt", "old_string": "hi", "new_string": "yo"}),
            )
            .await;
        assert!(
            dup.is_error && dup.content.contains("2 times"),
            "{}",
            dup.content
        );
        let ok = EditFile
            .call(
                &c,
                json!({"path": "d/hello.txt", "old_string": "there", "new_string": "you"}),
            )
            .await;
        assert!(!ok.is_error);
        let all = EditFile
            .call(&c, json!({"path": "d/hello.txt", "old_string": "hi", "new_string": "yo", "replace_all": true}))
            .await;
        assert!(!all.is_error);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("d/hello.txt")).unwrap(),
            "yo\nyou\nyo"
        );
    }

    #[tokio::test]
    async fn list_and_search() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(tmp.path().join("sub/b.txt"), "main idea\n").unwrap();
        let c = ctx(tmp.path());
        assert_eq!(ListDir.call(&c, json!({})).await.content, "a.rs\nsub/");
        let all = SearchFiles
            .call(&c, json!({"pattern": "main"}))
            .await
            .content;
        assert!(all.contains("a.rs:1: fn main() {}"), "{all}");
        assert!(all.contains("b.txt:1: main idea"), "{all}");
        let rs = SearchFiles
            .call(&c, json!({"pattern": "main", "glob": "*.rs"}))
            .await
            .content;
        assert!(!rs.contains("b.txt"), "{rs}");
        let bad = SearchFiles.call(&c, json!({"pattern": "("})).await;
        assert!(bad.is_error);
    }

    #[tokio::test]
    async fn rejects_escapes_and_bad_args() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        assert!(ReadFile.call(&c, json!({"path": "../x"})).await.is_error);
        let bad = WriteFile.call(&c, json!({"path": 3})).await;
        assert!(
            bad.is_error && bad.content.contains("invalid arguments"),
            "{}",
            bad.content
        );
    }

    #[test]
    fn outside_the_folder_is_refused_not_asked() {
        // The old behaviour: a read or write outside asked for approval. Now
        // no file tool ever sets `requires_approval`: the call is refused.
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let outside = json!({"path": "/etc/hosts"});
        for t in all() {
            let p = t.policy(&c, &outside);
            assert!(!p.requires_approval, "{}", t.spec().name);
            assert_eq!(p.reach, Reach::Workspace, "{}", t.spec().name);
        }
    }

    #[tokio::test]
    async fn absolute_paths_outside_are_refused_with_the_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        std::fs::write(tmp.path().join("out.txt"), "outside").unwrap();
        let c = ctx(&ws);
        let outside = tmp.path().join("out.txt");
        let r = ReadFile.call(&c, json!({"path": outside})).await;
        assert!(r.is_error, "{}", r.content);
        assert!(
            r.content.contains("outside this conversation's folder"),
            "{}",
            r.content
        );
        let w = WriteFile
            .call(
                &c,
                json!({"path": tmp.path().join("new.txt"), "content": "x"}),
            )
            .await;
        assert!(w.is_error);
        assert!(!tmp.path().join("new.txt").exists());
        let inside = ws.join("in.txt");
        let w = WriteFile
            .call(&c, json!({"path": inside, "content": "x"}))
            .await;
        assert!(!w.is_error, "{}", w.content);
    }

    #[tokio::test]
    async fn search_reaches_hidden_dirs_and_honors_gitignore_without_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
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
        let r = SearchFiles
            .call(&ctx(ws), json!({"pattern": "needle"}))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("ci.yml"), "{}", r.content);
        assert!(r.content.contains("keep.txt"), "{}", r.content);
        assert!(!r.content.contains("config"), "{}", r.content);
        assert!(!r.content.contains("ignored.txt"), "{}", r.content);
        assert!(!r.content.contains("out.txt"), "{}", r.content);
    }

    #[test]
    fn only_confined_workspace_writes_are_safe_under_taint() {
        use tern_core::approval::{Check, Gate, check};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (ws, other, data) = (root.join("ws"), root.join("other"), root.join("ws/data"));
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other, ws.join("link")).unwrap();
        let c = ToolCtx {
            scope: WorkspaceScope::for_dir_guarded(
                ws.clone(),
                Arc::new(tern_core::workspace::DenySet::for_data_dir(&data)),
            ),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let ask = |tool: &dyn Tool, path: &str| {
            let p = tool.policy(&c, &json!({"path": path}));
            check(&tool.spec().name, &p, false, Gate::TAINTED) != Check::Allow
        };
        for t in [&WriteFile as &dyn Tool, &EditFile as &dyn Tool] {
            assert!(!ask(t, "outline.md"), "inside the workspace");
            assert!(!ask(t, "sub/dir/new.md"), "a new folder inside it");
            assert!(ask(t, "../other/x.md"), "dotdot");
            assert!(ask(t, other.join("x.md").to_str().unwrap()), "outside");
            assert!(ask(t, "data/secret.md"), "protected");
            #[cfg(unix)]
            assert!(ask(t, "link/x.md"), "symlink out");
        }
        // Reads are never asked about; a clean run asks about nothing here.
        let p = WriteFile.policy(&c, &json!({"path": "../other/x.md"}));
        assert_eq!(check("write_file", &p, false, Gate::CLEAN), Check::Allow);
    }
}
