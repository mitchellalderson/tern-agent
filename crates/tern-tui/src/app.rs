//! TUI state and its reactions to keys, agent events, and run completion.
//! No terminal or async code here, so it is unit-testable.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tern_core::agent::{RunOutcome, RunStatus};
use tern_core::approval::{ApprovalOutcome, AuthCard, Decision};
use tern_core::events::AgentEvent;
use tern_core::types::Usage;

use crate::history;
use crate::input::Input;
use crate::mcp::{McpAction, McpPanel};
use crate::models::{ModelPicker, PickerAction};
use crate::outbox::OutboxPanel;
use crate::secret_prompt::{Done, SecretPrompt};
use crate::secrets::SecretsPanel;
use crate::setup::{SetupAction, SetupPanel, Task, TaskDone};
use crate::{
    AgentInfo, Backend, BackendResult, ChannelInfo, PendingApproval, PendingAuth, PendingSecret,
    PostLine, SessionInfo, UserPrompt,
};

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
    /// Backend work to run in the background; its result comes back
    /// through [`App::on_task_done`].
    Task(Task),
    /// Open a sign-in page in the user's browser.
    OpenUrl(String),
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
    /// The agent asking for secrets (`secret_request`), drawn in the
    /// transcript; it takes every key while open.
    pub secret_prompt: Option<SecretPrompt>,
    secret_queue: VecDeque<PendingSecret>,
    /// A sign-in card (`auth_propose`, `connection_add`): it takes the
    /// keys while open. `auth_pick` is the highlighted option.
    pub auth_prompt: Option<PendingAuth>,
    pub auth_pick: usize,
    auth_queue: VecDeque<PendingAuth>,
    pub last_usage: Option<Usage>,
    pub hint: Option<String>,
    /// The `/secrets` panel, drawn over the transcript while open.
    pub secrets: Option<SecretsPanel>,
    /// The `/outbox` panel, drawn over the transcript while open.
    pub outbox: Option<OutboxPanel>,
    /// The `/setup` panel, drawn over the transcript while open.
    pub setup: Option<SetupPanel>,
    /// The `/mcp` panel, drawn over the transcript while open.
    pub mcp: Option<McpPanel>,
    /// The `/model` picker, drawn over the transcript while open.
    pub picker: Option<ModelPicker>,
    /// A sub-agent's transcript, drawn over the transcript while open.
    pub overlay: Option<Overlay>,
    /// Sub-agent runs of the current tree. Their events update their
    /// `Entry::Agent` line and never the transcript.
    agents: HashSet<String>,
    /// The run `/agents` and `/agent` look at: the current or last one.
    last_run: Option<String>,
    queued: Option<String>,
    last_ctrl_c: Option<Instant>,
    /// The agent profile the composer talks to (`/agent <name>`); `None` is
    /// the default agent, in sessions of the TUI's own.
    pub profile: Option<AgentInfo>,
    /// The next send to `profile` starts a new thread in its DM (`/new`).
    new_topic: bool,
    /// The topic channel the composer posts to (`/channel <slug>`);
    /// `None` is Home (or `profile`'s DM).
    pub channel: Option<ChannelInfo>,
    /// The newest post of `channel` already shown.
    channel_seen: i64,
    /// Everything waiting on the user (Home's needs-you count), for the
    /// status line; refreshed after runs and channel commands.
    pub needs_you: usize,
}

/// "Research: …" (a card: "▲ needs you", a suggestion: how to decide).
fn post_line(p: &PostLine) -> String {
    let text = p.text.trim();
    match (&p.suggestion, p.kind.as_str()) {
        (Some((_, slug, true)), _) => format!(
            "{}: {text}\n  /channel accept {slug} creates it (and moves this conversation there), /channel decline {slug}",
            p.author
        ),
        (_, "note") => format!("· {text}"),
        _ if p.needs_you => format!("{} (needs you): {text}", p.author),
        _ => format!("{}: {text}", p.author),
    }
}

const HELP: &str = "\
Commands:
  /new              start a new session
  /model [model]    pick a model, or set one (the provider may be left off)
  /setup            add or change model providers and API keys
  /sessions         list recent sessions
  /resume <id>      open a session, or resume an interrupted run
  /tools            list available tools
  /stop             cancel the current run
  /reload           re-read config and rebuild tools
  /secrets [NAME]   manage API keys and tokens in secrets.toml
  /outbox           writes waiting for your approval: approve, edit, reject
  /mcp              add, test and manage MCP servers (/mcp add, /mcp paste,
                    /mcp restart|enable|disable <name>)
  /skills [offers|save N|dismiss N|show|publish|discard NAME]
                    skills, drafts, and runs offered as skills
  /connections      accounts of connectors and MCP servers (tern connections
                    adds, removes and reconnects them)
  /usage            token usage for this session
  /channels         list channels with what needs you
  /channel <slug>   post to a topic channel (/channel home: back to Home)
  /channel new <slug> [title]  make a channel and switch to it
  /channel accept|decline <slug>  answer an agent's channel suggestion
  /channel rename <slug> <new-slug> | title <slug> <title> | delete <slug>
                    rename, retitle or delete a channel (delete asks first)
  /agents           list your agents and the last run's sub-agents
  /agents rename <@name> <name> | slug <@name> <new-name> | delete <@name>
                    rename an agent or delete it (delete asks first)
  /agent <name>     talk to one of your agents in its DM (/agent tern: back)
  /agent <label>    show a sub-agent's transcript (Esc closes)
  /agent <label> <message>  send a running sub-agent a message
  /quit             exit
Keys: Enter send · Shift/Alt+Enter or Ctrl+J newline · Ctrl+O expand tool output
      PgUp/PgDn scroll · Ctrl+C cancel (twice to quit)";

impl App {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let model = backend.default_model();
        let setup = backend
            .setup_needed()
            .then(|| SetupPanel::open(backend.as_ref()).ok())
            .flatten()
            .map(|mut p| {
                p.intro = Some("Welcome to tern. Connect a model provider to get started.".into());
                p
            });
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
            secret_prompt: None,
            secret_queue: VecDeque::new(),
            auth_prompt: None,
            auth_pick: 0,
            auth_queue: VecDeque::new(),
            last_usage: None,
            hint: None,
            secrets: None,
            outbox: None,
            setup,
            mcp: None,
            picker: None,
            overlay: None,
            agents: HashSet::new(),
            last_run: None,
            queued: None,
            last_ctrl_c: None,
            profile: None,
            new_topic: false,
            channel: None,
            channel_seen: 0,
            needs_you: 0,
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
        if let Some(panel) = &mut self.outbox {
            if !panel.on_key(key, self.backend.as_ref()) {
                self.outbox = None;
            }
            return vec![];
        }
        if let Some(panel) = &mut self.setup {
            let action = panel.on_key(key, self.backend.as_ref());
            let changed = std::mem::take(&mut panel.changed);
            if changed {
                self.models_changed();
            }
            return match action {
                SetupAction::None => vec![],
                SetupAction::Close => {
                    self.setup = None;
                    vec![]
                }
                SetupAction::Run(task) => vec![Effect::Task(task)],
            };
        }
        if let Some(panel) = &mut self.mcp {
            return match panel.on_key(key, self.backend.as_ref()) {
                McpAction::None => vec![],
                McpAction::Close => {
                    self.mcp = None;
                    vec![]
                }
                McpAction::Run(task) => vec![Effect::Task(task)],
            };
        }
        if let Some(picker) = &mut self.picker {
            return match picker.on_key(key) {
                PickerAction::None => vec![],
                PickerAction::Close => {
                    self.picker = None;
                    vec![]
                }
                PickerAction::Refresh => vec![Effect::Task(Task::Models {
                    provider: None,
                    refresh: true,
                })],
                PickerAction::Choose { model, default } => {
                    self.use_model(&model, default);
                    vec![]
                }
            };
        }

        if let Some(p) = &mut self.secret_prompt {
            if let Some(done) = p.on_key(key) {
                self.answer_secret(done);
            }
            return vec![];
        }

        if self.auth_prompt.is_some() {
            return self.auth_key(key.code);
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
                // The tainted host limit's choices.
                KeyCode::Char('r' | 'R') if self.egress_prompt() => Decision::AllowRun,
                KeyCode::Char('h' | 'H') if self.egress_prompt() => Decision::AlwaysAllowHost,
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
        } else if let Some(panel) = &mut self.outbox {
            panel.on_paste(text);
        } else if let Some(panel) = &mut self.setup {
            panel.on_paste(text);
        } else if let Some(panel) = &mut self.mcp {
            panel.on_paste(text);
        } else if let Some(picker) = &mut self.picker {
            picker.list.on_paste(text);
        } else if let Some(p) = &mut self.secret_prompt {
            p.on_paste(text);
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
        if self.secrets.take().is_some()
            || self.outbox.take().is_some()
            || self.setup.take().is_some()
            || self.mcp.take().is_some()
            || self.picker.take().is_some()
            || self.overlay.take().is_some()
        {
            return vec![];
        }
        if self.running.is_some() {
            // The whole tree stops, so deny everything it is waiting on.
            self.deny_all_approvals();
            if let Some(run) = self.running.as_ref().map(|r| r.run_id.clone()) {
                self.skip_secrets_of(&run);
            }
        } else if self.secret_prompt.is_some() {
            self.answer_secret(Done::Skip);
        } else if self.auth_prompt.is_some() {
            self.answer_auth("cancel");
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

    /// The prompt asks only because of the tainted host limit.
    fn egress_prompt(&self) -> bool {
        self.approval
            .as_ref()
            .is_some_and(|p| p.request.egress_host.is_some())
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

    /// Keys on a sign-in card. A choice: ↑/↓ or a number, Enter; a sign-in:
    /// Enter or `o` opens the page, `d` switches to a code; Esc cancels.
    fn auth_key(&mut self, code: KeyCode) -> Vec<Effect> {
        let Some(p) = &self.auth_prompt else {
            return vec![];
        };
        match &p.request.card {
            AuthCard::Choose { options, .. } => match code {
                KeyCode::Up => self.auth_pick = self.auth_pick.saturating_sub(1),
                KeyCode::Down => {
                    self.auth_pick = (self.auth_pick + 1).min(options.len().saturating_sub(1))
                }
                KeyCode::Char(c @ '1'..='9') => {
                    let i = c as usize - '1' as usize;
                    if let Some(o) = options.get(i) {
                        let id = o.id.clone();
                        self.answer_auth(&id);
                    }
                }
                KeyCode::Enter => {
                    if let Some(o) = options.get(self.auth_pick) {
                        let id = o.id.clone();
                        self.answer_auth(&id);
                    }
                }
                KeyCode::Esc => self.answer_auth("cancel"),
                _ => {}
            },
            AuthCard::SignIn {
                url,
                device_available,
                ..
            } => match code {
                KeyCode::Enter | KeyCode::Char('o') => {
                    if let Some(u) = url.clone() {
                        self.hint = Some("opened the sign-in page in your browser".into());
                        return vec![Effect::OpenUrl(u)];
                    }
                }
                KeyCode::Char('d') if *device_available => self.answer_auth("device"),
                KeyCode::Esc => self.answer_auth("cancel"),
                _ => {}
            },
        }
        vec![]
    }

    fn answer_auth(&mut self, answer: &str) {
        if let Some(p) = self.auth_prompt.take() {
            let _ = p.reply.send(answer.to_owned());
        }
        self.next_auth();
    }

    fn next_auth(&mut self) {
        self.auth_pick = 0;
        self.auth_prompt = self.auth_queue.pop_front();
        if let Some(p) = &self.auth_prompt
            && let AuthCard::Choose { options, .. } = &p.request.card
        {
            self.auth_pick = options.iter().position(|o| o.recommended).unwrap_or(0);
        }
    }

    /// Close sign-in cards of `call_id` (it finished) or with `id`.
    fn close_auth(&mut self, call_id: Option<&str>, id: Option<&str>) {
        let gone = |p: &PendingAuth| {
            (call_id.is_some() && p.request.call_id.as_deref() == call_id)
                || id.is_some_and(|i| p.request.id == i)
        };
        self.auth_queue.retain(|p| !gone(p));
        if self.auth_prompt.as_ref().is_some_and(gone) {
            self.auth_prompt = None;
            self.next_auth();
        }
    }

    pub fn on_auth(&mut self, pending: PendingAuth) {
        let id = pending.request.id.clone();
        if self
            .auth_prompt
            .as_ref()
            .is_some_and(|p| p.request.id == id)
            || self.auth_queue.iter().any(|p| p.request.id == id)
        {
            return;
        }
        // A later card of the same call (a device code after the browser
        // page) replaces the earlier one.
        if let Some(call) = pending.request.call_id.clone() {
            self.close_auth(Some(&call), None);
        }
        self.auth_queue.push_back(pending);
        if self.auth_prompt.is_none() {
            self.next_auth();
        }
        self.overlay = None;
        self.scroll = 0;
    }

    fn answer_secret(&mut self, done: Done) {
        if let Some(p) = self.secret_prompt.take() {
            p.finish(done);
        }
        self.secret_prompt = self.secret_queue.pop_front().map(SecretPrompt::new);
    }

    /// Skip the prompts of `run` and its sub-agents. Others stay open: an
    /// attached TUI also sees prompts of runs other clients started, which
    /// they may still answer.
    fn skip_secrets_of(&mut self, run: &str) {
        let ours = |r: &str| r == run || self.agents.contains(r);
        let mut keep = VecDeque::new();
        for p in self.secret_queue.drain(..).collect::<Vec<_>>() {
            if ours(&p.request.run_id) {
                SecretPrompt::new(p).finish(Done::Skip);
            } else {
                keep.push_back(p);
            }
        }
        self.secret_queue = keep;
        if self
            .secret_prompt
            .as_ref()
            .is_some_and(|p| ours(&p.pending.request.run_id))
        {
            self.answer_secret(Done::Skip);
        }
    }

    /// Close prompts for `call_id` without answering: it finished, so
    /// someone answered elsewhere or the agent stopped waiting.
    fn close_secret(&mut self, call_id: &str) {
        self.secret_queue
            .retain(|p| p.request.call_id.as_deref() != Some(call_id));
        if self
            .secret_prompt
            .as_ref()
            .is_some_and(|p| p.call_id() == Some(call_id))
        {
            self.secret_prompt = self.secret_queue.pop_front().map(SecretPrompt::new);
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
        let started = match (&self.channel, &self.profile) {
            (Some(c), _) => self
                .backend
                .send_to_channel(&c.id, &text, self.model.as_deref()),
            (None, profile) => match profile {
                Some(p) => {
                    let new_topic = std::mem::take(&mut self.new_topic);
                    let r =
                        self.backend
                            .send_to_agent(&p.id, &text, self.model.as_deref(), new_topic);
                    if r.is_err() {
                        self.new_topic = new_topic;
                    }
                    r
                }
                None => self
                    .backend
                    .start_run(session.as_deref(), self.model.as_deref(), &text),
            },
        };
        match started {
            Ok(started) => {
                self.entries.push(Entry::User(text.trim_end().to_owned()));
                if self
                    .session
                    .as_ref()
                    .is_none_or(|s| s.id != started.session_id)
                {
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
                if let Some(p) = &self.profile {
                    self.new_topic = true;
                    let name = p.name.clone();
                    self.notice(format!("new topic with {name}"));
                } else {
                    self.notice("new session");
                }
            }
            "model" if arg.is_empty() => {
                self.picker = Some(ModelPicker::new(self.model.clone()));
                return vec![Effect::Task(Task::Models {
                    provider: None,
                    refresh: false,
                })];
            }
            "model" => self.use_model(arg, false),
            "setup" | "providers" => match SetupPanel::open(self.backend.as_ref()) {
                Ok(panel) => self.setup = Some(panel),
                Err(e) => self.error(e),
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
                let mut text = format!("tools:\n{}", lines.join("\n"));
                if let Ok(servers) = self.backend.mcp_servers()
                    && !servers.is_empty()
                {
                    let now = chrono::Utc::now().timestamp();
                    text.push_str("\nMCP servers (/mcp to manage):");
                    for s in &servers {
                        text.push_str(&format!(
                            "\n  {} — {}",
                            s.name,
                            crate::mcp::describe_state(s, now)
                        ));
                    }
                }
                self.notice(text);
            }
            "skills" => match self.backend.skills(arg) {
                Ok(text) => self.notice(text),
                Err(e) => self.error(e),
            },
            "connections" => match self.backend.connections() {
                Ok(lines) if lines.is_empty() => self.notice(
                    "No accounts yet: every connector and MCP server uses its default one. \
                     Ask the agent to connect another, or run `tern connections add <name>`."
                        .to_owned(),
                ),
                Ok(lines) => self.notice(format!(
                    "connections:\n{}\n(`tern connections default|rename|remove|reconnect` changes them)",
                    lines.join("\n")
                )),
                Err(e) => self.error(e),
            },
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
            "mcp" => return self.mcp_command(arg),
            "outbox" => match OutboxPanel::open(self.backend.as_ref()) {
                Ok(panel) => self.outbox = Some(panel),
                Err(e) => self.error(e),
            },
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
            "channels" => self.list_channels(),
            "channel" => {
                if self.running.is_some() {
                    return busy(self);
                }
                return self.channel_command(arg);
            }
            "agents" if matches!(arg.split_whitespace().next(), Some("rename" | "slug" | "delete")) => {
                return self.manage_agent(arg);
            }
            "agents" => {
                self.list_profiles();
                self.list_agents();
            }
            "agent" if arg.is_empty() => self.error(
                "usage: /agent <name> to talk to one of your agents; /agent <label> to show a \
                 sub-agent, /agent <label> <message> to message it",
            ),
            "agent" => {
                let (label, message) = match arg.split_once(char::is_whitespace) {
                    Some((l, m)) => (l, m.trim()),
                    None => (arg, ""),
                };
                // A sub-agent of the current tree wins: it's what the user
                // is looking at. Otherwise an agent profile.
                if !self.is_subagent(label)
                    && let Some(p) = self.find_profile(label)
                {
                    if self.running.is_some() {
                        return busy(self);
                    }
                    self.switch_profile(p);
                    if !message.is_empty() {
                        return self.send(message.to_owned());
                    }
                    return vec![];
                }
                self.agent_command(label, message);
            }
            other => self.error(format!("unknown command /{other}; try /help")),
        }
        vec![]
    }

    fn is_subagent(&self, label: &str) -> bool {
        self.last_run.as_ref().is_some_and(|run| {
            self.backend
                .children(run)
                .is_ok_and(|l| l.iter().any(|c| c.label == label))
        })
    }

    fn find_profile(&self, name: &str) -> Option<AgentInfo> {
        let name = name.trim_start_matches('@');
        self.backend
            .agents()
            .ok()?
            .into_iter()
            .find(|a| a.slug == name || a.id == name || a.name.eq_ignore_ascii_case(name))
    }

    /// Talk to `p` from now on: its DM (the default agent: the TUI's own
    /// sessions, as before).
    fn switch_profile(&mut self, p: AgentInfo) {
        self.channel = None;
        self.entries.clear();
        self.session = None;
        self.last_usage = None;
        self.new_topic = false;
        self.model = self.backend.default_model();
        let name = p.name.clone();
        if p.is_default {
            self.profile = None;
            self.notice(format!("talking to {name}"));
        } else {
            self.profile = Some(p);
            self.notice(format!(
                "talking to {name} in its DM; /new starts a new topic there, /agent tern goes back"
            ));
        }
    }

    fn list_channels(&mut self) {
        let list = match self.backend.channels() {
            Ok(l) => l,
            Err(e) => return self.error(e),
        };
        let current = self.channel.as_ref().map(|c| c.id.as_str());
        let lines: Vec<String> = list
            .iter()
            .filter(|c| c.kind == "topic" || c.slug == "home")
            .map(|c| {
                let here = match current {
                    Some(id) if id == c.id => "▸",
                    None if c.slug == "home" => "▸",
                    _ => " ",
                };
                let name = if c.kind == "topic" {
                    format!("#{}", c.slug)
                } else {
                    c.title.clone()
                };
                let mut extra = Vec::new();
                if c.active_runs > 0 {
                    extra.push("working".to_owned());
                }
                if c.needs_you > 0 {
                    extra.push(format!("{} need you", c.needs_you));
                }
                let extra = if extra.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", extra.join(", "))
                };
                format!("{here} {name}{extra}")
            })
            .collect();
        self.needs_you = list
            .iter()
            .find(|c| c.slug == "home")
            .map_or(0, |c| c.needs_you);
        self.notice(format!(
            "channels (switch with /channel <slug>, /channel home to go back; /channel new <slug> [title]):\n{}",
            lines.join("\n")
        ));
    }

    /// `/channel <slug> [message]`, `/channel home`, `/channel new <slug>
    /// [title]`, `/channel accept|decline <slug>`.
    fn channel_command(&mut self, arg: &str) -> Vec<Effect> {
        let (first, rest) = match arg.split_once(char::is_whitespace) {
            Some((f, r)) => (f, r.trim()),
            None => (arg, ""),
        };
        match first {
            "" => {
                match &self.channel {
                    Some(c) => {
                        let slug = c.slug.clone();
                        self.notice(format!("posting to #{slug}; /channel home goes back"));
                    }
                    None => self.notice("in Home; /channel <slug> switches, /channels lists them"),
                }
                vec![]
            }
            "home" => {
                self.channel = None;
                self.entries.clear();
                self.session = None;
                self.notice("back in Home");
                vec![]
            }
            "new" => {
                let (slug, title) = match rest.split_once(char::is_whitespace) {
                    Some((s, t)) => (s, Some(t.trim())),
                    None => (rest, None),
                };
                if slug.is_empty() {
                    self.error("usage: /channel new <slug> [title]");
                    return vec![];
                }
                match self
                    .backend
                    .create_channel(slug.trim_start_matches('#'), title)
                {
                    Ok(c) => self.switch_channel(c),
                    Err(e) => self.error(e),
                }
                vec![]
            }
            "accept" | "decline" => {
                self.decide_suggestion(rest.trim_start_matches('#'), first == "accept");
                vec![]
            }
            "rename" | "title" | "delete" => self.manage_channel(first, rest),
            slug => {
                let slug = slug.trim_start_matches('#');
                let found = match self.backend.channels() {
                    Ok(l) => l.into_iter().find(|c| c.slug == slug || c.id == slug),
                    Err(e) => {
                        self.error(e);
                        return vec![];
                    }
                };
                match found {
                    Some(c) if c.kind == "topic" => {
                        self.switch_channel(c);
                        if !rest.is_empty() {
                            return self.send(rest.to_owned());
                        }
                    }
                    Some(c) if c.slug == "home" => {
                        return self.channel_command("home");
                    }
                    Some(c) => self.error(format!(
                        "{} is an agent's DM: talk to it with /agent",
                        c.title
                    )),
                    None => self.error(format!(
                        "no channel #{slug}; /channels lists them, /channel new {slug} makes it"
                    )),
                }
                vec![]
            }
        }
    }

    /// `/channel rename <slug> <new-slug>`, `/channel title <slug> <title>`,
    /// `/channel delete <slug> [yes]`.
    fn manage_channel(&mut self, verb: &str, rest: &str) -> Vec<Effect> {
        let mut words = rest.split_whitespace();
        let Some(slug) = words.next().map(|s| s.trim_start_matches('#').to_owned()) else {
            self.error(format!(
                "usage: /channel {verb} <slug> {}",
                match verb {
                    "rename" => "<new-slug>",
                    "title" => "<new title>",
                    _ => "[yes]",
                }
            ));
            return vec![];
        };
        let tail: Vec<&str> = words.collect();
        let channel = match self.backend.channels() {
            Ok(l) => l.into_iter().find(|c| c.slug == slug || c.id == slug),
            Err(e) => {
                self.error(e);
                return vec![];
            }
        };
        let Some(c) = channel else {
            self.error(format!("no channel #{slug}; /channels lists them"));
            return vec![];
        };
        if c.kind != "topic" {
            self.error(format!(
                "{} isn't a topic channel: Home and DMs keep their names (a DM follows its agent: /agents rename)",
                c.title
            ));
            return vec![];
        }
        let current = self.channel.as_ref().is_some_and(|cur| cur.id == c.id);
        match verb {
            "rename" | "title" => {
                let value = tail.join(" ");
                if value.is_empty() {
                    self.error(format!("usage: /channel {verb} {slug} <new value>"));
                    return vec![];
                }
                let (new_slug, title) = if verb == "rename" {
                    (Some(value.as_str()), None)
                } else {
                    (None, Some(value.as_str()))
                };
                match self
                    .backend
                    .rename(crate::Managed::Channel, &c.id, new_slug, title)
                {
                    Ok(done) => {
                        self.notice(done);
                        if current && let Ok(l) = self.backend.channels() {
                            self.channel = l.into_iter().find(|x| x.id == c.id);
                        }
                    }
                    Err(e) => self.error(e),
                }
            }
            _ => {
                let confirmed = tail.last().is_some_and(|w| *w == "yes");
                if !confirmed {
                    match self.backend.delete_preview(crate::Managed::Channel, &c.id) {
                        Ok(text) => self.notice(format!(
                            "{text}\nThis can't be undone. Confirm with: /channel delete {slug} yes"
                        )),
                        Err(e) => self.error(e),
                    }
                    return vec![];
                }
                match self.backend.delete(
                    crate::Managed::Channel,
                    &c.id,
                    &crate::DeleteChoice::default(),
                ) {
                    Ok(done) => {
                        if current {
                            self.channel = None;
                            self.entries.clear();
                            self.session = None;
                        }
                        self.notice(done);
                    }
                    Err(e) => self.error(e),
                }
            }
        }
        vec![]
    }

    /// `/agents rename <slug> <name>`, `/agents slug <slug> <new-slug>`,
    /// `/agents delete <slug> [to <agent> | routines] [yes]`.
    fn manage_agent(&mut self, arg: &str) -> Vec<Effect> {
        let mut words = arg.split_whitespace();
        let verb = words.next().unwrap_or_default();
        let Some(target) = words.next() else {
            self.error(format!("usage: /agents {verb} <@name> ..."));
            return vec![];
        };
        let Some(p) = self.find_profile(target) else {
            self.error(format!("no agent {target:?}; /agents lists them"));
            return vec![];
        };
        let tail: Vec<&str> = words.collect();
        match verb {
            "rename" | "slug" => {
                let value = tail.join(" ");
                if value.is_empty() {
                    self.error(format!("usage: /agents {verb} {target} <new value>"));
                    return vec![];
                }
                let (slug, title) = if verb == "slug" {
                    (Some(value.as_str()), None)
                } else {
                    (None, Some(value.as_str()))
                };
                match self
                    .backend
                    .rename(crate::Managed::Agent, &p.id, slug, title)
                {
                    Ok(done) => {
                        self.notice(done);
                        if self.profile.as_ref().is_some_and(|cur| cur.id == p.id) {
                            self.profile = self.find_profile(&p.id);
                        }
                    }
                    Err(e) => self.error(e),
                }
            }
            _ => {
                let confirmed = tail.last().is_some_and(|w| *w == "yes");
                let mut choice = crate::DeleteChoice::default();
                let mut i = 0;
                while i < tail.len() {
                    match tail[i] {
                        "routines" => choice.delete_routines = true,
                        "to" if i + 1 < tail.len() => {
                            choice.reassign_to = Some(tail[i + 1].to_owned());
                            i += 1;
                        }
                        "yes" => {}
                        other => {
                            self.error(format!(
                                "unexpected {other:?}; /agents delete <@name> [to <@agent> | routines] [yes]"
                            ));
                            return vec![];
                        }
                    }
                    i += 1;
                }
                if !confirmed {
                    match self.backend.delete_preview(crate::Managed::Agent, &p.id) {
                        Ok(text) => {
                            let mut cmd = format!("/agents delete {}", p.slug);
                            if let Some(to) = &choice.reassign_to {
                                cmd.push_str(&format!(" to {to}"));
                            }
                            if choice.delete_routines {
                                cmd.push_str(" routines");
                            }
                            self.notice(format!(
                                "{text}\nThis can't be undone. Confirm with: {cmd} yes"
                            ));
                        }
                        Err(e) => self.error(e),
                    }
                    return vec![];
                }
                match self.backend.delete(crate::Managed::Agent, &p.id, &choice) {
                    Ok(done) => {
                        if self.profile.as_ref().is_some_and(|cur| cur.id == p.id) {
                            self.profile = None;
                            self.entries.clear();
                            self.session = None;
                        }
                        self.notice(done);
                    }
                    Err(e) => self.error(e),
                }
            }
        }
        vec![]
    }

    /// Post to `c` from now on, showing its recent posts.
    fn switch_channel(&mut self, c: ChannelInfo) {
        self.profile = None;
        self.entries.clear();
        self.session = None;
        self.last_usage = None;
        self.new_topic = false;
        let slug = c.slug.clone();
        self.channel = Some(c);
        self.channel_seen = 0;
        self.notice(format!(
            "posting to #{slug}; the lead answers, @name asks another agent. /channel home goes back"
        ));
        self.show_new_posts(None);
        self.refresh_needs_you();
    }

    /// Print the channel's posts newer than the last shown, leaving out the
    /// user's own and the answer of `driven` (shown as it streamed).
    fn show_new_posts(&mut self, driven: Option<&str>) {
        let Some(c) = &self.channel else {
            return;
        };
        let id = c.id.clone();
        let posts = match self.backend.channel_posts(&id, 20) {
            Ok(p) => p,
            Err(e) => return self.error(e),
        };
        let fresh: Vec<PostLine> = posts
            .into_iter()
            .filter(|p| p.id > self.channel_seen)
            .collect();
        let first = self.channel_seen == 0;
        if let Some(last) = fresh.last() {
            self.channel_seen = last.id;
        }
        for p in fresh {
            if !first
                && (p.author_kind == "user" || (driven.is_some() && p.run_id.as_deref() == driven))
            {
                continue;
            }
            self.notice(post_line(&p));
        }
    }

    fn refresh_needs_you(&mut self) {
        if let Ok(list) = self.backend.channels() {
            self.needs_you = list
                .iter()
                .find(|c| c.slug == "home")
                .map_or(0, |c| c.needs_you);
            if let Some(cur) = &self.channel
                && let Some(c) = list.into_iter().find(|c| c.id == cur.id)
            {
                self.channel = Some(c);
            }
        }
    }

    /// Accept or decline the pending suggestion of channel `slug` shown in
    /// Home or the current channel.
    fn decide_suggestion(&mut self, slug: &str, accept: bool) {
        let mut places = vec!["home".to_owned()];
        if let Some(c) = &self.channel {
            places.push(c.id.clone());
        }
        let found = places.iter().find_map(|place| {
            self.backend
                .channel_posts(place, 50)
                .ok()?
                .into_iter()
                .find_map(|p| {
                    p.suggestion
                        .filter(|(_, s, pending)| *pending && (slug.is_empty() || s == slug))
                        .map(|(id, s, _)| (id, s))
                })
        });
        let Some((id, s)) = found else {
            return self.error(if slug.is_empty() {
                "no channel suggestion waiting".to_owned()
            } else {
                format!("no suggestion of #{slug} waiting")
            });
        };
        match self.backend.decide_suggestion(id, accept) {
            Ok(how) => {
                self.notice(how);
                if accept
                    && let Ok(list) = self.backend.channels()
                    && let Some(c) = list.into_iter().find(|c| c.slug == s)
                {
                    self.switch_channel(c);
                }
            }
            Err(e) => self.error(e),
        }
        self.refresh_needs_you();
    }

    fn list_profiles(&mut self) {
        let list = match self.backend.agents() {
            Ok(l) if l.len() > 1 => l,
            _ => return,
        };
        let current = self.profile.as_ref().map(|p| p.id.as_str());
        let lines: Vec<String> = list
            .iter()
            .map(|a| {
                let here = if current == Some(a.id.as_str()) || (current.is_none() && a.is_default)
                {
                    "▸"
                } else {
                    " "
                };
                let mut extra = Vec::new();
                if a.active_runs > 0 {
                    extra.push("working".to_owned());
                }
                if a.needs_you > 0 {
                    extra.push(format!("{} need you", a.needs_you));
                }
                let extra = if extra.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", extra.join(", "))
                };
                format!("{here} @{}  {}{extra}", a.slug, a.name)
            })
            .collect();
        self.notice(format!(
            "your agents (talk to one with /agent <name>):\n{}",
            lines.join("\n")
        ));
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
        if let AgentEvent::ToolFinished { id, .. } = &ev {
            self.close_secret(id);
            self.close_auth(Some(id), None);
        }
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

    pub fn on_prompt(&mut self, prompt: UserPrompt) {
        match prompt {
            UserPrompt::Approval(p) => self.on_approval(p),
            UserPrompt::Secret(p) => self.on_secret(p),
            UserPrompt::Auth(p) => self.on_auth(p),
            UserPrompt::AuthClosed(id) => self.close_auth(None, Some(&id)),
        }
    }

    pub fn on_secret(&mut self, pending: PendingSecret) {
        // An attached TUI can get the same request twice: live, and from
        // the pending list it loads on attach.
        let id = &pending.request.id;
        if self
            .secret_prompt
            .as_ref()
            .is_some_and(|p| &p.pending.request.id == id)
            || self.secret_queue.iter().any(|p| &p.request.id == id)
        {
            return;
        }
        if self.secret_prompt.is_some() {
            self.secret_queue.push_back(pending);
        } else {
            self.secret_prompt = Some(SecretPrompt::new(pending));
        }
        // The prompt is drawn in the transcript. An open /secrets panel
        // keeps the keyboard until it closes, so a value being typed there
        // can't spill into the prompt.
        self.overlay = None;
        self.scroll = 0;
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
        self.skip_secrets_of(&run_id);
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
        // What else was said in the channel meanwhile (other agents,
        // routines), and what waits on the user now.
        self.show_new_posts(Some(&run_id));
        self.refresh_needs_you();
        match self.queued.take() {
            Some(text) => self.send(text),
            None => vec![],
        }
    }

    /// Use `model` for the next runs (and this session); with `default`,
    /// also make it the default for new sessions.
    fn use_model(&mut self, model: &str, default: bool) {
        let resolved = match self.backend.resolve_model(model) {
            Ok(r) => r,
            Err(e) => {
                match &mut self.picker {
                    Some(p) => p.message = Some((true, e)),
                    None => self.error(e),
                }
                return;
            }
        };
        if let Some(s) = &self.session
            && let Err(e) = self.backend.set_session_model(&s.id, &resolved.model)
        {
            self.error(e);
            return;
        }
        if default && let Err(e) = self.backend.set_default_model(&resolved.model) {
            self.error(e);
            return;
        }
        self.picker = None;
        self.model = Some(resolved.model.clone());
        let mut msg = if default {
            format!("model set to {} (and the default)", resolved.model)
        } else {
            format!("model set to {}", resolved.model)
        };
        if let Some(w) = resolved.warning {
            msg.push_str(&format!("\n{w}"));
        }
        self.notice(msg);
    }

    /// Providers or the default model changed: pick up the new default
    /// unless this session has chosen its own model.
    fn models_changed(&mut self) {
        let own = self.session.as_ref().and_then(|s| s.model.as_ref());
        if own.is_none() {
            self.model = self.backend.default_model();
        }
    }

    /// `/mcp` and its shortcuts.
    fn mcp_command(&mut self, arg: &str) -> Vec<Effect> {
        let backend = self.backend.clone();
        let (sub, name) = match arg.split_once(char::is_whitespace) {
            Some((s, n)) => (s, n.trim()),
            None => (arg, ""),
        };
        let opened = match sub {
            "" => McpPanel::open(backend.as_ref()),
            "add" => McpPanel::open_add(backend.as_ref()),
            "paste" | "import" => McpPanel::open_paste(backend.as_ref()),
            "restart" | "enable" | "disable" if name.is_empty() => {
                self.error(format!("usage: /mcp {sub} <name>"));
                return vec![];
            }
            "restart" => {
                self.notice(format!("restarting MCP server {name}…"));
                return vec![Effect::Task(Task::McpRestart(name.to_owned()))];
            }
            "enable" | "disable" => {
                let patch = tern_core::mcp::McpPatch {
                    enabled: Some(sub == "enable"),
                    ..Default::default()
                };
                match backend.patch_mcp(name, &patch) {
                    Ok(_) => self.notice(format!("MCP server {name} {sub}d")),
                    Err(e) => self.error(e),
                }
                return vec![];
            }
            other => {
                self.error(format!(
                    "unknown /mcp {other}; try /mcp, /mcp add, /mcp paste, or /mcp restart|enable|disable <name>"
                ));
                return vec![];
            }
        };
        match opened {
            Ok(panel) => self.mcp = Some(panel),
            Err(e) => self.error(e),
        }
        vec![]
    }

    /// The spinner ticked; panels that poll do so here.
    pub fn on_tick(&mut self) {
        if let Some(panel) = &mut self.mcp {
            panel.on_tick(self.backend.as_ref());
        }
    }

    /// A background [`Task`] finished.
    pub fn on_task_done(&mut self, done: TaskDone) -> Vec<Effect> {
        if let Some(panel) = &mut self.mcp {
            if let Some(done) = panel.on_task_done_or_return(done, self.backend.as_ref()) {
                return self.on_task_done_rest(done);
            }
            return vec![];
        }
        self.on_task_done_rest(done)
    }

    fn on_task_done_rest(&mut self, done: TaskDone) -> Vec<Effect> {
        if let TaskDone::McpRestarted(name, r) = done {
            match r {
                Ok(s) => self.notice(format!(
                    "MCP server {name}: {}",
                    crate::mcp::describe_state(&s, chrono::Utc::now().timestamp())
                )),
                Err(e) => self.error(e),
            }
            return vec![];
        }
        if let Some(panel) = &mut self.setup {
            panel.on_task_done(done, self.backend.as_ref());
            if std::mem::take(&mut panel.changed) {
                self.models_changed();
            }
        } else if let Some(picker) = &mut self.picker {
            match done {
                TaskDone::Models(Ok(groups)) => picker.list.set_groups(groups),
                TaskDone::Models(Err(e)) => {
                    picker.list.set_groups(Vec::new());
                    picker.message = Some((true, e));
                }
                _ => {}
            }
        }
        vec![]
    }

    /// Something is loading in a panel (for the spinner).
    pub fn is_busy(&self) -> bool {
        self.setup.as_ref().is_some_and(SetupPanel::is_busy)
            || self.mcp.as_ref().is_some_and(McpPanel::is_busy)
            || self.picker.as_ref().is_some_and(|p| p.list.loading)
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
