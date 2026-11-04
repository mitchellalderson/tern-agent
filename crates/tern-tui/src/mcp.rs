//! The `/mcp` panel (docs/mcp-management-spec.md §8.1): list, add, paste,
//! edit, enable or disable, restart and remove MCP servers. Testing,
//! saving and restarting run as [`Task`]s off the input loop; secret values
//! go to the backend only, never the transcript or input history.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tern_core::config::{McpServerConfig, McpTransport};
use tern_core::mcp::{
    ImportedServer, McpPatch, McpState, McpStatus, McpToolInfo, McpVerified, McpVerifyError,
    check_config, check_name, looks_like_secret,
};

use crate::Backend;
use crate::setup::{Task, TaskDone};

/// How long to keep polling for state changes after a save, toggle or
/// restart (the server connects in the background).
const WATCH: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Transport,
    Name,
    Command,
    Args,
    Env,
    Cwd,
    EnvSecrets,
    Url,
    Bearer,
    Headers,
    Approval,
    Retry,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Transport => "Transport",
            Field::Name => "Name",
            Field::Command => "Command",
            Field::Args => "Arguments",
            Field::Env => "Environment",
            Field::Cwd => "Directory",
            Field::EnvSecrets => "Secrets (env)",
            Field::Url => "URL",
            Field::Bearer => "Bearer token secret",
            Field::Headers => "Header secrets",
            Field::Approval => "Ask before each call",
            Field::Retry => "Safe to retry",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Field::Transport => "Space switches between a local command and a remote URL",
            Field::Name => "a-z, 0-9, _ and -; tools appear as mcp__<name>__<tool>",
            Field::Command => "e.g. npx, uvx, docker, or a path",
            Field::Args => "space separated; quote arguments with spaces",
            Field::Env => "plain settings only: LOG_LEVEL=warn MODE=fast",
            Field::Cwd => "working directory, ~ allowed (optional)",
            Field::EnvSecrets => "names of secrets passed as env vars: GITHUB_TOKEN",
            Field::Url => "the streamable HTTP endpoint, e.g. https://…/mcp",
            Field::Bearer => "secret sent as Authorization: Bearer … (optional)",
            Field::Headers => "header=SECRET pairs: X-API-Key=FOO_KEY (optional)",
            Field::Approval | Field::Retry => "Space toggles",
        }
    }

    pub fn is_toggle(self) -> bool {
        matches!(self, Field::Transport | Field::Approval | Field::Retry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Fields,
    /// One masked field per secret the server needs that isn't set.
    Secrets,
    Testing,
    /// The test failed; the error is kept for display.
    Failed(McpVerifyError),
    /// Review the tools the test listed; unchecked ones are hidden.
    Tools,
    Saving,
}

/// The add/edit form (§5): fields, secrets, test, tools, save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    /// The server being edited, if any.
    pub old: Option<String>,
    pub step: Step,
    pub transport: McpTransport,
    pub name: String,
    pub command: String,
    pub args: String,
    pub env: String,
    pub cwd: String,
    pub env_secrets: String,
    pub url: String,
    pub bearer: String,
    pub headers: String,
    pub approval: bool,
    pub retry: bool,
    pub focus: usize,
    /// `(secret, value typed)` for the Secrets step.
    pub secrets: Vec<(String, String)>,
    pub secret_focus: usize,
    /// Values from a pasted config, carried to the Secrets step.
    prefilled: BTreeMap<String, String>,
    /// Fields the form doesn't edit (`effects`, `enabled`, …).
    base: McpServerConfig,
    pub verified: Option<McpVerified>,
    /// `(tool, shown to the model)` from the test.
    pub tools: Vec<(McpToolInfo, bool)>,
    pub tool_cursor: usize,
}

impl Form {
    fn new() -> Self {
        Self::from_config(
            None,
            "",
            &McpServerConfig {
                enabled: true,
                ..Default::default()
            },
        )
    }

    fn from_config(old: Option<String>, name: &str, cfg: &McpServerConfig) -> Self {
        let pairs = |m: &BTreeMap<String, String>| {
            m.iter()
                .map(|(k, v)| quote(&format!("{k}={v}")))
                .collect::<Vec<_>>()
                .join(" ")
        };
        Self {
            old,
            step: Step::Fields,
            transport: cfg.transport,
            name: name.to_owned(),
            command: cfg.command.clone().unwrap_or_default(),
            args: cfg
                .args
                .iter()
                .map(|a| quote(a))
                .collect::<Vec<_>>()
                .join(" "),
            env: pairs(&cfg.env),
            cwd: cfg.cwd.clone().unwrap_or_default(),
            env_secrets: cfg.env_secrets.join(" "),
            url: cfg.url.clone().unwrap_or_default(),
            bearer: cfg.bearer_token_secret.clone().unwrap_or_default(),
            headers: pairs(&cfg.header_secrets),
            approval: cfg.requires_approval,
            retry: cfg.idempotent,
            // Start on the name for a new server, else the first real field.
            focus: 1,
            secrets: Vec::new(),
            secret_focus: 0,
            prefilled: BTreeMap::new(),
            base: cfg.clone(),
            verified: None,
            tools: Vec::new(),
            tool_cursor: 0,
        }
    }

    fn from_import(s: ImportedServer) -> Self {
        let mut f = Self::from_config(None, &s.name, &s.config);
        f.prefilled = s
            .secrets
            .into_iter()
            .filter_map(|(k, v)| Some((k, v?)))
            .collect();
        f
    }

    pub fn fields(&self) -> Vec<Field> {
        let mut f = vec![Field::Transport, Field::Name];
        match self.transport {
            McpTransport::Stdio => f.extend([
                Field::Command,
                Field::Args,
                Field::Env,
                Field::Cwd,
                Field::EnvSecrets,
            ]),
            McpTransport::Http => f.extend([Field::Url, Field::Bearer, Field::Headers]),
        }
        f.extend([Field::Approval, Field::Retry]);
        f
    }

    pub fn value(&self, field: Field) -> String {
        match field {
            Field::Transport => match self.transport {
                McpTransport::Stdio => "local command (stdio)".into(),
                McpTransport::Http => "remote URL (http)".into(),
            },
            Field::Name => self.name.clone(),
            Field::Command => self.command.clone(),
            Field::Args => self.args.clone(),
            Field::Env => self.env.clone(),
            Field::Cwd => self.cwd.clone(),
            Field::EnvSecrets => self.env_secrets.clone(),
            Field::Url => self.url.clone(),
            Field::Bearer => self.bearer.clone(),
            Field::Headers => self.headers.clone(),
            Field::Approval => yes_no(self.approval),
            Field::Retry => yes_no(self.retry),
        }
    }

    fn text_mut(&mut self, field: Field) -> Option<&mut String> {
        Some(match field {
            Field::Name => &mut self.name,
            Field::Command => &mut self.command,
            Field::Args => &mut self.args,
            Field::Env => &mut self.env,
            Field::Cwd => &mut self.cwd,
            Field::EnvSecrets => &mut self.env_secrets,
            Field::Url => &mut self.url,
            Field::Bearer => &mut self.bearer,
            Field::Headers => &mut self.headers,
            Field::Transport | Field::Approval | Field::Retry => return None,
        })
    }

    pub fn focused(&self) -> Field {
        let fields = self.fields();
        fields[self.focus.min(fields.len() - 1)]
    }

    fn toggle(&mut self, field: Field) {
        match field {
            Field::Transport => {
                self.transport = match self.transport {
                    McpTransport::Stdio => McpTransport::Http,
                    McpTransport::Http => McpTransport::Stdio,
                };
                self.focus = self.focus.min(self.fields().len() - 1);
            }
            Field::Approval => self.approval = !self.approval,
            Field::Retry => self.retry = !self.retry,
            _ => {}
        }
    }

    /// Move plain env values that look like keys into secrets, so they
    /// never reach `config.toml`. Returns the names moved.
    fn move_secret_env(&mut self) -> Vec<String> {
        let Ok(pairs) = split_pairs(&self.env, "environment") else {
            return Vec::new();
        };
        let mut moved = Vec::new();
        let mut keep = Vec::new();
        for (k, v) in pairs {
            if looks_like_secret(&k, &v) {
                self.prefilled.insert(k.clone(), v);
                moved.push(k);
            } else {
                keep.push(quote(&format!("{k}={v}")));
            }
        }
        if !moved.is_empty() {
            self.env = keep.join(" ");
            let mut names = split_names(&self.env_secrets);
            for k in &moved {
                if !names.contains(k) {
                    names.push(k.clone());
                }
            }
            self.env_secrets = names.join(" ");
        }
        moved
    }

    /// The config as typed, checked like the API checks it.
    pub fn config(&self) -> Result<McpServerConfig, String> {
        check_name(self.name.trim())?;
        let mut cfg = self.base.clone();
        cfg.transport = self.transport;
        cfg.requires_approval = self.approval;
        cfg.idempotent = self.retry;
        let some = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_owned());
        match self.transport {
            McpTransport::Stdio => {
                cfg.command = some(&self.command);
                cfg.args = split_args(&self.args)?;
                cfg.env = split_pairs(&self.env, "environment")?.into_iter().collect();
                cfg.cwd = some(&self.cwd);
                cfg.env_secrets = split_names(&self.env_secrets);
                (cfg.url, cfg.bearer_token_secret) = (None, None);
                cfg.header_secrets.clear();
            }
            McpTransport::Http => {
                cfg.url = some(&self.url);
                cfg.bearer_token_secret = some(&self.bearer);
                cfg.header_secrets = split_pairs(&self.headers, "headers")?.into_iter().collect();
                (cfg.command, cfg.cwd) = (None, None);
                cfg.args.clear();
                cfg.env.clear();
                cfg.env_secrets.clear();
            }
        }
        check_config(&cfg)?;
        Ok(cfg)
    }

    fn secret_values(&self) -> BTreeMap<String, String> {
        let mut out = self.prefilled.clone();
        for (k, v) in &self.secrets {
            if !v.trim().is_empty() {
                out.insert(k.clone(), v.trim().to_owned());
            }
        }
        out
    }
}

fn yes_no(b: bool) -> String {
    if b { "yes" } else { "no" }.into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    List,
    Detail {
        name: String,
        tool: usize,
        logs: bool,
    },
    Form(Box<Form>),
    /// Pasting a `mcpServers` JSON snippet.
    Paste(String),
    ConfirmRemove {
        name: String,
    },
}

/// What the app should do after a key.
#[derive(Debug, PartialEq, Eq)]
pub enum McpAction {
    None,
    Close,
    Run(Task),
}

pub struct McpPanel {
    pub items: Vec<McpStatus>,
    pub selected: usize,
    pub mode: Mode,
    /// Feedback for the last action: `(is_error, text)`.
    pub message: Option<(bool, String)>,
    /// Further drafts from one paste, reviewed after the current one.
    queue: Vec<ImportedServer>,
    /// Poll for state changes until then.
    watch_until: Option<Instant>,
    last_poll: Option<Instant>,
    /// Secret names that are set (file or env), for the Secrets step.
    set_secrets: Vec<String>,
}

impl McpPanel {
    pub fn open(backend: &dyn Backend) -> Result<Self, String> {
        Ok(Self {
            items: backend.mcp_servers()?,
            selected: 0,
            mode: Mode::List,
            message: None,
            queue: Vec::new(),
            watch_until: None,
            last_poll: None,
            set_secrets: Vec::new(),
        })
    }

    /// Open straight into the add form.
    pub fn open_add(backend: &dyn Backend) -> Result<Self, String> {
        let mut p = Self::open(backend)?;
        p.mode = Mode::Form(Box::new(Form::new()));
        Ok(p)
    }

    /// Open straight into paste mode.
    pub fn open_paste(backend: &dyn Backend) -> Result<Self, String> {
        let mut p = Self::open(backend)?;
        p.mode = Mode::Paste(String::new());
        Ok(p)
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.items.get(self.selected).map(|s| s.name.as_str())
    }

    fn select(&mut self, name: &str) {
        if let Some(i) = self.items.iter().position(|s| s.name == name) {
            self.selected = i;
        }
    }

    pub fn status(&self, name: &str) -> Option<&McpStatus> {
        self.items.iter().find(|s| s.name == name)
    }

    fn refresh(&mut self, backend: &dyn Backend) {
        let keep = self.selected_name().map(str::to_owned);
        match backend.mcp_servers() {
            Ok(items) => {
                self.items = items;
                self.selected = self.selected.min(self.items.len().saturating_sub(1));
                if let Some(k) = keep {
                    self.select(&k);
                }
            }
            Err(e) => self.message = Some((true, e)),
        }
        self.last_poll = Some(Instant::now());
    }

    fn watch(&mut self) {
        self.watch_until = Some(Instant::now() + WATCH);
    }

    /// Waiting on a test, a save, or a server that's still connecting.
    pub fn is_busy(&self) -> bool {
        matches!(
            &self.mode,
            Mode::Form(f) if matches!(f.step, Step::Testing | Step::Saving)
        ) || self.watch_until.is_some_and(|t| Instant::now() < t)
    }

    /// Called on the spinner tick: refresh while watching.
    pub fn on_tick(&mut self, backend: &dyn Backend) {
        let Some(until) = self.watch_until else {
            return;
        };
        let now = Instant::now();
        if now >= until {
            self.watch_until = None;
            return;
        }
        if self.last_poll.is_some_and(|t| now.duration_since(t) < POLL) {
            return;
        }
        self.refresh(backend);
        // Stop once nothing is on its way to a settled state.
        if self.items.iter().all(|s| {
            !matches!(s.state, McpState::Connecting | McpState::NotStarted) || !s.config.enabled
        }) {
            self.watch_until = None;
        }
    }

    /// Handle a key.
    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> McpAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mode = std::mem::replace(&mut self.mode, Mode::List);
        match mode {
            Mode::List => self.on_list_key(key, backend),
            Mode::Detail { name, tool, logs } => self.on_detail_key(key, backend, name, tool, logs),
            Mode::Form(form) => self.on_form_key(key, ctrl, backend, *form),
            Mode::Paste(mut text) => match key.code {
                KeyCode::Esc => McpAction::None,
                KeyCode::Enter if !text.trim().is_empty() => {
                    self.import(&text, backend);
                    McpAction::None
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.mode = Mode::Paste(text);
                    McpAction::None
                }
                KeyCode::Char('u') if ctrl => {
                    self.mode = Mode::Paste(String::new());
                    McpAction::None
                }
                KeyCode::Char(c) if !ctrl => {
                    text.push(c);
                    self.mode = Mode::Paste(text);
                    McpAction::None
                }
                _ => {
                    self.mode = Mode::Paste(text);
                    McpAction::None
                }
            },
            Mode::ConfirmRemove { name } => {
                let remove_secrets = match key.code {
                    KeyCode::Char('y') => false,
                    KeyCode::Char('x') => true,
                    _ => return McpAction::None,
                };
                match backend.remove_mcp(&name, remove_secrets) {
                    Ok(()) => {
                        self.refresh(backend);
                        self.message = Some((
                            false,
                            if remove_secrets {
                                format!("removed {name} and the secrets nothing else uses")
                            } else {
                                format!("removed {name}")
                            },
                        ));
                    }
                    Err(e) => self.message = Some((true, e)),
                }
                McpAction::None
            }
        }
    }

    fn on_list_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> McpAction {
        self.message = None;
        let current = self.items.get(self.selected).cloned();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return McpAction::Close,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1));
            }
            KeyCode::Char('a') => self.start_form(Form::new(), backend),
            KeyCode::Char('p') => self.mode = Mode::Paste(String::new()),
            KeyCode::Enter => {
                if let Some(s) = current {
                    self.mode = Mode::Detail {
                        name: s.name,
                        tool: 0,
                        logs: false,
                    };
                }
            }
            KeyCode::Char('e') => {
                if let Some(s) = current {
                    self.edit(&s, backend);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(s) = current {
                    self.set_enabled(&s.name, !s.config.enabled, backend);
                }
            }
            KeyCode::Char('r') => {
                if let Some(s) = current {
                    return self.restart(&s);
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(s) = current {
                    self.mode = Mode::ConfirmRemove { name: s.name };
                }
            }
            _ => {}
        }
        McpAction::None
    }

    fn on_detail_key(
        &mut self,
        key: KeyEvent,
        backend: &dyn Backend,
        name: String,
        mut tool: usize,
        mut logs: bool,
    ) -> McpAction {
        self.message = None;
        let Some(s) = self.status(&name).cloned() else {
            return McpAction::None;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return McpAction::None,
            KeyCode::Up | KeyCode::Char('k') => tool = tool.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                tool = (tool + 1).min(s.tools.len().saturating_sub(1));
            }
            KeyCode::Char('l') => logs = !logs,
            KeyCode::Char(' ') => {
                if let Some(t) = s.tools.get(tool) {
                    let mut hidden = s.config.disabled_tools.clone();
                    if t.disabled {
                        hidden.retain(|x| *x != t.remote);
                    } else {
                        hidden.push(t.remote.clone());
                    }
                    let patch = McpPatch {
                        disabled_tools: Some(hidden),
                        ..Default::default()
                    };
                    self.patch(&name, &patch, backend);
                }
            }
            KeyCode::Char('e') => {
                self.edit(&s, backend);
                return McpAction::None;
            }
            KeyCode::Char('r') => {
                self.mode = Mode::Detail { name, tool, logs };
                return self.restart(&s);
            }
            KeyCode::Char('d') => {
                self.mode = Mode::ConfirmRemove { name };
                return McpAction::None;
            }
            _ => {}
        }
        self.mode = Mode::Detail { name, tool, logs };
        McpAction::None
    }

    fn on_form_key(
        &mut self,
        key: KeyEvent,
        ctrl: bool,
        backend: &dyn Backend,
        mut form: Form,
    ) -> McpAction {
        match form.step.clone() {
            Step::Fields => {
                let fields = form.fields();
                let field = form.focused();
                match key.code {
                    KeyCode::Esc => return self.cancel_form(),
                    KeyCode::Up | KeyCode::BackTab => form.focus = form.focus.saturating_sub(1),
                    KeyCode::Down | KeyCode::Tab => {
                        form.focus = (form.focus + 1).min(fields.len() - 1);
                    }
                    KeyCode::Char(' ') if field.is_toggle() => form.toggle(field),
                    KeyCode::Left | KeyCode::Right if field.is_toggle() => form.toggle(field),
                    KeyCode::Enter => {
                        let moved = form.move_secret_env();
                        match form.config() {
                            Ok(cfg) => {
                                self.message = (!moved.is_empty()).then(|| {
                                    (false, format!("moved {} to secrets", moved.join(", ")))
                                });
                                return self.after_fields(form, cfg, backend);
                            }
                            Err(e) => self.message = Some((true, e)),
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(t) = form.text_mut(field) {
                            t.pop();
                        }
                    }
                    KeyCode::Char('u') if ctrl => {
                        if let Some(t) = form.text_mut(field) {
                            t.clear();
                        }
                    }
                    KeyCode::Char(c) if !ctrl => {
                        self.message = None;
                        if let Some(t) = form.text_mut(field) {
                            t.push(c);
                        }
                    }
                    _ => {}
                }
            }
            Step::Secrets => match key.code {
                KeyCode::Esc => form.step = Step::Fields,
                KeyCode::Up | KeyCode::BackTab => {
                    form.secret_focus = form.secret_focus.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Tab => {
                    form.secret_focus =
                        (form.secret_focus + 1).min(form.secrets.len().saturating_sub(1));
                }
                KeyCode::Enter => return self.test(form),
                KeyCode::Backspace => {
                    if let Some((_, v)) = form.secrets.get_mut(form.secret_focus) {
                        v.pop();
                    }
                }
                KeyCode::Char('u') if ctrl => {
                    if let Some((_, v)) = form.secrets.get_mut(form.secret_focus) {
                        v.clear();
                    }
                }
                KeyCode::Char(c) if !ctrl => {
                    if let Some((_, v)) = form.secrets.get_mut(form.secret_focus) {
                        v.push(c);
                    }
                }
                _ => {}
            },
            Step::Testing | Step::Saving => {}
            Step::Failed(_) => match key.code {
                KeyCode::Esc => return self.cancel_form(),
                KeyCode::Char('e') => form.step = Step::Fields,
                KeyCode::Char('t') | KeyCode::Enter => return self.test(form),
                KeyCode::Char('s') => return self.save(form),
                _ => {}
            },
            Step::Tools => match key.code {
                KeyCode::Esc => form.step = Step::Fields,
                KeyCode::Up | KeyCode::Char('k') => {
                    form.tool_cursor = form.tool_cursor.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    form.tool_cursor =
                        (form.tool_cursor + 1).min(form.tools.len().saturating_sub(1));
                }
                KeyCode::Char(' ') => {
                    if let Some((_, on)) = form.tools.get_mut(form.tool_cursor) {
                        *on = !*on;
                    }
                }
                KeyCode::Enter | KeyCode::Char('s') => return self.save(form),
                _ => {}
            },
        }
        self.mode = Mode::Form(Box::new(form));
        McpAction::None
    }

    /// Fields are valid: ask for missing secrets, or go straight to testing.
    fn after_fields(
        &mut self,
        mut form: Form,
        cfg: McpServerConfig,
        backend: &dyn Backend,
    ) -> McpAction {
        self.load_set_secrets(backend);
        let typed: BTreeMap<String, String> = form.secrets.drain(..).collect();
        form.secrets = cfg
            .secret_names()
            .filter(|n| !self.set_secrets.iter().any(|s| s == n) || form.prefilled.contains_key(*n))
            .map(|n| {
                let v = typed
                    .get(n)
                    .or_else(|| form.prefilled.get(n))
                    .cloned()
                    .unwrap_or_default();
                (n.to_owned(), v)
            })
            .collect();
        form.secrets.dedup_by(|a, b| a.0 == b.0);
        // Prefilled values move into the fields, where they can be changed.
        form.prefilled
            .retain(|k, _| !form.secrets.iter().any(|(n, _)| n == k));
        form.secret_focus = 0;
        if form.secrets.is_empty() {
            self.test(form)
        } else {
            form.step = Step::Secrets;
            self.mode = Mode::Form(Box::new(form));
            McpAction::None
        }
    }

    fn load_set_secrets(&mut self, backend: &dyn Backend) {
        if let Ok(all) = backend.secrets() {
            self.set_secrets = all
                .into_iter()
                .filter(|s| s.in_env || s.in_file)
                .map(|s| s.name)
                .collect();
        }
    }

    fn test(&mut self, mut form: Form) -> McpAction {
        let cfg = match form.config() {
            Ok(c) => c,
            Err(e) => {
                self.message = Some((true, e));
                form.step = Step::Fields;
                self.mode = Mode::Form(Box::new(form));
                return McpAction::None;
            }
        };
        let task = Task::McpVerify {
            name: form.name.trim().to_owned(),
            cfg: Box::new(cfg),
            secrets: form.secret_values(),
        };
        form.step = Step::Testing;
        self.message = None;
        self.mode = Mode::Form(Box::new(form));
        McpAction::Run(task)
    }

    fn save(&mut self, mut form: Form) -> McpAction {
        let mut cfg = match form.config() {
            Ok(c) => c,
            Err(e) => {
                self.message = Some((true, e));
                form.step = Step::Fields;
                self.mode = Mode::Form(Box::new(form));
                return McpAction::None;
            }
        };
        if !form.tools.is_empty() {
            // Keep hidden names the server no longer lists.
            let listed: Vec<&str> = form.tools.iter().map(|(t, _)| t.remote.as_str()).collect();
            cfg.disabled_tools.retain(|t| !listed.contains(&t.as_str()));
            cfg.disabled_tools.extend(
                form.tools
                    .iter()
                    .filter(|(_, on)| !on)
                    .map(|(t, _)| t.remote.clone()),
            );
        }
        let task = Task::McpSave {
            old: form.old.clone(),
            name: form.name.trim().to_owned(),
            cfg: Box::new(cfg),
            secrets: form.secret_values(),
        };
        form.step = Step::Saving;
        self.mode = Mode::Form(Box::new(form));
        McpAction::Run(task)
    }

    fn cancel_form(&mut self) -> McpAction {
        // A cancelled draft from a paste moves on to the next one.
        if !self.queue.is_empty() {
            let next = self.queue.remove(0);
            self.mode = Mode::Form(Box::new(Form::from_import(next)));
        }
        McpAction::None
    }

    fn start_form(&mut self, form: Form, backend: &dyn Backend) {
        self.load_set_secrets(backend);
        self.mode = Mode::Form(Box::new(form));
    }

    fn edit(&mut self, s: &McpStatus, backend: &dyn Backend) {
        let mut form = Form::from_config(Some(s.name.clone()), &s.name, &s.config);
        form.tools = s.tools.iter().map(|t| (t.clone(), !t.disabled)).collect();
        self.start_form(form, backend);
    }

    fn import(&mut self, text: &str, backend: &dyn Backend) {
        match backend.import_mcp(text) {
            Ok(mut drafts) if !drafts.is_empty() => {
                let first = drafts.remove(0);
                let warnings = first.warnings.join("; ");
                self.message = (!warnings.is_empty()).then_some((false, warnings));
                self.queue = drafts;
                self.start_form(Form::from_import(first), backend);
            }
            Ok(_) => {
                self.message = Some((true, "no MCP servers in that text".into()));
                self.mode = Mode::Paste(text.to_owned());
            }
            Err(e) => {
                self.message = Some((true, e));
                self.mode = Mode::Paste(text.to_owned());
            }
        }
    }

    fn set_enabled(&mut self, name: &str, enabled: bool, backend: &dyn Backend) {
        let patch = McpPatch {
            enabled: Some(enabled),
            ..Default::default()
        };
        if self.patch(name, &patch, backend) {
            let what = if enabled { "enabled" } else { "disabled" };
            self.message = Some((false, format!("{what} {name}")));
        }
    }

    fn patch(&mut self, name: &str, patch: &McpPatch, backend: &dyn Backend) -> bool {
        match backend.patch_mcp(name, patch) {
            Ok(_) => {
                self.refresh(backend);
                self.watch();
                true
            }
            Err(e) => {
                self.message = Some((true, e));
                false
            }
        }
    }

    fn restart(&mut self, s: &McpStatus) -> McpAction {
        if !s.config.enabled {
            self.message = Some((true, format!("{} is disabled; Space enables it", s.name)));
            return McpAction::None;
        }
        self.message = Some((false, format!("restarting {}…", s.name)));
        self.watch();
        McpAction::Run(Task::McpRestart(s.name.clone()))
    }

    /// A background task finished. Hands it back if it wasn't for this panel.
    pub fn on_task_done_or_return(
        &mut self,
        done: TaskDone,
        backend: &dyn Backend,
    ) -> Option<TaskDone> {
        match done {
            TaskDone::McpVerified(r) => {
                let Mode::Form(form) = &mut self.mode else {
                    return None;
                };
                match r {
                    Ok(Ok(v)) => {
                        // Keep the user's choices from an earlier test or edit.
                        let hidden: Vec<String> = form
                            .tools
                            .iter()
                            .filter(|(_, on)| !on)
                            .map(|(t, _)| t.remote.clone())
                            .chain(form.base.disabled_tools.iter().cloned())
                            .collect();
                        form.tools = v
                            .tools
                            .iter()
                            .map(|t| (t.clone(), !hidden.contains(&t.remote)))
                            .collect();
                        form.tool_cursor = 0;
                        form.verified = Some(v);
                        form.step = Step::Tools;
                    }
                    Ok(Err(e)) => form.step = Step::Failed(e),
                    Err(e) => {
                        form.step = Step::Fields;
                        self.message = Some((true, e));
                    }
                }
            }
            TaskDone::McpSaved(r) => {
                let renamed = match &self.mode {
                    Mode::Form(f) => f.old.as_ref().filter(|o| **o != f.name.trim()).cloned(),
                    _ => None,
                };
                match r {
                    Ok(s) => {
                        self.refresh(backend);
                        self.select(&s.name);
                        self.watch();
                        let mut msg = format!("saved {}; starting it", s.name);
                        if let Some(old) = renamed {
                            msg.push_str(&format!(
                                ". Allowlist entries for mcp__{old}__* don't carry over to the new name"
                            ));
                        }
                        self.message = Some((false, msg));
                        self.mode = Mode::List;
                        self.cancel_form();
                    }
                    Err(e) => {
                        if let Mode::Form(f) = &mut self.mode {
                            f.step = Step::Fields;
                        }
                        self.message = Some((true, e));
                    }
                }
            }
            TaskDone::McpRestarted(name, r) => {
                self.refresh(backend);
                self.message = Some(match r {
                    Ok(s) => match s.state {
                        McpState::Running { tools, .. } => {
                            (false, format!("{name}: running, {tools} tools"))
                        }
                        McpState::Unavailable { error, .. } => (true, format!("{name}: {error}")),
                        other => (false, format!("{name}: {other:?}")),
                    },
                    Err(e) => (true, e),
                });
            }
            other => return Some(other),
        }
        None
    }

    /// Pasted text goes into the field being edited, never the chat input.
    pub fn on_paste(&mut self, text: &str) {
        match &mut self.mode {
            Mode::Paste(buf) => buf.push_str(&text.replace("\r\n", "\n").replace('\r', "\n")),
            Mode::Form(form) => {
                let line = text.trim_end_matches(['\r', '\n']);
                match form.step {
                    Step::Fields => {
                        let field = form.focused();
                        if let Some(t) = form.text_mut(field) {
                            t.push_str(&line.replace(['\r', '\n'], " "));
                        }
                    }
                    Step::Secrets => {
                        if let Some((_, v)) = form.secrets.get_mut(form.secret_focus) {
                            v.push_str(line.trim());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// The one-line state, as the list shows it.
pub fn describe_state(s: &McpStatus, now: i64) -> String {
    match &s.state {
        McpState::Disabled => "disabled".into(),
        McpState::NotStarted => "not started".into(),
        McpState::Connecting => "connecting…".into(),
        McpState::Running { tools, .. } => {
            format!(
                "running · {tools} tool{}",
                if *tools == 1 { "" } else { "s" }
            )
        }
        McpState::Unavailable {
            error, retry_at, ..
        } => {
            let retry = match retry_at {
                Some(t) if *t > now => format!(" · retry in {}s", t - now),
                _ => String::new(),
            };
            format!("unavailable{retry} · {error}")
        }
    }
}

/// Split shell style: spaces separate, quotes group, `\` escapes.
pub fn split_args(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut any = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"') | None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                any = true;
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return Err("an argument has an unclosed quote".into());
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// The reverse of [`split_args`] for one argument.
pub fn quote(s: &str) -> String {
    if !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || "'\"\\".contains(c)) {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

fn split_pairs(s: &str, what: &str) -> Result<Vec<(String, String)>, String> {
    split_args(s)?
        .into_iter()
        .map(|p| match p.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_owned(), v.to_owned())),
            _ => Err(format!("{what}: expected NAME=value, got {p:?}")),
        })
        .collect()
}

fn split_names(s: &str) -> Vec<String> {
    let mut out: Vec<String> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .collect();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_split_and_quote() {
        assert_eq!(
            split_args(r#"run -i "a b" 'c d' e\ f"#).unwrap(),
            ["run", "-i", "a b", "c d", "e f"]
        );
        assert_eq!(split_args("  ").unwrap(), Vec::<String>::new());
        assert_eq!(split_args("''").unwrap(), [""]);
        assert!(split_args("'open").is_err());
        for a in ["plain", "with space", "it's", ""] {
            assert_eq!(split_args(&quote(a)).unwrap(), [a]);
        }
    }

    #[test]
    fn form_round_trips_a_config() {
        let mut cfg = McpServerConfig {
            enabled: true,
            command: Some("npx".into()),
            args: vec!["-y".into(), "a server".into()],
            env_secrets: vec!["TOKEN".into()],
            requires_approval: true,
            ..Default::default()
        };
        cfg.env.insert("LOG_LEVEL".into(), "warn".into());
        let f = Form::from_config(Some("x".into()), "x", &cfg);
        assert_eq!(f.config().unwrap(), cfg);

        let mut f = Form::new();
        f.name = "Bad Name".into();
        assert!(f.config().is_err());
    }

    #[test]
    fn secret_looking_env_moves_to_secrets() {
        let mut f = Form::new();
        f.name = "gh".into();
        f.command = "github-mcp-server".into();
        f.env = "LOG_LEVEL=warn GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz".into();
        assert!(f.config().is_err());
        assert_eq!(f.move_secret_env(), ["GITHUB_TOKEN"]);
        let cfg = f.config().unwrap();
        assert_eq!(cfg.env_secrets, ["GITHUB_TOKEN"]);
        assert_eq!(cfg.env.len(), 1);
        assert_eq!(
            f.secret_values()["GITHUB_TOKEN"],
            "ghp_abcdefghijklmnopqrstuvwxyz"
        );
    }
}
