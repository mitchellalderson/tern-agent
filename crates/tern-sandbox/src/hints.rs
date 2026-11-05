//! Plain-language hints for the two failures the sandbox causes on purpose:
//! a download attempted with the network off, and a read or write outside
//! the workspace. The model (and the person reading the transcript) sees
//! why the command failed and what to change, instead of a bare
//! "Operation not permitted".
//!
//! These are heuristics over the command's output: the OS doesn't report
//! which rule denied an access (Seatbelt logs it to the system log, Landlock
//! not at all), so the hint says what is likely, not what is certain.

use std::path::{Path, PathBuf};

use crate::ExecOutput;

/// Messages tools print when they couldn't reach a registry or host.
const OFFLINE_MARKERS: &[&str] = &[
    "failed to download",
    "failed to get `",
    "unable to update registry",
    "offline mode",
    "--offline was specified",
    "network failure seems to have happened",
    "could not resolve host",
    "couldn't resolve host",
    "temporary failure in name resolution",
    "name or service not known",
    "nodename nor servname",
    "enotfound",
    "eai_again",
    "network is unreachable",
    "no such host",
    "failed to establish a new connection",
    "could not connect to server",
    "unable to access 'http",
    "dial tcp",
    "connection refused",
];

/// Messages the OS gives for a denied access (Seatbelt: EPERM; Landlock:
/// EACCES).
const DENIED_MARKERS: &[&str] = &["operation not permitted", "permission denied"];

/// Hints for a failed command. Empty when it succeeded, was cancelled or
/// timed out, or nothing in its output points at the sandbox.
pub fn failure_hints(out: &ExecOutput, network: bool, workspace: &Path) -> Vec<String> {
    if out.exit_code == Some(0) || out.timed_out || out.cancelled {
        return Vec::new();
    }
    let text = format!("{}\n{}", out.stderr, out.stdout);
    let lower = text.to_lowercase();
    let mut hints = Vec::new();
    if !network && OFFLINE_MARKERS.iter().any(|m| lower.contains(m)) {
        hints.push(
            "This command ran with the network off and looks like it needed to download \
             something. Each workspace keeps its own package cache, so the first build or \
             install in a workspace needs network: true (the user approves it). Later runs \
             work offline."
                .to_owned(),
        );
    }
    if DENIED_MARKERS.iter().any(|m| lower.contains(m)) {
        let outside = denied_paths(&text, workspace);
        if !outside.is_empty() {
            let list: Vec<String> = outside.iter().map(|p| p.display().to_string()).collect();
            hints.push(format!(
                "A path outside the workspace was denied by the sandbox ({}). Commands can \
                 write only inside the workspace and read the workspace, system files, \
                 toolchains and folders the user added; to let commands read a folder, add \
                 it as a read-only folder in Settings > Folders.",
                list.join(", ")
            ));
        }
    }
    hints
}

/// Absolute paths that appear on lines reporting a denied access and are not
/// inside `workspace` (at most three, in order of appearance).
fn denied_paths(text: &str, workspace: &Path) -> Vec<PathBuf> {
    let canon = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut out: Vec<PathBuf> = Vec::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        if !DENIED_MARKERS.iter().any(|m| lower.contains(m)) {
            continue;
        }
        for token in line.split(|c: char| {
            c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '(' | ')' | '[' | ']' | '<' | '>')
        }) {
            let token = token.trim_end_matches([',', ';', ':', '.']);
            if !token.starts_with('/') || token.len() < 2 {
                continue;
            }
            let p = PathBuf::from(token);
            if p.starts_with(workspace) || p.starts_with(&canon) {
                continue;
            }
            if ["/dev/", "/proc/", "/sys/"]
                .iter()
                .any(|d| token.starts_with(d))
            {
                continue;
            }
            if !out.contains(&p) {
                out.push(p);
            }
            if out.len() == 3 {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(stderr: &str) -> ExecOutput {
        ExecOutput {
            exit_code: Some(101),
            stderr: stderr.to_owned(),
            ..ExecOutput::default()
        }
    }

    #[test]
    fn an_offline_cargo_build_says_a_first_build_needs_network() {
        let out = failed(
            "error: failed to get `serde` as a dependency of package `x`\n\
             Caused by: unable to update registry `crates-io`",
        );
        let h = failure_hints(&out, false, Path::new("/ws"));
        assert_eq!(h.len(), 1, "{h:?}");
        assert!(h[0].contains("network: true"), "{h:?}");
        // With network on, the same output is not a sandbox matter.
        assert!(failure_hints(&out, true, Path::new("/ws")).is_empty());
    }

    #[test]
    fn other_tools_offline_failures_match() {
        for msg in [
            "npm ERR! code ENOTFOUND",
            "pip: Failed to establish a new connection: [Errno -3] Temporary failure in name resolution",
            "fatal: unable to access 'https://x/y.git/': Could not resolve host: x",
            "dial tcp: lookup proxy.golang.org: no such host",
            "no matching package named `x` found\nAs a reminder, you're using offline mode (--offline)",
        ] {
            let h = failure_hints(&failed(msg), false, Path::new("/ws"));
            assert!(!h.is_empty(), "{msg}");
        }
    }

    #[test]
    fn a_denied_path_outside_the_workspace_points_at_settings() {
        let out = failed("cat: /Users/u/notes/a.txt: Operation not permitted");
        let h = failure_hints(&out, true, Path::new("/ws"));
        assert_eq!(h.len(), 1, "{h:?}");
        assert!(h[0].contains("/Users/u/notes/a.txt"), "{h:?}");
        assert!(h[0].contains("Settings > Folders"), "{h:?}");
    }

    #[test]
    fn denials_inside_the_workspace_or_without_a_path_are_not_blamed_on_the_sandbox() {
        let inside = failed("touch: /ws/a/b: Permission denied");
        assert!(failure_hints(&inside, true, Path::new("/ws")).is_empty());
        let none = failed("chown: Operation not permitted");
        assert!(failure_hints(&none, true, Path::new("/ws")).is_empty());
        let dev = failed("sh: /dev/tty: Permission denied");
        assert!(failure_hints(&dev, true, Path::new("/ws")).is_empty());
    }

    #[test]
    fn success_timeouts_and_clean_failures_get_no_hint() {
        let ok = ExecOutput {
            exit_code: Some(0),
            stderr: "Operation not permitted /Users/x".into(),
            ..ExecOutput::default()
        };
        assert!(failure_hints(&ok, false, Path::new("/ws")).is_empty());
        let timed = ExecOutput {
            exit_code: None,
            timed_out: true,
            stderr: "Could not resolve host".into(),
            ..ExecOutput::default()
        };
        assert!(failure_hints(&timed, false, Path::new("/ws")).is_empty());
        assert!(failure_hints(&failed("test failed: 1 != 2"), false, Path::new("/ws")).is_empty());
    }

    #[test]
    fn at_most_three_paths_are_listed() {
        let out = failed(
            "a: /x/1: Permission denied\nb: /x/2: Permission denied\nc: /x/3: Permission denied\nd: /x/4: Permission denied",
        );
        let h = failure_hints(&out, true, Path::new("/ws"));
        assert!(h[0].contains("/x/3") && !h[0].contains("/x/4"), "{h:?}");
    }
}
