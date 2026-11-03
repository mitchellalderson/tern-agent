//! TUI state and its reactions to keys, agent events, and run completion.
//! No terminal or async code here, so it is unit-testable.

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
    pub last_usage: Option<Usage>,
    pub hint: Option<String>,
    /// The `/secrets` panel, drawn over the transcript while open.
    pub secrets: Option<SecretsPanel>,
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
            last_usage: None,
            hint: None,
            secrets: None,
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
        if self.secrets.take().is_some() {
            return vec![];
        }
        if self.approval.is_some() {
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
            other => self.error(format!("unknown command /{other}; try /help")),
        }
        vec![]
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
            AgentEvent::RunStarted { .. }
            | AgentEvent::RunFinished { .. }
            | AgentEvent::ToolOutput { .. }
            | AgentEvent::SubagentSpawned { .. } => {}
        }
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .rev()
            .find(|e| matches!(e, Entry::Tool { id: i, .. } if i == id))
    }

    pub fn on_approval(&mut self, pending: PendingApproval) {
        // A second request can't arrive while one is pending (tools run one
        // at a time); deny defensively rather than overwrite.
        if let Some(old) = self.approval.replace(pending) {
            let _ = old.reply.send(Decision::Deny);
        }
        self.scroll = 0;
    }

    /// The driven run finished (or the backend failed). May start a queued message.
    pub fn on_run_done(&mut self, result: BackendResult<RunOutcome>) -> Vec<Effect> {
        let run_id = self.running.take().map(|r| r.run_id).unwrap_or_default();
        if let Some(p) = self.approval.take() {
            let _ = p.reply.send(Decision::Deny);
        }
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

#[cfg(test)]
mod tests;
