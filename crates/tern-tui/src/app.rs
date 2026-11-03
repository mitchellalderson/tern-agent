//! TUI state and its reactions to keys, agent events, and run completion.
//! No terminal or async code here, so it is unit-testable.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tern_core::agent::{RunOutcome, RunStatus};
use tern_core::approval::{ApprovalOutcome, Decision};
use tern_core::events::AgentEvent;
use tern_core::types::Usage;

use crate::history;
use crate::input::Input;
use crate::secrets::SecretsPanel;
use crate::{Backend, BackendResult, PendingApproval, SessionInfo};

/// Two Ctrl+C presses within this window quit.
pub const DOUBLE_CTRL_C: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Ok,
    Failed,
    Denied,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User(String),
    Assistant {
        text: String,
        streaming: bool,
    },
    Tool {
        id: String,
        summary: String,
        state: ToolState,
        output: String,
    },
    Summary(String),
    Notice(String),
    Error(String),
    /// One line per sub-agent, updated in place.
    Agent(AgentLine),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Queued,
    Running,
    /// Blocked on `agent_ask_parent`.
    Question,
    /// Waiting for the user to answer one of its approvals.
    Approval,
    Completed,
    Failed,
    Cancelled,
}

impl AgentState {
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Running | Self::Question | Self::Approval
        )
    }
}

/// A sub-agent of the current run, as its line in the transcript shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLine {
    pub run_id: String,
    pub label: String,
    pub state: AgentState,
    /// What it is doing ("Running shell cargo test", "Asked: …").
    pub activity: String,
    pub tools: u32,
    pub tokens: u64,
    pub started: Instant,
    /// Set when it finished.
    pub elapsed: Option<Duration>,
    /// Its latest assistant text (its report once it has finished).
    pub result: String,
}

/// A read-only transcript drawn over the main one (`/agent <label>`).
pub struct Overlay {
    pub title: String,
    pub entries: Vec<Entry>,
    /// Lines scrolled up from the bottom.
    pub scroll: usize,
}

/// Work the event loop must do on the app's behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Quit,
    Drive {
        run_id: String,
        resume: bool,
    },
    Cancel,
    /// The backend was rebuilt; subscribe to its event bus again.
    Resubscribe,
}

pub struct Running {
    pub run_id: String,
    pub started: Instant,
}

pub struct App {
    backend: Arc<dyn Backend>,
    pub entries: Vec<Entry>,
    pub input: Input,
    /// Transcript lines scrolled up from the bottom.
    pub scroll: usize,
    /// Show full tool output and summaries.
    pub expanded: bool,
    pub session: Option<SessionInfo>,
    /// Model for the next run (session model, `/model`, or the default).
    pub model: Option<String>,
    pub running: Option<Running>,
    pub approval: Option<PendingApproval>,
    /// Further approvals (sub-agents ask concurrently), shown in turn.
    approval_queue: VecDeque<PendingApproval>,
    pub last_usage: Option<Usage>,
    pub hint: Option<String>,
    /// The `/secrets` panel, drawn over the transcript while open.
    pub secrets: Option<SecretsPanel>,
    /// A sub-agent's transcript, drawn over the transcript while open.
    pub overlay: Option<Overlay>,
    /// Sub-agent runs of the current tree. Their events update their
    /// `Entry::Agent` line and never the transcript.
    agents: HashSet<String>,
    /// The run `/agents` and `/agent` look at: the current or last one.
    last_run: Option<String>,
    queued: Option<String>,
    last_ctrl_c: Option<Instant>,
}

const HELP: &str = "\
Commands:
  /new              start a new session
  /model [p:model]  show or set the model
  /sessions         list recent sessions
  /resume <id>      open a session, or resume an interrupted run
  /tools            list available tools
  /stop             cancel the current run
  /reload           re-read config and rebuild tools
  /secrets [NAME]   manage API keys and tokens in secrets.toml
  /usage            token usage for this session
  /agents           list the last run's sub-agents
  /agent <label>    show a sub-agent's transcript (Esc closes)
  /agent <label> <message>  send a running sub-agent a message
  /quit             exit
Keys: Enter send · Shift/Alt+Enter or Ctrl+J newline · Ctrl+O expand tool output
      PgUp/PgDn scroll · Ctrl+C cancel (twice to quit)";

impl App {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let model = backend.default_model();
        Self {
            backend,
            entries: Vec::new(),
            input: Input::default(),
            scroll: 0,
            expanded: false,
            session: None,
            model,
            running: None,
            approval: None,
            approval_queue: VecDeque::new(),
            last_usage: None,
            hint: None,
            secrets: None,
            overlay: None,
            agents: HashSet::new(),
            last_run: None,
            queued: None,
            last_ctrl_c: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    // ---- keys ----

    pub fn on_key(&mut self, key: KeyEvent, now: Instant) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return vec![];
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if ctrl && key.code == KeyCode::Char('c') {
            return self.ctrl_c(now);
        }
        self.hint = None;

        // The panel takes every key, so a value being typed can't answer
        // an approval or land in the chat input.
        if let Some(panel) = &mut self.secrets {
            if !panel.on_key(key, self.backend.as_ref()) {
                self.secrets = None;
            }
            return vec![];
        }

        if let Some(o) = &mut self.overlay {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Up => o.scroll = o.scroll.saturating_add(1),
                KeyCode::Down => o.scroll = o.scroll.saturating_sub(1),
                KeyCode::PageUp => o.scroll = o.scroll.saturating_add(10),
                KeyCode::PageDown => o.scroll = o.scroll.saturating_sub(10),
                KeyCode::Home => o.scroll = usize::MAX,
                KeyCode::End => o.scroll = 0,
                _ => {}
            }
            return vec![];
        }

        if self.approval.is_some() {
            let decision = match key.code {
                KeyCode::Char('y' | 'Y') => Decision::AllowOnce,
                KeyCode::Char('a' | 'A')
                    if self
                        .approval
                        .as_ref()
                        .is_some_and(|p| p.request.pattern.is_some()) =>
                {
                    Decision::AlwaysAllow
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Decision::Deny,
                _ => return vec![],
            };
            self.answer_approval(decision);
            return vec![];
        }

        match key.code {
            KeyCode::Enter if shift || alt => self.input.insert('\n'),
            KeyCode::Char('j') if ctrl => self.input.insert('\n'),
            KeyCode::Enter => return self.submit(),
            KeyCode::Char('o') if ctrl => self.expanded = !self.expanded,
            KeyCode::Char('d') if ctrl && self.input.text().is_empty() => {
                return vec![Effect::Quit];
            }
            KeyCode::Char('u') if ctrl => self.input.kill_line(),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char(c) if !ctrl => self.input.insert(c),
            KeyCode::Tab => self.input.insert_str("    "),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home if ctrl => self.scroll = usize::MAX,
            KeyCode::End if ctrl => self.scroll = 0,
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up if shift => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Down if shift => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Up => {
                self.input.up();
            }
            KeyCode::Down => {
                self.input.down();
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Esc => self.input.clear(),
            _ => {}
        }
        vec![]
    }

    /// Run a slash command as if typed (e.g. `tern --resume <id>`).
    pub fn on_key_command(&mut self, line: &str) -> Vec<Effect> {
        self.command(line.trim_start_matches('/'))
    }

    pub fn on_paste(&mut self, text: &str) {
        if let Some(panel) = &mut self.secrets {
            panel.on_paste(text);
        } else if self.approval.is_none() {
            self.input
                .insert_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
        }
    }

    fn ctrl_c(&mut self, now: Instant) -> Vec<Effect> {
        if self
            .last_ctrl_c
            .is_some_and(|t| now.duration_since(t) < DOUBLE_CTRL_C)
        {
            return vec![Effect::Quit];
        }
        self.last_ctrl_c = Some(now);
        self.hint = Some("press Ctrl+C again to quit".into());
        if self.secrets.take().is_some() || self.overlay.take().is_some() {
            return vec![];
        }
        if self.running.is_some() {
            // The whole tree stops, so deny everything it is waiting on.
            self.deny_all_approvals();
        } else if self.approval.is_some() {
            self.answer_approval(Decision::Deny);
        }
        if self.running.is_some() {
            self.queued = None;
            return vec![Effect::Cancel];
        }
        self.input.clear();
        vec![]
    }

    fn answer_approval(&mut self, decision: Decision) {
        if let Some(p) = self.approval.take() {
            let _ = p.reply.send(decision);
        }
        self.approval = self.approval_queue.pop_front();
    }

    fn deny_all_approvals(&mut self) {
        while self.approval.is_some() {
            self.answer_approval(Decision::Deny);
        }
    }

    fn submit(&mut self) -> Vec<Effect> {
        if self.input.is_empty() {
            return vec![];
        }
        let text = self.input.take();
        self.scroll = 0;
        if let Some(cmd) = text.trim().strip_prefix('/') {
            return self.command(cmd);
        }
        if self.running.is_some() {
            // A new message cancels the current run, then gets sent.
            self.queued = Some(text);
            self.notice("stopping the current run; your message will be sent next");
            return vec![Effect::Cancel];
        }
        self.send(text)
    }

    fn send(&mut self, text: String) -> Vec<Effect> {
        let session = self.session.as_ref().map(|s| s.id.clone());
        match self
            .backend
            .start_run(session.as_deref(), self.model.as_deref(), &text)
        {
            Ok(started) => {
                self.entries.push(Entry::User(text.trim_end().to_owned()));
                if self.session.is_none() {
                    let title = text.trim().chars().take(80).collect();
                    self.session = Some(SessionInfo {
                        id: started.session_id,
                        title: Some(title),
                        model: None,
                    });
                }
                self.model = Some(started.model);
                self.start_tree(&started.run_id);
                self.running = Some(Running {
                    run_id: started.run_id.clone(),
                    started: Instant::now(),
                });
                vec![Effect::Drive {
                    run_id: started.run_id,
                    resume: false,
                }]
            }
            Err(e) => {
                // Keep the text so the user can fix the problem and resend.
                self.input.insert_str(&text);
                self.error(e);
                vec![]
            }
        }
    }

    // ---- slash commands ----

    fn command(&mut self, line: &str) -> Vec<Effect> {
        let (name, arg) = match line.split_once(char::is_whitespace) {
            Some((n, a)) => (n, a.trim()),
            None => (line, ""),
        };
        let busy = |app: &mut Self| {
            app.error(format!(
                "/{name} isn't available while a run is in progress; /stop it first"
            ));
            vec![]
        };
        match name {
            "help" | "?" => self.notice(HELP),
            "quit" | "exit" => return vec![Effect::Quit],
            "new" => {
                if self.running.is_some() {
                    return busy(self);
                }
                self.entries.clear();
                self.session = None;
                self.model = self.backend.default_model();
                self.last_usage = None;
                self.notice("new session");
            }
            "model" if arg.is_empty() => {
                let m = self
                    .model
                    .clone()
                    .unwrap_or_else(|| "(none configured)".into());
                self.notice(format!("model: {m}"));
            }
            "model" => match self.backend.check_model(arg) {
                Err(e) => self.error(e),
                Ok(()) => {
                    if let Some(s) = &self.session
                        && let Err(e) = self.backend.set_session_model(&s.id, arg)
                    {
                        self.error(e);
                        return vec![];
                    }
                    self.model = Some(arg.to_owned());
                    self.notice(format!("model set to {arg}"));
                }
            },
            "sessions" => match self.backend.sessions(15) {
                Err(e) => self.error(e),
                Ok(list) if list.is_empty() => self.notice("no sessions yet"),
                Ok(list) => {
                    let lines: Vec<String> = list
                        .iter()
                        .map(|s| {
                            format!(
                                "{}  {} msgs  {}",
                                s.session.id,
                                s.messages,
                                s.session.title.as_deref().unwrap_or("(untitled)")
                            )
                        })
                        .collect();
                    self.notice(format!(
                        "recent sessions (open with /resume <id>):\n{}",
                        lines.join("\n")
                    ));
                }
            },
            "resume" if arg.is_empty() => self.error("usage: /resume <session id or run id>"),
            "resume" => {
                if self.running.is_some() {
                    return busy(self);
                }
                return self.resume(arg);
            }
            "tools" => {
                let lines: Vec<String> = self
                    .backend
                    .tools()
                    .into_iter()
                    .map(|(n, d)| format!("{n} — {}", d.lines().next().unwrap_or_default()))
                    .collect();
                self.notice(format!("tools:\n{}", lines.join("\n")));
            }
            "stop" => {
                if self.running.is_some() {
                    self.queued = None;
                    return vec![Effect::Cancel];
                }
                self.notice("nothing is running");
            }
            "reload" => {
                if self.running.is_some() {
                    return busy(self);
                }
                match self.backend.reload() {
                    Ok(msg) => {
                        self.notice(msg);
                        return vec![Effect::Resubscribe];
                    }
                    Err(e) => self.error(e),
                }
            }
            "secrets" => {
                let name = (!arg.is_empty()).then_some(arg);
                match SecretsPanel::open(self.backend.as_ref(), name) {
                    Ok(panel) => self.secrets = Some(panel),
                    Err(e) => self.error(e),
                }
            }
            "usage" => match &self.session {
                None => self.notice("no session yet"),
                Some(s) => match self.backend.usage(&s.id) {
                    Ok(u) => self.notice(format!(
                        "session usage: {} input, {} output, {} cache read, {} cache write tokens",
                        u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_write_tokens
                    )),
                    Err(e) => self.error(e),
                },
            },
            "agents" => self.list_agents(),
            "agent" if arg.is_empty() => self
                .error("usage: /agent <label> to show it, /agent <label> <message> to message it"),
            "agent" => {
                let (label, message) = match arg.split_once(char::is_whitespace) {
                    Some((l, m)) => (l, m.trim()),
                    None => (arg, ""),
                };
                self.agent_command(label, message);
            }
            other => self.error(format!("unknown command /{other}; try /help")),
        }
        vec![]
    }

    /// A new top-level run: forget the previous run's sub-agents.
    fn start_tree(&mut self, run_id: &str) {
        self.agents.clear();
        self.last_run = Some(run_id.to_owned());
    }

    fn list_agents(&mut self) {
        let Some(run) = self.last_run.clone() else {
            self.notice("no run yet");
            return;
        };
        match self.backend.children(&run) {
            Err(e) => self.error(e),
            Ok(list) if list.is_empty() => self.notice("the last run started no sub-agents"),
            Ok(list) => {
                let lines: Vec<String> = list
                    .iter()
                    .map(|c| {
                        let detail = c
                            .pending_question
                            .as_deref()
                            .map(|q| format!("asked: {q}"))
                            .or_else(|| c.error.clone())
                            .or_else(|| c.result_preview.clone())
                            .unwrap_or_default();
                        let detail = detail.lines().next().unwrap_or_default().to_owned();
                        format!("{}  {}  {}  {detail}", c.label, c.status, c.run_id)
                    })
                    .collect();
                self.notice(format!(
                    "sub-agents (show with /agent <label>, message with /agent <label> <text>):\n{}",
                    lines.join("\n")
                ));
            }
        }
    }

    fn agent_command(&mut self, label: &str, message: &str) {
        let Some(run) = self.last_run.clone() else {
            self.error("no run yet");
            return;
        };
        let list = match self.backend.children(&run) {
            Ok(l) => l,
            Err(e) => return self.error(e),
        };
        let Some(child) = list.iter().find(|c| c.label == label) else {
            let labels: Vec<&str> = list.iter().map(|c| c.label.as_str()).collect();
            let known = if labels.is_empty() {
                "none".to_owned()
            } else {
                labels.join(", ")
            };
            return self.error(format!(
                "no sub-agent {label:?}; the last run's are: {known}"
            ));
        };
        if !message.is_empty() {
            match self.backend.message_run(&child.run_id, message) {
                Ok(()) => self.notice(format!("sent to {label}")),
                Err(e) => self.error(e),
            }
            return;
        }
        match self.backend.session(&child.session_id) {
            Ok(Some((_, messages))) => {
                self.overlay = Some(Overlay {
                    title: format!(" {label} · {} · Esc to close ", child.status),
                    entries: history::entries(&messages),
                    scroll: 0,
                });
            }
            Ok(None) => self.error(format!("session {} not found", child.session_id)),
            Err(e) => self.error(e),
        }
    }

    fn resume(&mut self, id: &str) -> Vec<Effect> {
        // A session id opens that conversation.
        match self.backend.session(id) {
            Err(e) => {
                self.error(e);
                return vec![];
            }
            Ok(Some((info, messages))) => {
                self.open_session(info, &messages);
                return vec![];
            }
            Ok(None) => {}
        }
        // A run id opens its session and resumes the run.
        match self.backend.run_session(id) {
            Err(e) => self.error(e),
            Ok(None) => self.error(format!("no session or run with id {id}")),
            Ok(Some(session_id)) => match self.backend.session(&session_id) {
                Ok(Some((info, messages))) => {
                    self.open_session(info, &messages);
                    self.notice(format!("resuming run {id}"));
                    self.start_tree(id);
                    self.running = Some(Running {
                        run_id: id.to_owned(),
                        started: Instant::now(),
                    });
                    return vec![Effect::Drive {
                        run_id: id.to_owned(),
                        resume: true,
                    }];
                }
                Ok(None) => self.error(format!("session {session_id} not found")),
                Err(e) => self.error(e),
            },
        }
        vec![]
    }

    fn open_session(&mut self, info: SessionInfo, messages: &[crate::HistoryMessage]) {
        self.entries = history::entries(messages);
        if let Some(m) = &info.model {
            self.model = Some(m.clone());
        }
        self.notice(format!(
            "opened session {} {}",
            info.id,
            info.title
                .as_deref()
                .map(|t| format!("\"{t}\""))
                .unwrap_or_default()
        ));
        self.session = Some(info);
        self.scroll = 0;
    }

    // ---- agent events ----

    pub fn on_agent_event(&mut self, ev: AgentEvent) {
        // Children run concurrently with their parent: their events update
        // their own line, never the transcript. Events of other runs (another
        // client's, or a previous run's) are ignored.
        if self.agents.contains(ev.run_id()) {
            return self.on_child_event(ev);
        }
        if self
            .running
            .as_ref()
            .is_none_or(|r| r.run_id != ev.run_id())
        {
            return;
        }
        match ev {
            AgentEvent::TextDelta { text, .. } => match self.entries.last_mut() {
                Some(Entry::Assistant {
                    text: t,
                    streaming: true,
                }) => t.push_str(&text),
                _ => self.entries.push(Entry::Assistant {
                    text,
                    streaming: true,
                }),
            },
            AgentEvent::AssistantMessage { text, .. } => {
                if let Some(Entry::Assistant {
                    streaming: true, ..
                }) = self.entries.last()
                {
                    self.entries.pop();
                }
                if !text.trim().is_empty() {
                    self.entries.push(Entry::Assistant {
                        text,
                        streaming: false,
                    });
                }
            }
            // The agent lines stand in for the agent_* calls.
            AgentEvent::ToolStarted { name, .. } if name.starts_with("agent_") => {}
            AgentEvent::ToolStarted { id, summary, .. } => {
                self.entries.push(Entry::Tool {
                    id,
                    summary,
                    state: ToolState::Running,
                    output: String::new(),
                });
            }
            AgentEvent::ToolFinished {
                id,
                is_error,
                output,
                ..
            } => {
                if let Some(Entry::Tool {
                    state, output: o, ..
                }) = self.tool_mut(&id)
                {
                    *state = if is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Ok
                    };
                    *o = output;
                }
            }
            AgentEvent::Approval {
                id, name, outcome, ..
            } => {
                if outcome != ApprovalOutcome::Approved {
                    let why = if outcome == ApprovalOutcome::Timeout {
                        "approval timed out"
                    } else {
                        "not approved"
                    };
                    self.entries.push(Entry::Tool {
                        id,
                        summary: name,
                        state: ToolState::Denied,
                        output: why.into(),
                    });
                }
            }
            AgentEvent::Retrying {
                error,
                delay_ms,
                attempt,
                ..
            } => {
                if let Some(Entry::Assistant {
                    streaming: true, ..
                }) = self.entries.last()
                {
                    self.entries.pop();
                }
                self.notice(format!(
                    "model call failed ({error}); retry {attempt} in {:.1}s",
                    delay_ms as f64 / 1000.0
                ));
            }
            AgentEvent::Compacted { .. } => {
                self.notice("summarized earlier messages to fit the context window")
            }
            AgentEvent::Usage { usage, .. } => self.last_usage = Some(usage),
            AgentEvent::SubagentSpawned {
                child_run_id,
                label,
                status,
                ..
            } => self.add_agent(child_run_id, label, &status),
            AgentEvent::SubagentMessage {
                to_run_id,
                kind,
                text,
                ..
            } => {
                let what = if kind == "answer" { "Answered" } else { "Told" };
                let activity = format!("{what}: {}", first_line(&text));
                self.set_agent_state(&to_run_id, AgentState::Running, Some(activity));
            }
            AgentEvent::RunStarted { .. }
            | AgentEvent::RunFinished { .. }
            | AgentEvent::ToolOutput { .. } => {}
        }
    }

    fn add_agent(&mut self, run_id: String, label: String, status: &str) {
        let queued = status == "queued";
        self.agents.insert(run_id.clone());
        self.entries.push(Entry::Agent(AgentLine {
            run_id,
            label,
            state: if queued {
                AgentState::Queued
            } else {
                AgentState::Running
            },
            activity: if queued { "Queued" } else { "Starting" }.into(),
            tools: 0,
            tokens: 0,
            started: Instant::now(),
            elapsed: None,
            result: String::new(),
        }));
    }

    fn agent_mut(&mut self, run_id: &str) -> Option<&mut AgentLine> {
        self.entries.iter_mut().rev().find_map(|e| match e {
            Entry::Agent(a) if a.run_id == run_id => Some(a),
            _ => None,
        })
    }

    /// Change a live agent's state (finished ones stay finished).
    fn set_agent_state(&mut self, run_id: &str, state: AgentState, activity: Option<String>) {
        if let Some(a) = self.agent_mut(run_id)
            && a.state.is_live()
        {
            a.state = state;
            if let Some(act) = activity {
                a.activity = act;
            }
        }
    }

    /// An event of a sub-agent run: update its line.
    fn on_child_event(&mut self, ev: AgentEvent) {
        let run_id = ev.run_id().to_owned();
        match ev {
            // A grandchild.
            AgentEvent::SubagentSpawned {
                child_run_id,
                label,
                status,
                ..
            } => self.add_agent(child_run_id, label, &status),
            AgentEvent::SubagentMessage { kind, text, .. } if kind == "question" => {
                let activity = format!("Asked: {}", first_line(&text));
                self.set_agent_state(&run_id, AgentState::Question, Some(activity));
            }
            AgentEvent::RunStarted { .. } => {
                self.set_agent_state(&run_id, AgentState::Running, Some("Thinking".into()));
            }
            AgentEvent::TextDelta { .. } => {
                if let Some(a) = self.agent_mut(&run_id)
                    && a.state == AgentState::Running
                {
                    a.activity = "Writing".into();
                }
            }
            AgentEvent::AssistantMessage { text, .. } => {
                if let Some(a) = self.agent_mut(&run_id)
                    && !text.trim().is_empty()
                {
                    a.result = text;
                }
            }
            AgentEvent::ToolStarted { summary, .. } => {
                if let Some(a) = self.agent_mut(&run_id) {
                    a.tools += 1;
                    if a.state == AgentState::Running {
                        a.activity = format!("Running {summary}");
                    }
                }
            }
            AgentEvent::ToolFinished { name, .. } => {
                let state = if name == "agent_ask_parent" {
                    // Answered (or timed out).
                    Some(AgentState::Running)
                } else {
                    None
                };
                if let Some(a) = self.agent_mut(&run_id)
                    && a.state.is_live()
                {
                    a.state = state.unwrap_or(a.state);
                    a.activity = "Thinking".into();
                }
            }
            AgentEvent::Approval { .. } => {
                self.set_agent_state(&run_id, AgentState::Running, None);
            }
            AgentEvent::Usage { usage, .. } => {
                if let Some(a) = self.agent_mut(&run_id) {
                    a.tokens += usage.input_tokens
                        + usage.output_tokens
                        + usage.cache_read_tokens
                        + usage.cache_write_tokens;
                }
            }
            AgentEvent::RunFinished { status, .. } => {
                if let Some(a) = self.agent_mut(&run_id) {
                    a.state = match status.as_str() {
                        "completed" => AgentState::Completed,
                        "interrupted" => AgentState::Cancelled,
                        _ => AgentState::Failed,
                    };
                    a.elapsed = Some(a.started.elapsed());
                    a.activity.clear();
                }
            }
            AgentEvent::SubagentMessage { .. }
            | AgentEvent::ToolOutput { .. }
            | AgentEvent::Retrying { .. }
            | AgentEvent::Compacted { .. } => {}
        }
    }

    /// Sub-agents of the current tree that are still working.
    pub fn live_agents(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, Entry::Agent(a) if a.state.is_live()))
            .count()
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .rev()
            .find(|e| matches!(e, Entry::Tool { id: i, .. } if i == id))
    }

    pub fn on_approval(&mut self, pending: PendingApproval) {
        let run = pending.request.run_id.clone();
        self.set_agent_state(&run, AgentState::Approval, None);
        // Sub-agents ask concurrently: answer one at a time, in order.
        if self.approval.is_some() {
            self.approval_queue.push_back(pending);
        } else {
            self.approval = Some(pending);
        }
        // The prompt is drawn in the transcript.
        self.overlay = None;
        self.scroll = 0;
    }

    /// The driven run finished (or the backend failed). May start a queued message.
    pub fn on_run_done(&mut self, result: BackendResult<RunOutcome>) -> Vec<Effect> {
        let run_id = self.running.take().map(|r| r.run_id).unwrap_or_default();
        self.deny_all_approvals();
        for e in &mut self.entries {
            match e {
                Entry::Assistant { streaming, .. } => *streaming = false,
                Entry::Tool { state, .. } if *state == ToolState::Running => {
                    *state = ToolState::Interrupted
                }
                _ => {}
            }
        }
        match result {
            Err(e) => self.error(format!("run failed: {e}")),
            Ok(o) => match o.status {
                RunStatus::Completed => {}
                RunStatus::Interrupted if self.queued.is_some() => {}
                RunStatus::Interrupted => {
                    self.notice(format!("interrupted — /resume {run_id} to continue"))
                }
                RunStatus::Failed | RunStatus::Running => self.error(format!(
                    "run failed: {} — /resume {run_id} to retry",
                    o.error.as_deref().unwrap_or("unknown error")
                )),
            },
        }
        match self.queued.take() {
            Some(text) => self.send(text),
            None => vec![],
        }
    }

    fn notice(&mut self, s: impl Into<String>) {
        self.entries.push(Entry::Notice(s.into()));
    }

    fn error(&mut self, s: impl Into<String>) {
        self.entries.push(Entry::Error(s.into()));
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect()
}

#[cfg(test)]
mod tests;
