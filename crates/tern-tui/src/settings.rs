//! The `/settings` screen: every platform-wide setting in one place, in the
//! same sections as the desktop Settings window (General, Models &
//! Providers, Secrets, Permissions & Sandbox, Limits & Budgets, Servers,
//! Advanced, About). `config.toml` settings are described by the core
//! (`GET /v1/config/settings`), so this lists and edits whatever it says;
//! providers, secrets and MCP servers keep their own panels, which a row
//! here opens. Agent settings are not here: they stay in `/agents`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use tern_core::config::settings::{ConfigView, Kind, Setting};

use crate::Backend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    General,
    Models,
    Secrets,
    Permissions,
    Limits,
    Servers,
    Advanced,
    About,
}

impl Section {
    pub const ALL: [Section; 8] = [
        Section::General,
        Section::Models,
        Section::Secrets,
        Section::Permissions,
        Section::Limits,
        Section::Servers,
        Section::Advanced,
        Section::About,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Models => "Models & Providers",
            Section::Secrets => "Secrets",
            Section::Permissions => "Permissions & Sandbox",
            Section::Limits => "Limits & Budgets",
            Section::Servers => "Servers",
            Section::Advanced => "Advanced",
            Section::About => "About",
        }
    }

    pub fn intro(self) -> &'static str {
        match self {
            Section::General => {
                "How tern behaves day to day: when it speaks up, what it remembers."
            }
            Section::Models => "Providers, API keys, and the model new conversations start with.",
            Section::Secrets => "API keys and tokens for providers, connectors and MCP servers.",
            Section::Permissions => {
                "What tern may do without asking, and how shell commands are confined."
            }
            Section::Limits => "Caps on how long and how much tern works before it stops.",
            Section::Servers => "How other devices and webhooks reach tern; MCP servers.",
            Section::Advanced => "Tuning knobs most people never touch.",
            Section::About => "Versions and where tern keeps things.",
        }
    }

    /// What searching finds in a section besides its settings.
    fn keywords(self) -> &'static str {
        match self {
            Section::General => "workspace quiet hours initiative digest memory skills",
            Section::Models => "provider api key default model fallback chain",
            Section::Secrets => "api key token password secrets.toml",
            Section::Permissions => "approvals autonomy sandbox network shell taint scripts",
            Section::Limits => "budget spend cap usd tokens timeout iterations",
            Section::Servers => "mcp listen api webhooks devices tls",
            Section::Advanced => "tuning raw config.toml",
            Section::About => "version data location logs telemetry updates",
        }
    }

    /// The core's `group` for the settings this section shows.
    fn group(self) -> Option<&'static str> {
        match self {
            Section::General => Some("general"),
            Section::Models => Some("models"),
            Section::Permissions => Some("permissions"),
            Section::Limits => Some("limits"),
            Section::Servers => Some("servers"),
            _ => None,
        }
    }

    /// `/settings models`, `/settings sandbox`, ...
    pub fn parse(s: &str) -> Option<Section> {
        let s = s.trim().to_lowercase();
        Some(match s.as_str() {
            "general" => Section::General,
            "models" | "model" | "providers" | "provider" => Section::Models,
            "secrets" | "secret" | "keys" => Section::Secrets,
            "permissions" | "sandbox" | "approvals" | "security" => Section::Permissions,
            "limits" | "budgets" | "budget" => Section::Limits,
            "servers" | "server" | "mcp" => Section::Servers,
            "advanced" => Section::Advanced,
            "about" | "version" => Section::About,
            _ => return None,
        })
    }
}

/// An existing panel a row of the settings screen opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Providers,
    Secrets,
    Mcp,
}

/// What the app should do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsAction {
    None,
    Close,
    Open(Target),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    Setting(String),
    Action(Target, &'static str, &'static str),
    Info(String, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Browse,
    /// Typing the new value of `key`.
    Edit {
        key: String,
        buf: String,
    },
    /// Typing a search; Enter keeps the filter.
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sections,
    Rows,
}

pub struct SettingsPanel {
    pub view: ConfigView,
    pub section: Section,
    pub focus: Focus,
    pub selected: usize,
    pub mode: Mode,
    pub query: String,
    /// Feedback for the last action: `(is_error, text)`.
    pub message: Option<(bool, String)>,
}

impl SettingsPanel {
    pub fn open(backend: &dyn Backend, section: Option<Section>) -> Result<Self, String> {
        let view = backend.config_view()?;
        if let Some(e) = &view.error {
            return Err(format!(
                "config.toml doesn't load, so settings can't be changed until it is fixed: {e}"
            ));
        }
        Ok(Self {
            view,
            section: section.unwrap_or(Section::General),
            focus: if section.is_some() {
                Focus::Rows
            } else {
                Focus::Sections
            },
            selected: 0,
            mode: Mode::Browse,
            query: String::new(),
            message: None,
        })
    }

    /// Open on a search for `query` across every section.
    pub fn open_search(backend: &dyn Backend, query: &str) -> Result<Self, String> {
        let mut p = Self::open(backend, None)?;
        p.query = query.trim().to_owned();
        p.focus = Focus::Rows;
        Ok(p)
    }

    fn settings(&self) -> impl Iterator<Item = &Setting> {
        self.view.sections.iter().flat_map(|s| &s.settings)
    }

    fn find(&self, key: &str) -> Option<&Setting> {
        self.settings().find(|s| s.key == key)
    }

    fn in_group(&self, group: &str, advanced: bool) -> Vec<Row> {
        self.settings()
            .filter(|s| s.group == group && s.advanced == advanced)
            .map(|s| Row::Setting(s.key.clone()))
            .collect()
    }

    fn matches(s: &Setting, q: &str) -> bool {
        s.key.to_lowercase().contains(q)
            || label(s).to_lowercase().contains(q)
            || s.description.to_lowercase().contains(q)
            || s.security
                .as_deref()
                .is_some_and(|x| x.to_lowercase().contains(q))
    }

    fn section_actions(section: Section) -> Vec<Row> {
        match section {
            Section::Models => vec![Row::Action(
                Target::Providers,
                "Providers and API keys",
                "add or change providers, keys and the default model (/setup)",
            )],
            Section::Secrets => vec![Row::Action(
                Target::Secrets,
                "Secrets",
                "set and remove API keys and tokens; values are never shown (/secrets)",
            )],
            Section::Servers => vec![Row::Action(
                Target::Mcp,
                "MCP servers",
                "add, test and manage MCP servers (/mcp)",
            )],
            _ => Vec::new(),
        }
    }

    fn about_rows(&self) -> Vec<Row> {
        let path = std::path::Path::new(&self.view.path);
        let dir = path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        vec![
            Row::Info("tern".into(), env!("CARGO_PKG_VERSION").into()),
            Row::Info("Config file".into(), self.view.path.clone()),
            Row::Info("Data folder".into(), dir.clone()),
            Row::Info("Logs".into(), format!("{dir}/logs")),
            Row::Info(
                "Telemetry".into(),
                "none: tern sends no telemetry and doesn't check for updates".into(),
            ),
            Row::Info(
                "Environment".into(),
                "TERN_HOME moves the data folder, TERN_LOG sets the log level".into(),
            ),
        ]
    }

    /// The rows shown now: the search results across every section, or the
    /// selected section's.
    fn rows(&self) -> Vec<Row> {
        let q = self.query.trim().to_lowercase();
        if !q.is_empty() {
            let mut rows = Vec::new();
            for s in Section::ALL {
                if s.title().to_lowercase().contains(&q) || s.keywords().contains(q.as_str()) {
                    rows.extend(Self::section_actions(s));
                }
            }
            rows.extend(
                self.settings()
                    .filter(|s| Self::matches(s, &q))
                    .map(|s| Row::Setting(s.key.clone())),
            );
            return rows;
        }
        match self.section {
            Section::About => self.about_rows(),
            Section::Advanced => self
                .view
                .sections
                .iter()
                .flat_map(|s| &s.settings)
                .filter(|s| s.advanced)
                .map(|s| Row::Setting(s.key.clone()))
                .collect(),
            sec => {
                let mut rows = Self::section_actions(sec);
                if let Some(g) = sec.group() {
                    rows.extend(self.in_group(g, false));
                }
                rows
            }
        }
    }

    fn selected_row(&self) -> Option<Row> {
        self.rows().into_iter().nth(self.selected)
    }

    /// The setting under the cursor, if the row is one.
    pub fn current(&self) -> Option<&Setting> {
        match self.selected_row()? {
            Row::Setting(k) => self.find(&k),
            _ => None,
        }
    }

    /// Settings saved but waiting on a restart.
    pub fn pending(&self) -> usize {
        self.settings().filter(|s| s.restart_pending).count()
    }

    /// Lines for the screen, built from the rows.
    pub fn lines(&self) -> Vec<ListLine> {
        self.rows()
            .into_iter()
            .map(|r| match r {
                Row::Setting(k) => {
                    let s = self.find(&k).cloned();
                    match s {
                        Some(s) => ListLine {
                            name: label(&s),
                            value: show(&s.value, s.kind),
                            marks: marks(&s),
                            risky: s.risky,
                            action: false,
                        },
                        None => ListLine::default(),
                    }
                }
                Row::Action(_, name, _) => ListLine {
                    name: name.to_owned(),
                    value: "open".into(),
                    action: true,
                    ..ListLine::default()
                },
                Row::Info(name, value) => ListLine {
                    name,
                    value,
                    ..ListLine::default()
                },
            })
            .collect()
    }

    /// The text under the list for the row under the cursor.
    pub fn detail(&self) -> Vec<String> {
        match self.selected_row() {
            Some(Row::Setting(k)) => {
                let Some(s) = self.find(&k) else {
                    return vec![];
                };
                let mut out = vec![format!("{}  ({})", s.key, kind_name(s))];
                if !s.description.is_empty() {
                    out.push(s.description.clone());
                }
                if let Some(sec) = &s.security {
                    out.push(format!("Safety: {sec}"));
                }
                let mut notes = Vec::new();
                if s.restart_required {
                    notes.push("applies when tern restarts".to_owned());
                } else {
                    notes.push("applies from the next run".to_owned());
                }
                if s.set {
                    notes.push(format!("default {}", show(&s.default, s.kind)));
                }
                out.push(notes.join(" · "));
                out
            }
            Some(Row::Action(_, _, detail)) => vec![detail.to_owned()],
            _ => vec![],
        }
    }

    fn clamp(&mut self) {
        let n = self.rows().len();
        self.selected = self.selected.min(n.saturating_sub(1));
    }

    fn set_section(&mut self, section: Section) {
        self.section = section;
        self.selected = 0;
        self.query.clear();
        self.message = None;
    }

    fn replace(&mut self, new: Setting) {
        for sec in &mut self.view.sections {
            for s in &mut sec.settings {
                if s.key == new.key {
                    *s = new.clone();
                }
            }
        }
        let pending = self.pending() > 0;
        self.view.restart_required = pending;
    }

    fn apply(&mut self, backend: &dyn Backend, key: &str, value: Option<Value>) -> bool {
        match backend.change_setting(key, value) {
            Ok(s) => {
                let note = if s.restart_pending {
                    format!("saved {key}; applies when tern restarts")
                } else {
                    format!("saved {key}")
                };
                self.replace(s);
                self.message = Some((false, note));
                true
            }
            Err(e) => {
                self.message = Some((true, e));
                false
            }
        }
    }

    /// Handle a key.
    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> SettingsAction {
        let mode = std::mem::replace(&mut self.mode, Mode::Browse);
        match mode {
            Mode::Edit { key: k, mut buf } => {
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => match self.find(&k).map(|s| parse(s, &buf)) {
                        Some(Ok(v)) => {
                            // A refusal keeps the text, to correct.
                            if !self.apply(backend, &k, Some(v)) {
                                self.mode = Mode::Edit { key: k, buf };
                            }
                        }
                        Some(Err(e)) => {
                            self.message = Some((true, e));
                            self.mode = Mode::Edit { key: k, buf };
                        }
                        None => {}
                    },
                    KeyCode::Backspace => {
                        buf.pop();
                        self.mode = Mode::Edit { key: k, buf };
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        buf.clear();
                        self.mode = Mode::Edit { key: k, buf };
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        buf.push(c);
                        self.mode = Mode::Edit { key: k, buf };
                    }
                    _ => self.mode = Mode::Edit { key: k, buf },
                }
                SettingsAction::None
            }
            Mode::Search => {
                match key.code {
                    KeyCode::Esc => {
                        self.query.clear();
                    }
                    KeyCode::Enter => self.focus = Focus::Rows,
                    KeyCode::Backspace => {
                        self.query.pop();
                        self.mode = Mode::Search;
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.query.push(c);
                        self.mode = Mode::Search;
                    }
                    _ => self.mode = Mode::Search,
                }
                self.selected = 0;
                SettingsAction::None
            }
            Mode::Browse => self.browse(key, backend),
        }
    }

    fn browse(&mut self, key: KeyEvent, backend: &dyn Backend) -> SettingsAction {
        self.message = None;
        let searching = !self.query.is_empty();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if searching {
                    self.query.clear();
                    self.selected = 0;
                } else {
                    return SettingsAction::Close;
                }
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Search;
                self.focus = Focus::Rows;
            }
            KeyCode::Char(c @ '1'..='8') => {
                self.set_section(Section::ALL[c as usize - '1' as usize]);
                self.focus = Focus::Rows;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Sections => Focus::Rows,
                    Focus::Rows => Focus::Sections,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Sections => {
                    let i = Section::ALL
                        .iter()
                        .position(|s| *s == self.section)
                        .unwrap_or(0);
                    self.set_section(Section::ALL[i.saturating_sub(1)]);
                }
                Focus::Rows => self.selected = self.selected.saturating_sub(1),
            },
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Sections => {
                    let i = Section::ALL
                        .iter()
                        .position(|s| *s == self.section)
                        .unwrap_or(0);
                    self.set_section(Section::ALL[(i + 1).min(Section::ALL.len() - 1)]);
                }
                Focus::Rows => {
                    self.selected = (self.selected + 1).min(self.rows().len().saturating_sub(1));
                }
            },
            KeyCode::Right | KeyCode::Char('l') if self.focus == Focus::Sections => {
                self.focus = Focus::Rows;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l')
                if self.focus == Focus::Rows =>
            {
                let forward = matches!(key.code, KeyCode::Right | KeyCode::Char('l'));
                match self.current().cloned() {
                    Some(s) if s.kind == Kind::Choice && s.editable => {
                        self.cycle(backend, &s, forward);
                    }
                    _ if !forward && !searching => self.focus = Focus::Sections,
                    _ => {}
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('e') => {
                if self.focus == Focus::Sections {
                    self.focus = Focus::Rows;
                } else {
                    return self.activate(backend);
                }
            }
            KeyCode::Char('r') | KeyCode::Delete | KeyCode::Backspace
                if self.focus == Focus::Rows =>
            {
                if let Some(s) = self.current().cloned() {
                    if !s.editable {
                        self.message = Some((true, format!("{} can't be changed here", s.key)));
                    } else if !s.set {
                        self.message = Some((false, format!("{} is already the default", s.key)));
                    } else {
                        self.apply(backend, &s.key, None);
                    }
                }
            }
            _ => {}
        }
        self.clamp();
        SettingsAction::None
    }

    fn cycle(&mut self, backend: &dyn Backend, s: &Setting, forward: bool) {
        let cur = s.value.as_str().unwrap_or_default();
        let n = s.choices.len();
        if n == 0 {
            return;
        }
        let i = s.choices.iter().position(|c| c == cur).unwrap_or(0);
        let next = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        self.apply(
            backend,
            &s.key,
            Some(Value::String(s.choices[next].clone())),
        );
    }

    fn activate(&mut self, backend: &dyn Backend) -> SettingsAction {
        match self.selected_row() {
            Some(Row::Action(t, _, _)) => return SettingsAction::Open(t),
            Some(Row::Setting(k)) => {
                let Some(s) = self.find(&k).cloned() else {
                    return SettingsAction::None;
                };
                if !s.editable {
                    self.message = Some((
                        true,
                        format!("{} can't be changed here; edit config.toml", s.key),
                    ));
                    return SettingsAction::None;
                }
                match s.kind {
                    Kind::Bool => {
                        let on = s.value.as_bool().unwrap_or(false);
                        self.apply(backend, &s.key, Some(Value::Bool(!on)));
                    }
                    Kind::Choice => self.cycle(backend, &s, true),
                    _ => {
                        self.mode = Mode::Edit {
                            key: s.key.clone(),
                            buf: edit_text(&s.value),
                        };
                    }
                }
            }
            _ => {}
        }
        SettingsAction::None
    }

    pub fn on_paste(&mut self, text: &str) {
        let line = text.lines().next().unwrap_or_default();
        match &mut self.mode {
            Mode::Edit { buf, .. } => buf.push_str(line),
            Mode::Search => {
                self.query.push_str(line);
                self.selected = 0;
            }
            Mode::Browse => {}
        }
    }
}

/// One row as the screen draws it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListLine {
    pub name: String,
    pub value: String,
    pub marks: String,
    pub risky: bool,
    pub action: bool,
}

/// `sandbox.shell_timeout_secs` becomes "Shell timeout (seconds)".
pub fn label(s: &Setting) -> String {
    let name = s.key.rsplit('.').next().unwrap_or(&s.key);
    let mut words: Vec<&str> = name.split('_').collect();
    let mut unit = None;
    if words.len() > 1 {
        unit = match words[words.len() - 1] {
            "secs" => Some("seconds"),
            "hours" => Some("hours"),
            _ => None,
        };
        if unit.is_some() {
            words.pop();
        }
    }
    let text = words
        .iter()
        .map(|w| match *w {
            "md" => ".md".to_owned(),
            "usd" => "USD".to_owned(),
            "mcp" => "MCP".to_owned(),
            "api" => "API".to_owned(),
            "tls" => "TLS".to_owned(),
            "url" => "URL".to_owned(),
            "cpus" => "CPUs".to_owned(),
            "pids" => "processes".to_owned(),
            w => w.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" .md", ".md");
    let mut chars = text.chars();
    let label = match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect::<String>(),
        None => s.key.clone(),
    };
    match unit {
        Some(u) => format!("{label} ({u})"),
        None => label,
    }
}

fn kind_name(s: &Setting) -> &'static str {
    match s.kind {
        Kind::Bool => "on/off",
        Kind::Int => "whole number",
        Kind::Float => "number",
        Kind::Text => "text",
        Kind::Choice => "choice",
        Kind::List => "list, comma separated",
        Kind::Model => "provider:model, or several separated by commas for fallbacks",
        Kind::Other => "edit in config.toml",
    }
}

fn marks(s: &Setting) -> String {
    let mut m = String::new();
    if s.set && s.value != s.default {
        m.push('*');
    }
    if s.restart_pending {
        m.push('↻');
    }
    m
}

/// A value as the list shows it.
pub fn show(v: &Value, kind: Kind) -> String {
    match v {
        Value::Null => "(unset)".into(),
        Value::Bool(true) => "on".into(),
        Value::Bool(false) => "off".into(),
        Value::String(s) if s.is_empty() => "(empty)".into(),
        Value::String(s) => s.clone(),
        Value::Array(a) if a.is_empty() => "(none)".into(),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| x.to_string())
            })
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) if kind == Kind::Other => "(set in config.toml)".into(),
        other => other.to_string(),
    }
}

fn edit_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

/// The typed text as the JSON the API expects, or what is wrong with it.
/// The core checks ranges and the whole file; this only reads the type.
fn parse(s: &Setting, raw: &str) -> Result<Value, String> {
    let t = raw.trim();
    if t.is_empty() {
        return match s.kind {
            Kind::List => Ok(Value::Array(vec![])),
            Kind::Text if !s.optional => Ok(Value::String(String::new())),
            _ if s.optional => Ok(Value::Null),
            _ => Err("needs a value".into()),
        };
    }
    let items = || -> Vec<Value> {
        t.split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| Value::String(p.to_owned()))
            .collect()
    };
    match s.kind {
        Kind::Int => t
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| "a whole number".into()),
        Kind::Float => t
            .parse::<f64>()
            .map(Value::from)
            .map_err(|_| "a number".into()),
        Kind::List => Ok(Value::Array(items())),
        Kind::Model => {
            let mut v = items();
            if v.len() == 1 {
                Ok(v.remove(0))
            } else {
                Ok(Value::Array(v))
            }
        }
        _ => Ok(Value::String(t.to_owned())),
    }
}
