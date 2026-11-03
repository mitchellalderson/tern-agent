//! The `/setup` panel (docs/provider-setup-spec.md §6–7): add, change and
//! remove providers. Choose a kind, give a key (checked before it is
//! saved), pick a model. Network calls run as [`Task`]s off the input loop.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tern_core::config::ProviderKind;
use tern_core::models::{ProviderDraft, ProviderModels, ProviderStatus, Resolved, Verified};
use tern_core::secrets::keys::mask_key;

use crate::models::{ListKey, ModelList};
use crate::{Backend, BackendResult};

pub const ANTHROPIC_KEY_URL: &str = "https://console.anthropic.com/settings/keys";

/// Backend work the event loop runs in the background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    Models {
        provider: Option<String>,
        refresh: bool,
    },
    Verify {
        draft: ProviderDraft,
        existing: Option<String>,
    },
    Save {
        draft: ProviderDraft,
        existing: Option<String>,
        default_model: Option<String>,
    },
}

/// A finished [`Task`].
#[derive(Debug)]
pub enum TaskDone {
    Models(BackendResult<Vec<ProviderModels>>),
    Verified(BackendResult<Verified>),
    Saved(BackendResult<ProviderStatus>),
}

impl Task {
    pub async fn run(self, backend: &dyn Backend) -> TaskDone {
        match self {
            Task::Models { provider, refresh } => {
                TaskDone::Models(backend.models(provider, refresh).await)
            }
            Task::Verify { draft, existing } => {
                TaskDone::Verified(backend.verify_provider(draft, existing).await)
            }
            Task::Save {
                draft,
                existing,
                default_model,
            } => TaskDone::Saved(backend.save_provider(draft, existing, default_model).await),
        }
    }
}

/// What the user has filled in so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub kind: ProviderKind,
    /// The provider being changed, when not adding one.
    pub existing: Option<String>,
    pub name: String,
    pub base_url: String,
    pub key: String,
    pub use_env_key: bool,
}

impl Form {
    fn new(kind: ProviderKind) -> Self {
        Self {
            kind,
            existing: None,
            name: String::new(),
            base_url: String::new(),
            key: String::new(),
            use_env_key: false,
        }
    }

    /// The Endpoint step's fields: 0 = base URL, 1 = name.
    fn field(&mut self, field: usize) -> &mut String {
        if field == 0 {
            &mut self.base_url
        } else {
            &mut self.name
        }
    }

    fn draft(&self) -> ProviderDraft {
        let opt = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_owned());
        ProviderDraft {
            kind: Some(self.kind),
            name: opt(&self.name),
            base_url: opt(&self.base_url),
            key: if self.use_env_key {
                None
            } else {
                opt(&self.key)
            },
            use_env_key: self.use_env_key,
        }
    }

    /// What to call the provider before the backend has named it.
    pub fn label(&self) -> String {
        match (&self.existing, self.name.trim(), self.kind) {
            (Some(n), _, _) => n.clone(),
            (None, n, _) if !n.is_empty() => n.to_owned(),
            (None, _, ProviderKind::Anthropic) => "Anthropic".into(),
            (None, _, ProviderKind::OpenaiCompat) => "the server".into(),
        }
    }
}

#[derive(Debug)]
pub enum Step {
    /// Configured providers, then "Add a provider".
    List,
    Manage {
        name: String,
        selected: usize,
    },
    ConfirmRemove(String),
    Kind {
        selected: usize,
    },
    /// Base URL and name for an OpenAI-compatible provider.
    Endpoint {
        form: Form,
        /// 0 = URL, 1 = name.
        field: usize,
    },
    Key(Form),
    Verifying(Form),
    Model {
        form: Form,
        list: ModelList,
        make_default: bool,
        /// Only choosing a default model for `form.existing`; nothing to save.
        default_only: bool,
    },
    Saving {
        form: Form,
        list: ModelList,
        make_default: bool,
    },
}

pub const MANAGE: &[&str] = &["Change API key", "Choose default model", "Remove"];
pub const KINDS: &[(&str, &str)] = &[
    ("Anthropic", "Claude models"),
    (
        "OpenAI-compatible",
        "OpenAI, OpenRouter, Ollama, or any server with the OpenAI API",
    ),
];

pub struct SetupPanel {
    pub providers: Vec<ProviderStatus>,
    pub selected: usize,
    pub step: Step,
    /// `(is_error, text)`; warnings are shown as errors that don't block.
    pub message: Option<(bool, String)>,
    /// A line shown above everything, e.g. the first-run welcome.
    pub intro: Option<String>,
    /// Set when providers or the default model changed.
    pub changed: bool,
}

/// What a key asks the app to do.
#[derive(Debug, PartialEq, Eq)]
pub enum SetupAction {
    None,
    Close,
    Run(Task),
}

impl SetupPanel {
    pub fn open(backend: &dyn Backend) -> Result<Self, String> {
        let providers = backend.providers()?;
        let step = if providers.is_empty() {
            Step::Kind { selected: 0 }
        } else {
            Step::List
        };
        Ok(Self {
            providers,
            selected: 0,
            step,
            message: None,
            intro: None,
            changed: false,
        })
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.step, Step::Verifying(_) | Step::Saving { .. })
            || matches!(&self.step, Step::Model { list, .. } if list.loading)
    }

    fn refresh(&mut self, backend: &dyn Backend) {
        match backend.providers() {
            Ok(p) => {
                self.providers = p;
                self.selected = self.selected.min(self.providers.len());
            }
            Err(e) => self.message = Some((true, e)),
        }
    }

    fn has_default(&self) -> bool {
        self.providers.iter().any(|p| p.is_default)
    }

    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> SetupAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.is_busy() {
            // Nothing to cancel: the check finishes on its own.
            return SetupAction::None;
        }
        let step = std::mem::replace(&mut self.step, Step::List);
        let (step, action) = match step {
            Step::List => {
                self.message = None;
                let rows = self.providers.len() + 1;
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return SetupAction::Close,
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.selected = self.selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.selected = (self.selected + 1).min(rows - 1);
                    }
                    KeyCode::Char('a' | 'n') => {
                        return self.go(Step::Kind { selected: 0 });
                    }
                    KeyCode::Char('d') | KeyCode::Delete => {
                        if let Some(p) = self.providers.get(self.selected) {
                            return self.go(Step::ConfirmRemove(p.name.clone()));
                        }
                    }
                    KeyCode::Enter => {
                        return match self.providers.get(self.selected) {
                            Some(p) => self.go(Step::Manage {
                                name: p.name.clone(),
                                selected: 0,
                            }),
                            None => self.go(Step::Kind { selected: 0 }),
                        };
                    }
                    _ => {}
                }
                (Step::List, SetupAction::None)
            }
            Step::Manage { name, mut selected } => match key.code {
                KeyCode::Esc => (Step::List, SetupAction::None),
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    (Step::Manage { name, selected }, SetupAction::None)
                }
                KeyCode::Down => {
                    selected = (selected + 1).min(MANAGE.len() - 1);
                    (Step::Manage { name, selected }, SetupAction::None)
                }
                KeyCode::Enter => {
                    let kind = self
                        .providers
                        .iter()
                        .find(|p| p.name == name)
                        .map_or(ProviderKind::OpenaiCompat, |p| p.kind);
                    let mut form = Form::new(kind);
                    form.existing = Some(name.clone());
                    match selected {
                        0 => (Step::Key(form), SetupAction::None),
                        1 => {
                            let current = backend.default_model();
                            (
                                Step::Model {
                                    form,
                                    list: ModelList::new(current),
                                    make_default: true,
                                    default_only: true,
                                },
                                SetupAction::Run(Task::Models {
                                    provider: Some(name),
                                    refresh: false,
                                }),
                            )
                        }
                        _ => (Step::ConfirmRemove(name), SetupAction::None),
                    }
                }
                _ => (Step::Manage { name, selected }, SetupAction::None),
            },
            Step::ConfirmRemove(name) => {
                let remove_secret = match key.code {
                    KeyCode::Char('y' | 'Y') => Some(true),
                    KeyCode::Char('k' | 'K') => Some(false),
                    _ => None,
                };
                if let Some(remove_secret) = remove_secret {
                    match backend.remove_provider(&name, remove_secret) {
                        Ok(()) => {
                            self.changed = true;
                            self.refresh(backend);
                            self.message = Some((false, format!("Removed {name}.")));
                        }
                        Err(e) => self.message = Some((true, e)),
                    }
                }
                (Step::List, SetupAction::None)
            }
            Step::Kind { mut selected } => match key.code {
                KeyCode::Esc if self.providers.is_empty() => return SetupAction::Close,
                KeyCode::Esc => (Step::List, SetupAction::None),
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    (Step::Kind { selected }, SetupAction::None)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(KINDS.len() - 1);
                    (Step::Kind { selected }, SetupAction::None)
                }
                KeyCode::Char('1') => (
                    Step::Key(Form::new(ProviderKind::Anthropic)),
                    SetupAction::None,
                ),
                KeyCode::Char('2') => (self.endpoint(), SetupAction::None),
                KeyCode::Enter if selected == 0 => (
                    Step::Key(Form::new(ProviderKind::Anthropic)),
                    SetupAction::None,
                ),
                KeyCode::Enter => (self.endpoint(), SetupAction::None),
                _ => (Step::Kind { selected }, SetupAction::None),
            },
            Step::Endpoint {
                mut form,
                mut field,
            } => {
                let url_empty = form.base_url.trim().is_empty();
                match key.code {
                    KeyCode::Esc => (Step::Kind { selected: 1 }, SetupAction::None),
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                        field = 1 - field;
                        (Step::Endpoint { form, field }, SetupAction::None)
                    }
                    KeyCode::Enter if url_empty => {
                        self.message = Some((true, "Enter the server's base URL.".into()));
                        (Step::Endpoint { form, field: 0 }, SetupAction::None)
                    }
                    KeyCode::Enter if field == 0 => {
                        (Step::Endpoint { form, field: 1 }, SetupAction::None)
                    }
                    KeyCode::Enter => {
                        self.message = None;
                        (Step::Key(form), SetupAction::None)
                    }
                    KeyCode::Backspace => {
                        form.field(field).pop();
                        (Step::Endpoint { form, field }, SetupAction::None)
                    }
                    KeyCode::Char('u') if ctrl => {
                        form.field(field).clear();
                        (Step::Endpoint { form, field }, SetupAction::None)
                    }
                    KeyCode::Char(c) if !ctrl && !c.is_whitespace() => {
                        self.message = None;
                        let c = if field == 1 {
                            c.to_ascii_lowercase()
                        } else {
                            c
                        };
                        form.field(field).push(c);
                        (Step::Endpoint { form, field }, SetupAction::None)
                    }
                    _ => (Step::Endpoint { form, field }, SetupAction::None),
                }
            }
            Step::Key(mut form) => match key.code {
                KeyCode::Esc if form.existing.is_some() => (Step::List, SetupAction::None),
                KeyCode::Esc if form.kind == ProviderKind::OpenaiCompat => {
                    (Step::Endpoint { form, field: 0 }, SetupAction::None)
                }
                KeyCode::Esc => (Step::Kind { selected: 0 }, SetupAction::None),
                KeyCode::Enter | KeyCode::Char('e') if key.code == KeyCode::Enter || ctrl => {
                    form.use_env_key = key.code != KeyCode::Enter;
                    let needs_key = form.kind == ProviderKind::Anthropic || form.existing.is_some();
                    if !form.use_env_key && needs_key && form.key.trim().is_empty() {
                        self.message = Some((true, "Paste your API key first.".into()));
                        (Step::Key(form), SetupAction::None)
                    } else {
                        self.message = None;
                        let task = Task::Verify {
                            draft: form.draft(),
                            existing: form.existing.clone(),
                        };
                        (Step::Verifying(form), SetupAction::Run(task))
                    }
                }
                KeyCode::Backspace => {
                    form.key.pop();
                    (Step::Key(form), SetupAction::None)
                }
                KeyCode::Char('u') if ctrl => {
                    form.key.clear();
                    (Step::Key(form), SetupAction::None)
                }
                KeyCode::Char(c) if !ctrl => {
                    self.message = None;
                    form.key.push(c);
                    (Step::Key(form), SetupAction::None)
                }
                _ => (Step::Key(form), SetupAction::None),
            },
            Step::Model {
                form,
                mut list,
                mut make_default,
                default_only,
            } => {
                if key.code == KeyCode::Esc {
                    let back = if default_only {
                        Step::List
                    } else {
                        Step::Key(form)
                    };
                    return self.go(back);
                }
                if key.code == KeyCode::Tab && !default_only {
                    make_default = !make_default;
                    return self.go(Step::Model {
                        form,
                        list,
                        make_default,
                        default_only,
                    });
                }
                let chosen = match list.on_key(key) {
                    ListKey::Chosen(m) => Some(m),
                    ListKey::Empty if !default_only => None,
                    ListKey::Empty => {
                        self.message = Some((true, "Type a model id.".into()));
                        return self.go(Step::Model {
                            form,
                            list,
                            make_default,
                            default_only,
                        });
                    }
                    ListKey::Handled | ListKey::Ignored => {
                        return self.go(Step::Model {
                            form,
                            list,
                            make_default,
                            default_only,
                        });
                    }
                };
                if default_only {
                    let provider = form.existing.clone().unwrap_or_default();
                    let model = chosen
                        .map(|m| full_model(&provider, &m))
                        .unwrap_or_default();
                    match backend.set_default_model(&model) {
                        Ok(Resolved { model, warning }) => {
                            self.changed = true;
                            self.refresh(backend);
                            let mut msg = format!("Default model: {model}.");
                            if let Some(w) = warning {
                                msg.push_str(&format!(" ({w})"));
                            }
                            self.message = Some((false, msg));
                            (Step::List, SetupAction::None)
                        }
                        Err(e) => {
                            self.message = Some((true, e));
                            (
                                Step::Model {
                                    form,
                                    list,
                                    make_default,
                                    default_only,
                                },
                                SetupAction::None,
                            )
                        }
                    }
                } else {
                    let name = form.name.trim().to_owned();
                    let task = Task::Save {
                        draft: form.draft(),
                        existing: form.existing.clone(),
                        default_model: chosen
                            .filter(|_| make_default)
                            .map(|m| full_model(&name, &m)),
                    };
                    (
                        Step::Saving {
                            form,
                            list,
                            make_default,
                        },
                        SetupAction::Run(task),
                    )
                }
            }
            busy @ (Step::Verifying(_) | Step::Saving { .. }) => (busy, SetupAction::None),
        };
        self.step = step;
        action
    }

    fn go(&mut self, step: Step) -> SetupAction {
        self.step = step;
        SetupAction::None
    }

    fn endpoint(&self) -> Step {
        Step::Endpoint {
            form: Form::new(ProviderKind::OpenaiCompat),
            field: 0,
        }
    }

    /// Pasted text goes into the field being edited.
    pub fn on_paste(&mut self, text: &str) {
        let text = text.trim();
        match &mut self.step {
            Step::Key(form) => form.key.push_str(text),
            Step::Endpoint { form, field } => {
                if *field == 0 {
                    form.base_url.push_str(text);
                } else {
                    form.name.push_str(&text.to_ascii_lowercase());
                }
            }
            Step::Model { list, .. } => list.on_paste(text),
            _ => {}
        }
    }

    pub fn on_task_done(&mut self, done: TaskDone, backend: &dyn Backend) {
        let step = std::mem::replace(&mut self.step, Step::List);
        self.step = match (step, done) {
            (Step::Verifying(mut form), TaskDone::Verified(Ok(v))) if v.ok => {
                form.name = v.name.clone();
                if let Some(url) = &v.suggested_base_url {
                    form.base_url = url.clone();
                }
                let mut notes = v.warnings.clone();
                notes.extend(v.message.clone());
                if v.suggested_base_url.is_some() {
                    notes.push(format!("Using {} (added /v1).", form.base_url));
                }
                let hint = v.key_hint.as_deref().map(|h| format!("Key {h} works. "));
                self.message = Some(match notes.is_empty() {
                    true => (
                        false,
                        format!(
                            "{}{} models available.",
                            hint.unwrap_or_default(),
                            v.models.len()
                        ),
                    ),
                    false => (true, notes.join(" ")),
                });
                let mut list = ModelList::new(None);
                list.set_groups(vec![ProviderModels {
                    provider: v.name.clone(),
                    error: v.error.clone(),
                    message: v.message.clone(),
                    fetched_at: None,
                    models: v.models,
                }]);
                Step::Model {
                    form,
                    list,
                    make_default: !self.has_default(),
                    default_only: false,
                }
            }
            (Step::Verifying(form), TaskDone::Verified(Ok(v))) => {
                let mut msg = v.message.unwrap_or_else(|| "That key didn't work.".into());
                for w in v.warnings {
                    msg.push(' ');
                    msg.push_str(&w);
                }
                self.message = Some((true, msg));
                Step::Key(form)
            }
            (Step::Verifying(form), TaskDone::Verified(Err(e))) => {
                self.message = Some((true, e));
                if form.kind == ProviderKind::OpenaiCompat && form.existing.is_none() {
                    Step::Endpoint { form, field: 0 }
                } else {
                    Step::Key(form)
                }
            }
            (Step::Saving { .. }, TaskDone::Saved(Ok(status))) => {
                self.changed = true;
                self.refresh(backend);
                self.selected = self
                    .providers
                    .iter()
                    .position(|p| p.name == status.name)
                    .unwrap_or(0);
                let key = status
                    .key_hint
                    .as_deref()
                    .map(|h| format!(" (key {h})"))
                    .unwrap_or_default();
                let default = if status.is_default {
                    " It's the default provider now."
                } else {
                    ""
                };
                self.message = Some((false, format!("Saved {}{key}.{default}", status.name)));
                self.intro = None;
                Step::List
            }
            (
                Step::Saving {
                    form,
                    list,
                    make_default,
                },
                TaskDone::Saved(Err(e)),
            ) => {
                self.message = Some((true, e));
                Step::Model {
                    form,
                    list,
                    make_default,
                    default_only: false,
                }
            }
            (
                Step::Model {
                    form,
                    mut list,
                    make_default,
                    default_only,
                },
                TaskDone::Models(result),
            ) => {
                match result {
                    Ok(groups) => list.set_groups(groups),
                    Err(e) => {
                        list.set_groups(Vec::new());
                        self.message = Some((true, e));
                    }
                }
                Step::Model {
                    form,
                    list,
                    make_default,
                    default_only,
                }
            }
            // A result for a step the user has left.
            (step, _) => step,
        };
    }

    /// The pasted key, masked, for display.
    pub fn masked(form: &Form) -> String {
        if form.key.is_empty() {
            String::new()
        } else {
            mask_key(form.key.trim())
        }
    }
}

/// `model` as `provider:model`, unless it already names `provider`.
fn full_model(provider: &str, model: &str) -> String {
    match model.split_once(':') {
        Some((p, _)) if p == provider || provider.is_empty() => model.to_owned(),
        _ => format!("{provider}:{model}"),
    }
}

#[cfg(test)]
mod tests;
