//! File tools reach files only through the run's `WorkspaceScope`
//! (docs/alpha-workspaces.md §6.1): a tool that opened a path by name would
//! be unbounded. This guards `fs.rs` (the file tools) against raw `std::fs`
//! and `tokio::fs`, and against building a path from the workspace string.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[test]
fn the_file_tools_use_no_raw_filesystem_calls() {
    let src = include_str!("../src/fs.rs");
    let code = src.split("#[cfg(test)]").next().unwrap();
    for banned in [
        "std::fs",
        "tokio::fs",
        "File::open",
        "File::create",
        "read_dir(",
        "canonicalize(",
        "ctx.workspace()",
        "ctx.workspace.",
        "ignore::WalkBuilder",
    ] {
        // Doc comments may name them.
        let hit = code
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .any(|l| l.contains(banned));
        assert!(!hit, "fs.rs uses `{banned}`: go through ctx.scope instead");
    }
}
