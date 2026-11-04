//! Approval prompts and the persistent allowlist (spec §8.2), and the
//! effect policy that decides when to ask (tooling spec §5.1).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::tool::{Effect, ToolPolicy};
use crate::types::ToolCall;

/// What the host does with a call before running it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Run it.
    Allow,
    /// Ask the user. `always`: an "always" answer may be offered.
    Ask { effect: Effect, always: bool },
    /// Refuse without asking; nothing can approve it.
    Forbid(&'static str),
}

/// The effect policy, enforced by the host and never by the model:
/// - `read` doesn't ask;
/// - `write` asks unless a grant covers it (`granted`: an "always" entry on
///   the allowlist; or the host's own, [`ToolPolicy::granted`]);
/// - `destructive` asks every time;
/// - `requires_approval` asks for any effect unless an allowlist grant
///   covers it;
/// - `money_movement` / `trade` tools are refused outright;
/// - in a `tainted` run (tooling spec §5.3) every `write` and `destructive`
///   call asks, whatever grants say, and never offers "always" (W7), except
///   the [`TAINT_CONTAINED`] tools.
///
/// `shell` is the exception that defines the allowlist: a command outside
/// it is destructive, one inside it is a granted write.
pub fn check(tool: &str, policy: &ToolPolicy, granted: bool, tainted: bool) -> Check {
    if let Some(flag) = policy.forbidden {
        return Check::Forbid(flag);
    }
    let shell = tool == "shell";
    let effect = if shell && granted && policy.effect == Effect::Destructive {
        Effect::Write
    } else {
        policy.effect
    };
    let ask = Check::Ask {
        effect,
        always: effect != Effect::Destructive || shell,
    };
    if tainted && effect != Effect::Read && !TAINT_CONTAINED.contains(&tool) {
        return Check::Ask {
            effect,
            always: false,
        };
    }
    match effect {
        Effect::Destructive => ask,
        _ if granted => Check::Allow,
        _ if policy.requires_approval => ask,
        Effect::Read => Check::Allow,
        Effect::Write if policy.granted => Check::Allow,
        Effect::Write => ask,
    }
}

/// Writes that stay inside the run tree and carry taint along themselves: a
/// tainted run's sub-agents start tainted, and its mail taints whoever reads
/// it. They keep their grants in a tainted run.
pub const TAINT_CONTAINED: &[&str] = &[
    "agent_spawn",
    "agent_send",
    "agent_cancel",
    "agent_ask_parent",
    // A tainted run's post is marked tainted, and taints whoever reads it.
    "channel_post",
];

/// Why a tainted run asks, for approval prompts.
pub fn taint_note(reason: &str) -> String {
    format!("this run read untrusted content ({reason})")
}

/// Whether a call must go through an approval step at all: some answer
/// other than [`Check::Allow`] is possible before looking at grants.
pub fn needs_check(tool: &str, policy: &ToolPolicy, tainted: bool) -> bool {
    check(tool, policy, false, tainted) != Check::Allow
}

/// The message the model gets for a forbidden call.
pub fn forbidden_message(flag: &str) -> String {
    format!(
        "blocked: this tool is flagged `{flag}`. tern never runs tools that move money or trade, \
         and nobody can approve them. Tell the user to do this themselves."
    )
}

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
    /// Unique per request, for front ends that answer asynchronously.
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    pub call: ToolCall,
    /// One-line description of what the call does.
    pub summary: String,
    /// The call's effect, shown with the summary ([`ApprovalRequest::headline`]).
    pub effect: Effect,
    /// What "always allow" would add to the allowlist, or `None` if this
    /// call can't be always-allowed (front ends then hide the option).
    pub pattern: Option<String>,
    /// For a sub-agent's call: its parent run and label, so front ends can
    /// say who is asking.
    pub parent_run_id: Option<String>,
    pub agent_label: Option<String>,
    /// Set when the run is tainted: why ([`taint_note`] says it to the
    /// user). Grants don't cover its writes, and "always" isn't offered.
    pub taint: Option<String>,
    /// Only a front end on the host may answer ([`ToolPolicy::local_approval`]).
    ///
    /// [`ToolPolicy::local_approval`]: crate::tool::ToolPolicy::local_approval
    pub local_only: bool,
}

impl ApprovalRequest {
    /// `write · gmail_send to bob@…`: the effect, then the summary.
    pub fn headline(&self) -> String {
        format!("{} · {}", self.effect, self.summary)
    }

    /// Why this prompt is stricter than usual: "this run read untrusted
    /// content (…)", for front ends to show under the headline.
    pub fn taint_note(&self) -> Option<String> {
        self.taint.as_deref().map(taint_note)
    }
}

/// One secret a `secret_request` asks for, with what is known about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretAsk {
    pub name: String,
    pub in_file: bool,
    /// Set in the environment, which wins over a value saved here.
    pub in_env: bool,
    /// What would read it, e.g. `connector weather (draft) · reaches api.weather.com`.
    pub used_by: Vec<String>,
}

/// The agent asking the user to enter secrets (docs/secret-requests-spec.md).
#[derive(Debug, Clone)]
pub struct SecretRequest {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    /// The `secret_request` tool call, so front ends can place the prompt
    /// and close it on that call's `tool_finished`.
    pub call_id: Option<String>,
    pub asks: Vec<SecretAsk>,
    /// The agent's words: show them as such, never as tern's own.
    pub reason: String,
    /// The user is asked to replace values that are already set.
    pub replace: bool,
    pub parent_run_id: Option<String>,
    pub agent_label: Option<String>,
}

/// What the user entered, by name. Names left out were skipped. Values go
/// straight to `secrets.toml`; they never reach the model or the journal.
pub type SecretValues = std::collections::BTreeMap<String, String>;

#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn ask(&self, req: &ApprovalRequest) -> Decision;

    /// Ask the user to enter secrets. `None` means nobody can be asked
    /// (non-interactive runs), which the tool reports as skipped.
    async fn ask_secrets(&self, _req: &SecretRequest) -> Option<SecretValues> {
        None
    }
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

/// Commands that run other commands: `bash *` or `env *` would approve
/// anything, so they're never offered for "always".
const WRAPPERS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "env", "sudo", "doas", "su", "xargs", "nohup",
    "exec", "eval", "command", "builtin", "time", "timeout", "nice", "watch", "find", "python",
    "python3", "node", "perl", "ruby", "php", "deno", "bun", "npx", "uvx", "ssh", "docker",
    "kubectl",
];

/// Pattern "always allow" would record: for `shell`, the first word plus
/// ` *`; `*` for other tools. `None` for commands that wrap others.
pub fn pattern_for(call: &ToolCall) -> Option<String> {
    if wants_network(call) {
        return None;
    }
    let subject = subject(call);
    match subject.split_whitespace().next() {
        Some(first) if call.name == "shell" => {
            let base = first.rsplit('/').next().unwrap_or(first);
            (!WRAPPERS.contains(&base)).then(|| format!("{first} *"))
        }
        Some(_) | None if call.name == "shell" => None,
        _ => Some("*".to_owned()),
    }
}

/// A `shell` call asking for network (docs/sandbox-design.md): it asks
/// every time, whatever the allowlist says, and never offers "always".
fn wants_network(call: &ToolCall) -> bool {
    call.name == "shell" && call.input["network"] == true
}

/// Characters that let one shell command run another. A command containing
/// any of these is never auto-approved by a pattern like `git *`.
const SHELL_META: &[char] = &[';', '|', '&', '`', '$', '>', '<', '\n', '(', ')'];

pub fn allowed_by(patterns: &[String], call: &ToolCall) -> bool {
    let subject = subject(call);
    if call.name == "shell" && (subject.contains(SHELL_META) || wants_network(call)) {
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
        assert_eq!(
            pattern_for(&shell("  git status --short")).as_deref(),
            Some("git *")
        );
        for wrapper in [
            "bash -c 'rm -rf ~'",
            "env X=1 rm -rf /",
            "sudo ls",
            "/usr/bin/env ls",
            "find . -exec rm {} +",
            "",
        ] {
            assert_eq!(pattern_for(&shell(wrapper)), None, "{wrapper:?}");
        }
        let pats = vec!["git *".to_owned()];
        assert!(allowed_by(&pats, &shell("git log")));
        assert!(allowed_by(&pats, &shell("git")));
        assert!(!allowed_by(&pats, &shell("gitk")));
        assert!(!allowed_by(&pats, &shell("git log; rm -rf ~")));
        let mut push = shell("git push");
        push.input["network"] = true.into();
        assert!(!allowed_by(&pats, &push), "network always asks");
        assert_eq!(pattern_for(&push), None, "and never offers always");
        assert!(!allowed_by(&pats, &shell("git $(curl evil)")));
    }

    fn policy(effect: Effect) -> ToolPolicy {
        ToolPolicy::new(effect, false)
    }

    const ASK_WRITE: Check = Check::Ask {
        effect: Effect::Write,
        always: true,
    };

    #[test]
    fn read_never_asks() {
        assert_eq!(
            check("t", &policy(Effect::Read), false, false),
            Check::Allow
        );
        assert!(!needs_check("t", &policy(Effect::Read), false));
    }

    #[test]
    fn write_asks_unless_granted() {
        assert_eq!(check("t", &policy(Effect::Write), false, false), ASK_WRITE);
        assert_eq!(
            check("t", &policy(Effect::Write), true, false),
            Check::Allow
        );
        // The host's own grant (workspace writes, grandfathered connectors).
        let own = ToolPolicy::granted_write(false);
        assert_eq!(check("t", &own, false, false), Check::Allow);
        assert!(!needs_check("t", &own, false));
    }

    #[test]
    fn destructive_asks_every_time() {
        let p = policy(Effect::Destructive);
        let ask = Check::Ask {
            effect: Effect::Destructive,
            always: false,
        };
        assert_eq!(check("t", &p, false, false), ask);
        assert_eq!(
            check("t", &p, true, false),
            ask,
            "grants don't cover destructive"
        );
        let p = ToolPolicy { granted: true, ..p };
        assert_eq!(check("t", &p, true, false), ask);
    }

    #[test]
    fn requires_approval_forces_a_prompt() {
        let p = policy(Effect::Read).forced(true);
        let ask = Check::Ask {
            effect: Effect::Read,
            always: true,
        };
        assert_eq!(check("t", &p, false, false), ask);
        assert_eq!(check("t", &p, true, false), Check::Allow);
        let p = ToolPolicy::granted_write(false).forced(true);
        assert_eq!(check("t", &p, false, false), ASK_WRITE);
    }

    #[test]
    fn shell_outside_the_allowlist_is_destructive() {
        let p = policy(Effect::Destructive).forced(true);
        assert_eq!(
            check("shell", &p, false, false),
            Check::Ask {
                effect: Effect::Destructive,
                always: true
            }
        );
        assert_eq!(check("shell", &p, true, false), Check::Allow);
    }

    #[test]
    fn money_movement_is_forbidden_whatever_else() {
        for effect in [Effect::Read, Effect::Write, Effect::Destructive] {
            let p = ToolPolicy {
                forbidden: Some("money_movement"),
                ..ToolPolicy::granted_write(true)
            };
            let p = ToolPolicy { effect, ..p };
            assert_eq!(check("t", &p, true, false), Check::Forbid("money_movement"));
            assert!(needs_check("t", &p, false));
        }
        assert!(forbidden_message("trade").contains("`trade`"));
    }

    #[test]
    fn grants_never_cover_a_tainted_write() {
        let never = |effect| Check::Ask {
            effect,
            always: false,
        };
        // An "always" entry, the host's own grant, and the shell allowlist.
        assert_eq!(
            check("t", &policy(Effect::Write), true, true),
            never(Effect::Write)
        );
        let own = ToolPolicy::granted_write(true);
        assert_eq!(check("write_file", &own, false, true), never(Effect::Write));
        assert!(needs_check("write_file", &own, true));
        let shell = policy(Effect::Destructive).forced(true);
        assert_eq!(check("shell", &shell, true, true), never(Effect::Write));
        assert_eq!(
            check("t", &policy(Effect::Destructive), true, true),
            never(Effect::Destructive)
        );
        // Reads don't ask, coordinating sub-agents keeps its grant, and
        // forbidden stays forbidden.
        assert_eq!(check("t", &policy(Effect::Read), false, true), Check::Allow);
        assert!(!needs_check("agent_spawn", &own, true));
        let money = ToolPolicy {
            forbidden: Some("trade"),
            ..own
        };
        assert_eq!(check("t", &money, true, true), Check::Forbid("trade"));
        assert!(taint_note("http_fetch").contains("untrusted content"));
    }

    #[test]
    fn non_shell_tools_allow_all_with_star() {
        let call = ToolCall {
            id: "1".into(),
            name: "write_file".into(),
            input: json!({}),
        };
        assert_eq!(pattern_for(&call).as_deref(), Some("*"));
        assert!(allowed_by(&["*".to_owned()], &call));
        assert!(!allowed_by(&[], &call));
    }
}
