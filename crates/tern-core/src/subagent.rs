//! Sub-agents (docs/subagents-design.md): child runs started by a run's
//! `agent_spawn` calls. Core defines what the `agent_*` tools and the agent
//! loop need; the implementation that creates and drives child runs lives in
//! the binary (`crates/tern/src/subagents.rs`), like other traits (DECISIONS M1).

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tern_store::{MailRow, RunRow};
use tokio_util::sync::CancellationToken;

use crate::agent::RunOutcome;
use crate::config::SubagentsConfig;
use crate::journal::StepError;
use crate::tool::ToolCtx;

/// Dedupe-key prefix of the mail a child posts to its parent when it ends.
pub const FINISHED_PREFIX: &str = "finished:";
/// Default for `[subagents].max_result_chars`: a child's report reaches its
/// parent cut to this many characters (about 2,000 tokens); the rest stays
/// in the child's session and is fetched with `agent_result`.
pub const DEFAULT_RESULT_CHARS: usize = 8_000;
/// The most characters of reports one call (`agent_wait`, `delegate_team`)
/// puts in the caller's context, all reports together. Each report is also
/// cut to `max_result_chars`; when together they'd go over, the longer ones
/// are cut further so every report still arrives.
pub const FANOUT_RESULT_CHARS: usize = 24_000;
/// Label prefix of the sub-agents a `delegate_team` call starts.
pub const TEAM_LABEL_PREFIX: &str = "team-";
/// The fewest characters of a report a fan-out budget leaves it.
const FANOUT_MIN_SHARE: usize = 600;
/// A team call splits a question across this many teammates, at most.
pub const MAX_TEAM_LANES: usize = 3;
/// How much of the brief events and status lists carry.
pub const TASK_PREVIEW: usize = 500;
/// Session source of a child's session.
pub const SESSION_SOURCE: &str = "subagent";
/// How much of a child's report `ChildRun::result_preview` carries.
pub const RESULT_PREVIEW: usize = 300;
/// Dedupe-key prefix of a child's `agent_ask_parent` question:
/// `ask:<child run>:<call id>`.
pub const ASK_PREFIX: &str = "ask:";

/// Tool names of the parent-side tools; children get them only below the
/// depth limit.
pub const AGENT_TOOLS: &[&str] = &[
    "agent_spawn",
    "agent_wait",
    "agent_send",
    "agent_cancel",
    "agent_status",
    "agent_result",
    "read_report",
    "delegate_team",
];
/// Tools only sub-agents get; top-level runs never see them.
pub const CHILD_ONLY_TOOLS: &[&str] = &["agent_ask_parent"];
/// `tools: "read_only"`.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "search_files",
    "http_fetch",
    "memory_search",
    "session_search",
    "skill_read",
];
/// Persistent, user-facing changes belong to the main agent.
pub const CHILD_EXCLUDED_TOOLS: &[&str] = &[
    "schedule_create",
    "schedule_update",
    "schedule_delete",
    "wait_until",
    "wait_list",
    "wait_cancel",
    "connector_activate",
    "profile_edit",
    "role_set",
    "agent_create",
    "agent_edit",
    "agent_delete",
    "channel_create",
    "channel_edit",
    "channel_delete",
    "brief_team",
];

/// Numeric limits from `[subagents]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentLimits {
    pub max_depth: u32,
    pub max_concurrent: u32,
    pub max_children: u32,
    pub max_concurrent_global: u32,
    pub max_iterations: u32,
    pub max_iterations_ceiling: u32,
    /// 0 = none.
    pub budget_tokens: u64,
    /// 0 = no ceiling.
    pub budget_tokens_ceiling: u64,
    /// The most characters of a report that reach the parent's context.
    pub max_result_chars: usize,
}

impl From<&SubagentsConfig> for SubagentLimits {
    fn from(c: &SubagentsConfig) -> Self {
        Self {
            max_depth: c.max_depth,
            max_concurrent: c.max_concurrent.max(1),
            max_children: c.max_children,
            max_concurrent_global: c.max_concurrent_global.max(1),
            max_iterations: c.max_iterations.max(1),
            max_iterations_ceiling: c.max_iterations_ceiling.max(c.max_iterations).max(1),
            budget_tokens: c.budget_tokens,
            budget_tokens_ceiling: match c.budget_tokens_ceiling {
                0 => 0,
                ceiling => ceiling.max(c.budget_tokens),
            },
            max_result_chars: c.max_result_chars.max(200),
        }
    }
}

/// `agent_spawn` arguments.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnRequest {
    pub label: String,
    pub task: String,
    #[serde(default)]
    pub tools: Option<ToolsArg>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub budget_tokens: Option<u64>,
    /// May post to the channel (`channel_post`), and its report is posted
    /// when it finishes (docs/channels-design.md §2.4, open question 4).
    #[serde(default)]
    pub post: bool,
    /// Run it as this agent profile (id, @name or name), one the caller
    /// may delegate to (agent profiles §6.1).
    #[serde(default)]
    pub agent: Option<String>,
    /// Narrow the sub-agent to a folder inside the caller's workspace (a
    /// relative path to an existing directory). It can only go deeper: it
    /// can't widen, name another workspace, or leave through `..`.
    #[serde(default)]
    pub subdir: Option<String>,
}

/// `"all"`, `"read_only"`, or a list of tool names.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ToolsArg {
    Preset(String),
    Names(Vec<String>),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitMode {
    #[default]
    All,
    Any,
}

/// `agent_wait` arguments.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitRequest {
    /// Labels; `None` = every child not reported yet.
    #[serde(default)]
    pub agents: Option<Vec<String>>,
    #[serde(default)]
    pub mode: WaitMode,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// `agent_result` arguments: fetch (more of) a finished child's report.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultRequest {
    pub agent: String,
    /// Characters to skip; default 0.
    #[serde(default)]
    pub offset: Option<usize>,
    /// At most this many characters (default and ceiling: `max_result_chars`).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One teammate's part of a `delegate_team` call.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lane {
    /// The teammate (its name, e.g. `scout`).
    pub agent: String,
    /// The complete brief for that part: it sees nothing else.
    pub task: String,
}

/// `delegate_team` arguments.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamRequest {
    /// Two or three lanes, each for a different teammate.
    pub lanes: Vec<Lane>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// `agent_cancel` arguments.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    pub agent: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `agent_send` arguments.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendRequest {
    pub agent: String,
    pub message: String,
    #[serde(default)]
    pub question_id: Option<String>,
}

/// `agent_ask_parent` arguments.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskRequest {
    pub question: String,
    #[serde(default)]
    pub recommendation: Option<String>,
}

/// Body of `question` mail (child → parent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub agent: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
}

/// Body of `answer` mail (parent → child, for a pending question) and of
/// `message` mail (steering from the parent or the user).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_id: Option<String>,
}

/// The id the parent uses to answer the question in mail `mail_id`.
pub fn question_id(mail_id: i64) -> String {
    format!("q_{mail_id}")
}

/// A sub-agent run as front ends list it (`GET /v1/runs/{id}/children`,
/// `GET /v1/sessions/{id}/subagents`, the TUI's `/agents`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildRun {
    pub run_id: String,
    pub session_id: String,
    pub parent_run_id: String,
    /// The `agent_spawn` tool call that started it.
    pub call_id: String,
    pub label: String,
    /// The first 500 characters of the brief.
    pub task: String,
    pub model: Option<String>,
    pub depth: u32,
    /// `queued`, `running`, `question` (blocked in `agent_ask_parent`),
    /// `completed`, `stopped`, `failed`, `cancelled` or `interrupted`.
    pub status: String,
    pub error: Option<String>,
    /// Unix seconds.
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub usage: crate::types::Usage,
    /// The first 300 characters of its report.
    pub result_preview: Option<String>,
    pub pending_question: Option<String>,
    /// The agent profile it runs as, when the parent delegated to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// Body of `finished` mail: how a child ended, as its parent sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finished {
    pub agent: String,
    pub run_id: String,
    /// `completed`, `stopped` (budget or iterations), `failed` or `cancelled`.
    pub status: String,
    /// The final text, cut to `max_result_chars` with a marker naming
    /// `agent_result` when it was longer; partial for `stopped`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// The full report's length in characters, set when `result` is cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub iterations: u32,
    pub tokens: u64,
}

/// One delivered piece of mail, as an `inbox` step records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxEntry {
    pub id: i64,
    pub kind: String,
    pub from_run_id: Option<String>,
    /// The sending child's label and status, for `finished` mail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Sent by a tainted run: reading it taints this one (tooling spec §5.3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tainted: bool,
}

/// Journaled output of an `inbox` step.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InboxOutput {
    pub mail: Vec<InboxEntry>,
    /// True for the blocking inbox at the end of a turn (live children).
    pub waited: bool,
}

/// What the `agent_*` tools and the agent loop call.
#[async_trait]
pub trait SubagentHost: Send + Sync {
    /// Create (or, re-executed with the same `call_id`, find) a child of
    /// `ctx.run_id` and start or queue it. Returns the tool's JSON result.
    async fn spawn(&self, ctx: &ToolCtx, call_id: &str, req: SpawnRequest)
    -> Result<Value, String>;
    /// Block until children finish (see `agent_wait`). Reports it delivers
    /// are consumed with [`crate::tool::consume_mail`].
    async fn wait(&self, ctx: &ToolCtx, req: WaitRequest) -> Result<Value, String>;
    /// A finished child's full report, a slice at a time (`agent_result`).
    async fn result(&self, ctx: &ToolCtx, req: ResultRequest) -> Result<Value, String>;
    /// Hand each lane to its teammate, wait for all, and return their
    /// reports together (`delegate_team`), keyed by `call_id` so a
    /// re-execution doesn't start them twice.
    async fn delegate_team(
        &self,
        ctx: &ToolCtx,
        call_id: &str,
        req: TeamRequest,
    ) -> Result<Value, String>;
    /// Cancel a child and its subtree.
    async fn cancel(&self, ctx: &ToolCtx, req: CancelRequest) -> Result<Value, String>;
    /// Every child of `ctx.run_id` with status, activity and usage.
    async fn status(&self, ctx: &ToolCtx) -> Result<Value, String>;
    /// Answer a child's question or steer it (`agent_send`), keyed by
    /// `call_id` so a re-execution doesn't send twice.
    async fn send(&self, ctx: &ToolCtx, call_id: &str, req: SendRequest) -> Result<Value, String>;
    /// Ask the parent of `ctx.run_id` and block until it answers or the
    /// configured timeout passes (`agent_ask_parent`). The answer mail is
    /// consumed with [`crate::tool::consume_mail`].
    async fn ask_parent(
        &self,
        ctx: &ToolCtx,
        call_id: &str,
        req: AskRequest,
    ) -> Result<Value, String>;

    /// Whether `run_id` has mail waiting or, with `or_live`, children that
    /// haven't reported back yet.
    fn inbox_due(&self, run_id: &str, or_live: bool) -> Result<bool, StepError>;
    /// `run_id`'s pending mail. With `wait`, first block until there is
    /// some or no child is outstanding. Doesn't consume it.
    async fn take_mail(
        &self,
        run_id: &str,
        wait: bool,
        cancel: &CancellationToken,
    ) -> Result<Vec<MailRow>, StepError>;
    /// Why `run` was cancelled, recorded as its error so it isn't resumed
    /// as if it had crashed.
    fn interrupt_reason(&self, run: &RunRow) -> Option<String>;
    /// `run` stopped being driven (its status is already stored): report a
    /// child to its parent, cancel children still running, start queued ones.
    async fn run_ended(&self, run: &RunRow, outcome: &RunOutcome);
}

/// The host, bound after the tools and the agent are built (they're built
/// first, in `Runtime::open`).
#[derive(Clone, Default)]
pub struct HostSlot(Arc<OnceLock<Arc<dyn SubagentHost>>>);

impl HostSlot {
    pub fn set(&self, host: Arc<dyn SubagentHost>) {
        let _ = self.0.set(host);
    }

    pub fn get(&self) -> Option<&Arc<dyn SubagentHost>> {
        self.0.get()
    }
}

/// How a child's end looks to its parent: `stopped` for a run that hit its
/// budget or iteration limit, `cancelled` for an interrupted one.
pub fn finished_status(status: &str, final_text: &str) -> &'static str {
    match status {
        "completed" if final_text.contains(crate::agent::STOPPED_MARKER) => "stopped",
        "completed" => "completed",
        "interrupted" => "cancelled",
        _ => "failed",
    }
}

/// Whether an `interrupted` run was stopped on purpose (so it must not be
/// resumed as though a crash had cut it off).
pub fn was_cancelled(run: &RunRow) -> bool {
    run.status == "interrupted"
        && run
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("cancelled"))
}

/// The user-message text an inbox step appends for one piece of mail.
pub fn render_mail(m: &MailRow) -> String {
    let body: Value = serde_json::from_str(&m.body).unwrap_or(Value::Null);
    if m.kind == "finished"
        && let Ok(f) = serde_json::from_value::<Finished>(body.clone())
    {
        let next = unfinished_guidance(&f)
            .map(|g| format!("\n\n{g}"))
            .unwrap_or_default();
        let detail = match f.status.as_str() {
            "completed" | "stopped" => f.result.unwrap_or_default(),
            _ => f.error.or(f.result).unwrap_or_default(),
        };
        let head = match f.status.as_str() {
            "cancelled" => format!("[sub-agent {} was cancelled]", f.agent),
            s => format!("[sub-agent {} {s}]", f.agent),
        };
        return format!("{head}\n{}{next}", detail.trim_end());
    }
    if m.kind == "question"
        && let Ok(q) = serde_json::from_value::<Question>(body.clone())
    {
        let mut out = format!(
            "[sub-agent {} asks ({})]\n{}",
            q.agent,
            question_id(m.id),
            q.question.trim_end()
        );
        if let Some(r) = q.recommendation.filter(|r| !r.trim().is_empty()) {
            out.push_str(&format!("\nIts recommendation: {}", r.trim_end()));
        }
        out.push_str("\nIt is blocked until you answer with agent_send.");
        return out;
    }
    let text = body
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| m.body.clone());
    match (m.from_run_id.as_deref(), m.kind.as_str()) {
        // tern's own note about an outbox item; the text says what it is.
        (None, crate::outbox::MAIL_KIND) => text,
        (None, _) => format!("[message from the user to this agent]\n{text}"),
        (Some(_), "answer") => format!("[answer from the main agent]\n{text}"),
        (Some(_), _) => format!("[message from the main agent]\n{text}"),
    }
}

/// What the caller should do about a hand-off that didn't finish, and what
/// got done: why it ended (`stopped` ran out of steps or tokens, `failed`,
/// `cancelled` by the user or by the caller), what it left, and the next
/// step. The host decides this; the model only reads it. `None` for a
/// `completed` hand-off.
pub fn unfinished_guidance(f: &Finished) -> Option<String> {
    let done = match f.result.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(_) => format!(
            "What got done: its partial report is above ({} steps, {} tokens).",
            f.iterations, f.tokens
        ),
        None if f.iterations > 0 => format!(
            "What got done: {} steps, but it left no report.",
            f.iterations
        ),
        None => "What got done: nothing; it never got going.".to_owned(),
    };
    let by_user = f
        .error
        .as_deref()
        .is_some_and(|e| e.starts_with("cancelled by the user"));
    Some(match f.status.as_str() {
        "completed" => return None,
        "stopped" => format!(
            "It stopped: it ran out of steps or tokens before it was done. {done} Next: ask the user whether to continue, with a larger budget or a narrower brief, or to leave it there. Don't start it again on your own."
        ),
        "cancelled" if by_user => format!(
            "The user stopped it. {done} Next: don't restart it. Tell the user what got done and ask what they want."
        ),
        "cancelled" => format!("It was cancelled. {done} Next: only redo it if you still need it."),
        _ => format!(
            "It failed{}. {done} Next: try once more, differently (a clearer brief, another teammate or another approach), or take the part over yourself. If that fails too, tell the user why and what got done.",
            f.error
                .as_deref()
                .map(|e| format!(": {}", e.trim()))
                .unwrap_or_default()
        ),
    })
}

/// Cut the reports in `entries` (the `agent_wait`/`delegate_team` result
/// entries: `agent`, `result`, optional `result_chars`) so together they hold
/// at most `budget` characters. Short reports stay whole; the budget left is
/// shared evenly among the longer ones, each keeping at least a few hundred
/// characters. A cut report ends in a marker saying how to read on. Returns
/// whether any was cut.
pub fn share_budget(entries: &mut [Value], budget: usize) -> bool {
    let lens: Vec<usize> = entries
        .iter()
        .map(|e| {
            e.get("result")
                .and_then(Value::as_str)
                .map_or(0, |r| r.chars().count())
        })
        .collect();
    if lens.iter().sum::<usize>() <= budget {
        return false;
    }
    // Water-filling: the shortest take what they need, the rest split what
    // is left.
    let mut order: Vec<usize> = (0..lens.len()).filter(|i| lens[*i] > 0).collect();
    order.sort_by_key(|i| lens[*i]);
    let mut left = budget;
    let mut share = vec![0usize; lens.len()];
    for (n, &i) in order.iter().enumerate() {
        let fair = (left / (order.len() - n)).max(FANOUT_MIN_SHARE);
        share[i] = lens[i].min(fair);
        left = left.saturating_sub(share[i]);
    }
    let mut cut = false;
    for (i, e) in entries.iter_mut().enumerate() {
        if share[i] == 0 || share[i] >= lens[i] {
            continue;
        }
        let Some(result) = e.get("result").and_then(Value::as_str) else {
            continue;
        };
        let agent = e
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let total = e
            .get("result_chars")
            .and_then(Value::as_u64)
            .map_or(lens[i], |t| t as usize);
        let end = result
            .char_indices()
            .nth(share[i])
            .map_or(result.len(), |(b, _)| b);
        e["result"] = format!(
            "{}\n[... cut to share this call's {budget}-character report budget: showing the first {} of {total} characters. Read on with read_report {{\"agent\": \"{agent}\", \"offset\": {}}}]",
            &result[..end],
            share[i],
            share[i]
        )
        .into();
        cut = true;
    }
    cut
}

/// `text` cut to at most `max` characters for the parent's context. When it
/// was longer, the cut text ends in a marker that says how much was shown,
/// of how many, and how to get the rest. Returns the text and, when cut, the
/// full length in characters.
pub fn cap_report(text: &str, max: usize, agent: &str) -> (String, Option<usize>) {
    let total = text.chars().count();
    if total <= max {
        return (text.to_owned(), None);
    }
    let end = text.char_indices().nth(max).map_or(text.len(), |(i, _)| i);
    (
        format!(
            "{}\n[... report truncated: showing the first {max} of {total} characters. Fetch the rest with read_report {{\"agent\": \"{agent}\", \"offset\": {max}}}]",
            &text[..end]
        ),
        Some(total),
    )
}

/// `s` cut to at most `max` bytes on a char boundary.
pub fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    format!(
        "{}\n[... cut at {max} bytes]",
        crate::text::prefix_bytes(s, max)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(kind: &str, from: Option<&str>, body: Value) -> MailRow {
        MailRow {
            id: 1,
            to_run_id: "p".into(),
            from_run_id: from.map(Into::into),
            kind: kind.into(),
            body: body.to_string(),
            dedupe_key: "k".into(),
            created_at: 0,
            consumed_run_id: None,
            consumed_step: None,
        }
    }

    #[test]
    fn finished_mail_renders_the_report_or_the_reason() {
        let f = |status: &str, result: Option<&str>, error: Option<&str>| {
            serde_json::to_value(Finished {
                agent: "research-1".into(),
                run_id: "c".into(),
                status: status.into(),
                result: result.map(Into::into),
                error: error.map(Into::into),
                result_chars: None,
                iterations: 2,
                tokens: 10,
            })
            .unwrap()
        };
        let m = mail(
            "finished",
            Some("c"),
            f("completed", Some("the report"), None),
        );
        assert_eq!(
            render_mail(&m),
            "[sub-agent research-1 completed]\nthe report"
        );
        let m = mail(
            "finished",
            Some("c"),
            f(
                "cancelled",
                None,
                Some("cancelled by the parent agent: enough"),
            ),
        );
        let text = render_mail(&m);
        assert!(
            text.starts_with(
                "[sub-agent research-1 was cancelled]\ncancelled by the parent agent: enough"
            ),
            "{text}"
        );
        assert!(text.contains("only redo it if you still need it"), "{text}");
        let m = mail("message", None, serde_json::json!({"text": "hurry up"}));
        assert_eq!(
            render_mail(&m),
            "[message from the user to this agent]\nhurry up"
        );
        let m = mail("message", Some("p"), serde_json::json!({"text": "focus"}));
        assert_eq!(render_mail(&m), "[message from the main agent]\nfocus");
        let m = mail(
            "answer",
            Some("p"),
            serde_json::json!({"text": "no", "question_id": "q_1"}),
        );
        assert_eq!(render_mail(&m), "[answer from the main agent]\nno");
        let m = mail(
            "question",
            Some("c"),
            serde_json::json!({"agent": "scan", "question": "Private too?", "recommendation": "no"}),
        );
        assert_eq!(
            render_mail(&m),
            "[sub-agent scan asks (q_1)]\nPrivate too?\nIts recommendation: no\nIt is blocked until you answer with agent_send."
        );
    }

    fn finished(status: &str, result: Option<&str>, error: Option<&str>) -> Finished {
        Finished {
            agent: "scout".into(),
            run_id: "c".into(),
            status: status.into(),
            result: result.map(Into::into),
            result_chars: None,
            error: error.map(Into::into),
            iterations: 7,
            tokens: 1200,
        }
    }

    #[test]
    fn an_unfinished_hand_off_says_what_got_done_and_what_next() {
        assert!(unfinished_guidance(&finished("completed", Some("x"), None)).is_none());
        // A stop (limits): ask the user, partial work named.
        let g = unfinished_guidance(&finished("stopped", Some("half"), None)).unwrap();
        assert!(g.contains("ran out of steps or tokens"), "{g}");
        assert!(g.contains("partial report is above (7 steps"), "{g}");
        assert!(g.contains("ask the user"), "{g}");
        // The user's own stop: don't restart, ask.
        let g = unfinished_guidance(&finished("cancelled", None, Some("cancelled by the user")))
            .unwrap();
        assert!(g.contains("The user stopped it"), "{g}");
        assert!(
            g.contains("don't restart it") && g.contains("ask what they want"),
            "{g}"
        );
        // A failure: the error, once more differently, then tell the user.
        let g = unfinished_guidance(&finished("failed", None, Some("provider down"))).unwrap();
        assert!(g.contains("It failed: provider down"), "{g}");
        assert!(g.contains("left no report"), "{g}");
        assert!(g.contains("try once more, differently"), "{g}");
        assert!(g.contains("tell the user why"), "{g}");
        // It reaches the caller in the mail it reads.
        let m = mail(
            "finished",
            Some("c"),
            serde_json::to_value(finished("failed", None, Some("provider down"))).unwrap(),
        );
        assert!(render_mail(&m).contains("try once more, differently"));
    }

    #[test]
    fn reports_that_arrive_together_share_one_budget() {
        let entry =
            |agent: &str, n: usize| serde_json::json!({ "agent": agent, "result": "x".repeat(n) });
        // Within the budget: untouched.
        let mut small = vec![entry("a", 100), entry("b", 200)];
        assert!(!share_budget(&mut small, 24_000));
        // Three long reports share it; a short one stays whole.
        let mut v = vec![
            entry("a", 8_000),
            entry("b", 8_000),
            entry("short", 500),
            entry("c", 8_000),
        ];
        assert!(share_budget(&mut v, 20_000));
        let len = |i: usize| v[i]["result"].as_str().unwrap().chars().count();
        assert_eq!(
            v[2]["result"].as_str().unwrap().chars().count(),
            500,
            "short stays whole"
        );
        let shown: usize = [0, 1, 3]
            .iter()
            .map(|&i| {
                v[i]["result"]
                    .as_str()
                    .unwrap()
                    .split("\n[...")
                    .next()
                    .unwrap()
                    .len()
            })
            .sum();
        assert!(shown + 500 <= 20_000, "{shown}");
        assert!(len(0) < 8_000 && len(1) < 8_000 && len(3) < 8_000);
        let r = v[0]["result"].as_str().unwrap();
        assert!(
            r.contains("read_report {\"agent\": \"a\", \"offset\":"),
            "{r}"
        );
        assert!(r.contains("of 8000 characters"), "{r}");
    }

    #[test]
    fn statuses_as_the_parent_sees_them() {
        assert_eq!(finished_status("completed", "done"), "completed");
        assert_eq!(
            finished_status("completed", "partial\n\n[stopped: token budget]"),
            "stopped"
        );
        assert_eq!(finished_status("interrupted", ""), "cancelled");
        assert_eq!(finished_status("failed", ""), "failed");
        assert_eq!(cap("héllo", 2), "h\n[... cut at 2 bytes]");
    }

    #[test]
    fn tools_argument_accepts_presets_and_lists() {
        let r: SpawnRequest = serde_json::from_value(serde_json::json!({
            "label": "a", "task": "t", "tools": ["read_file"]
        }))
        .unwrap();
        assert!(matches!(r.tools, Some(ToolsArg::Names(n)) if n == ["read_file"]));
        let r: SpawnRequest = serde_json::from_value(
            serde_json::json!({"label": "a", "task": "t", "tools": "read_only"}),
        )
        .unwrap();
        assert!(matches!(r.tools, Some(ToolsArg::Preset(p)) if p == "read_only"));
    }
}
