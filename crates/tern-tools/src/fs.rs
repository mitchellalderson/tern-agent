//! File tools: read_file, list_dir, search_files, write_file, edit_file.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;

use std::sync::Arc;

use crate::paths::{Guard, resolve};
use crate::{args, spec};

const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;
const MAX_LIST_ENTRIES: usize = 1000;
const MAX_SEARCH_MATCHES: usize = 200;
const MAX_SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Reads inside the workspace are free; anywhere else needs approval, so
/// a prompt-injected model can't quietly read `~/.ssh` and send it out.
fn read_policy(ctx: &ToolCtx, args: &Value, default: &str) -> ToolPolicy {
    let raw = args.get("path").and_then(Value::as_str).unwrap_or(default);
    let outside = resolve(&ctx.workspace, raw).is_ok_and(|r| !r.inside_workspace);
    ToolPolicy::new(Effect::Read, true).forced(outside)
}

// ---- read_file ----

pub struct ReadFile {
    pub guard: Arc<Guard>,
}

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
            "Read a UTF-8 text file. Relative paths are inside the workspace. Optionally pass a 1-based line `offset` and a line `limit`.",
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

    fn policy(&self, ctx: &ToolCtx, args: &Value) -> ToolPolicy {
        read_policy(ctx, args, "")
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: ReadArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let path = match resolve(&ctx.workspace, &a.path)
            .and_then(|r| self.guard.check_read(&r).map(|()| r.path))
        {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        match tokio::fs::metadata(&path).await {
            Ok(m) if m.is_dir() => {
                return ToolResult::error(format!("{} is a directory; use list_dir", a.path));
            }
            Ok(m) if m.len() > MAX_READ_BYTES => {
                return ToolResult::error(format!(
                    "{} is {} bytes; too large to read",
                    a.path,
                    m.len()
                ));
            }
            Ok(_) => {}
            Err(e) => return ToolResult::error(format!("{}: {e}", a.path)),
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return ToolResult::error(format!("{}: {e}", a.path)),
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

pub struct ListDir {
    pub guard: Arc<Guard>,
}

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

    fn policy(&self, ctx: &ToolCtx, args: &Value) -> ToolPolicy {
        read_policy(ctx, args, ".")
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: ListArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let path = match resolve(&ctx.workspace, &a.path)
            .and_then(|r| self.guard.check_read(&r).map(|()| r.path))
        {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        let mut rd = match tokio::fs::read_dir(&path).await {
            Ok(rd) => rd,
            Err(e) => return ToolResult::error(format!("{}: {e}", a.path)),
        };
        let mut entries = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().await.is_ok_and(|t| t.is_dir()) {
                name.push('/');
            }
            entries.push(name);
        }
        entries.sort();
        let total = entries.len();
        entries.truncate(MAX_LIST_ENTRIES);
        let mut out = entries.join("\n");
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

pub struct SearchFiles {
    pub guard: Arc<Guard>,
}

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

    fn policy(&self, ctx: &ToolCtx, args: &Value) -> ToolPolicy {
        read_policy(ctx, args, ".")
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: SearchArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let root = match resolve(&ctx.workspace, &a.path)
            .and_then(|r| self.guard.check_read(&r).map(|()| r.path))
        {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        let re = match regex::Regex::new(&a.pattern) {
            Ok(re) => re,
            Err(e) => return ToolResult::error(format!("invalid regex: {e}")),
        };
        let guard = self.guard.clone();
        let task =
            tokio::task::spawn_blocking(move || search(&root, &re, a.glob.as_deref(), &guard));
        match task.await {
            Ok(Ok(out)) => ToolResult::ok(out),
            Ok(Err(e)) => ToolResult::error(e),
            Err(e) => ToolResult::error(format!("search failed: {e}")),
        }
    }
}

fn search(
    root: &std::path::Path,
    re: &regex::Regex,
    glob: Option<&str>,
    guard: &Guard,
) -> Result<String, String> {
    let mut builder = ignore::WalkBuilder::new(root);
    // Hidden directories (`.github`, `.config`) are searchable; only the
    // repository metadata directory is not. Ignore files apply even outside
    // a repository.
    builder
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git");
    if let Some(glob) = glob {
        let mut ov = ignore::overrides::OverrideBuilder::new(root);
        ov.add(glob).map_err(|e| format!("invalid glob: {e}"))?;
        builder.overrides(ov.build().map_err(|e| format!("invalid glob: {e}"))?);
    }
    let mut out = Vec::new();
    let mut truncated = false;
    'walk: for entry in builder.build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        // Checked per file: the root may be allowed while a file under it
        // (a secret, or a symlink to one) isn't.
        let real = std::fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path().to_owned());
        if !guard.allows_read(&real) {
            continue;
        }
        if entry
            .metadata()
            .is_ok_and(|m| m.len() > MAX_SEARCH_FILE_BYTES)
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let shown = entry.path().strip_prefix(root).unwrap_or(entry.path());
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                if out.len() == MAX_SEARCH_MATCHES {
                    truncated = true;
                    break 'walk;
                }
                let line: String = line.chars().take(300).collect();
                out.push(format!("{}:{}: {}", shown.display(), i + 1, line));
            }
        }
    }
    if out.is_empty() {
        return Ok("no matches".into());
    }
    let mut s = out.join("\n");
    if truncated {
        s.push_str(&format!("\n[stopped after {MAX_SEARCH_MATCHES} matches]"));
    }
    Ok(s)
}

// ---- write_file ----

pub struct WriteFile {
    pub guard: Arc<Guard>,
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

/// Workspace writes are granted by the host; outside it they need approval.
fn write_policy(ctx: &ToolCtx, args: &Value) -> ToolPolicy {
    let outside = args["path"]
        .as_str()
        .and_then(|p| resolve(&ctx.workspace, p).ok())
        .is_some_and(|r| !r.inside_workspace);
    ToolPolicy::granted_write(false).forced(outside)
}

#[async_trait]
impl Tool for WriteFile {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "write_file",
            "Create or overwrite a file with `content`, creating parent directories. Relative paths are inside the workspace.",
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
        let path = match resolve(&ctx.workspace, &a.path)
            .and_then(|r| self.guard.check_write(&r).map(|()| r.path))
        {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        if let Some(parent) = path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolResult::error(format!("creating {}: {e}", parent.display()));
        }
        match tokio::fs::write(&path, a.content.as_bytes()).await {
            Ok(()) => ToolResult::ok(format!("wrote {} bytes to {}", a.content.len(), a.path)),
            Err(e) => ToolResult::error(format!("{}: {e}", a.path)),
        }
    }
}

// ---- edit_file ----

pub struct EditFile {
    pub guard: Arc<Guard>,
}

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
        let path = match resolve(&ctx.workspace, &a.path)
            .and_then(|r| self.guard.check_write(&r).map(|()| r.path))
        {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) => return ToolResult::error(format!("{}: {e}", a.path)),
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
        match tokio::fs::write(&path, updated).await {
            Ok(()) => ToolResult::ok(format!("replaced {count} occurrence(s) in {}", a.path)),
            Err(e) => ToolResult::error(format!("{}: {e}", a.path)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn g() -> Arc<Guard> {
        Arc::new(Guard::default())
    }

    fn ctx(dir: &std::path::Path) -> ToolCtx {
        ToolCtx {
            workspace: dir.to_path_buf(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn write_read_edit_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let w = WriteFile { guard: g() }
            .call(
                &c,
                json!({"path": "d/hello.txt", "content": "hi\nthere\nhi"}),
            )
            .await;
        assert!(!w.is_error, "{}", w.content);
        let r = ReadFile { guard: g() }
            .call(&c, json!({"path": "d/hello.txt"}))
            .await;
        assert_eq!(r.content, "hi\nthere\nhi");
        let r = ReadFile { guard: g() }
            .call(&c, json!({"path": "d/hello.txt", "offset": 2, "limit": 1}))
            .await;
        assert_eq!(r.content, "there");

        let dup = EditFile { guard: g() }
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
        let ok = EditFile { guard: g() }
            .call(
                &c,
                json!({"path": "d/hello.txt", "old_string": "there", "new_string": "you"}),
            )
            .await;
        assert!(!ok.is_error);
        let all = EditFile { guard: g() }
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
        assert_eq!(
            ListDir { guard: g() }.call(&c, json!({})).await.content,
            "a.rs\nsub/"
        );
        let all = SearchFiles { guard: g() }
            .call(&c, json!({"pattern": "main"}))
            .await
            .content;
        assert!(all.contains("a.rs:1: fn main() {}"), "{all}");
        assert!(all.contains("b.txt:1: main idea"), "{all}");
        let rs = SearchFiles { guard: g() }
            .call(&c, json!({"pattern": "main", "glob": "*.rs"}))
            .await
            .content;
        assert!(!rs.contains("b.txt"), "{rs}");
        let bad = SearchFiles { guard: g() }
            .call(&c, json!({"pattern": "("}))
            .await;
        assert!(bad.is_error);
    }

    #[tokio::test]
    async fn rejects_escapes_and_bad_args() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        assert!(
            ReadFile { guard: g() }
                .call(&c, json!({"path": "../x"}))
                .await
                .is_error
        );
        let bad = WriteFile { guard: g() }.call(&c, json!({"path": 3})).await;
        assert!(
            bad.is_error && bad.content.contains("invalid arguments"),
            "{}",
            bad.content
        );
    }

    #[test]
    fn reads_and_writes_outside_workspace_need_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        let c = ctx(&ws);
        assert!(
            !WriteFile { guard: g() }
                .policy(&c, &json!({"path": "in.txt"}))
                .requires_approval
        );
        let outside = tmp.path().join("out.txt");
        assert!(
            WriteFile { guard: g() }
                .policy(&c, &json!({"path": outside}))
                .requires_approval
        );
        assert!(
            ReadFile { guard: g() }
                .policy(&c, &json!({"path": outside}))
                .requires_approval
        );
        assert!(
            !ReadFile { guard: g() }
                .policy(&c, &json!({"path": "in.txt"}))
                .requires_approval
        );
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
        let r = SearchFiles { guard: g() }
            .call(&ctx(ws), json!({"pattern": "needle"}))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("ci.yml"), "{}", r.content);
        assert!(r.content.contains("keep.txt"), "{}", r.content);
        assert!(!r.content.contains("config"), "{}", r.content);
        assert!(!r.content.contains("ignored.txt"), "{}", r.content);
        assert!(!r.content.contains("out.txt"), "{}", r.content);
    }
}
