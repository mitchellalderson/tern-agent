//! A filterable list of models grouped by provider, used by the `/model`
//! picker and the model step of `/setup`. Typed text is always a choice
//! too, so a model the provider doesn't list is one Enter away.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tern_core::models::{ModelInfo, ProviderModels};

/// One line of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A provider heading, with why its list is missing or stale.
    Header { title: String, note: Option<String> },
    Model {
        /// `provider:id`.
        full: String,
        id: String,
        name: Option<String>,
        context_window: Option<u64>,
        current: bool,
    },
    /// Use the filter text as typed.
    Typed(String),
}

impl Row {
    fn choice(&self) -> Option<&str> {
        match self {
            Row::Header { .. } => None,
            Row::Model { full, .. } => Some(full),
            Row::Typed(t) => Some(t),
        }
    }
}

#[derive(Debug, Default)]
pub struct ModelList {
    pub groups: Vec<ProviderModels>,
    pub filter: String,
    /// Index among the selectable rows.
    pub selected: usize,
    pub loading: bool,
    /// The current model, as `provider:id`, listed first.
    pub current: Option<String>,
}

/// What a key did to the list.
#[derive(Debug, PartialEq, Eq)]
pub enum ListKey {
    /// Enter on a row: `provider:id`, or the typed text.
    Chosen(String),
    /// Enter with nothing to choose.
    Empty,
    Handled,
    /// Not a list key; the caller may use it.
    Ignored,
}

impl ModelList {
    pub fn new(current: Option<String>) -> Self {
        Self {
            current,
            loading: true,
            ..Self::default()
        }
    }

    pub fn set_groups(&mut self, groups: Vec<ProviderModels>) {
        self.groups = groups;
        self.loading = false;
        self.clamp();
    }

    pub fn rows(&self) -> Vec<Row> {
        let filter = self.filter.trim();
        let mut rows = Vec::new();
        if filter.is_empty()
            && let Some(cur) = &self.current
        {
            let (provider, id) = cur.split_once(':').unwrap_or(("", cur));
            let info = self
                .groups
                .iter()
                .find(|g| g.provider == provider)
                .and_then(|g| g.models.iter().find(|m| m.id == id));
            rows.push(Row::Header {
                title: "Current".into(),
                note: None,
            });
            rows.push(model_row(provider, id, info, true));
        }
        for g in &self.groups {
            let models: Vec<&ModelInfo> = g
                .models
                .iter()
                .filter(|m| filter.is_empty() || matches(filter, &g.provider, m))
                .collect();
            if models.is_empty() && !filter.is_empty() {
                continue;
            }
            let note = match (&g.message, g.models.is_empty()) {
                (Some(m), true) => Some(format!("couldn't list models: {m}")),
                (Some(m), false) => Some(format!("showing the last list: {m}")),
                (None, true) => Some("no models listed; type a model id".into()),
                (None, false) => None,
            };
            rows.push(Row::Header {
                title: g.provider.clone(),
                note,
            });
            for m in models {
                let full = format!("{}:{}", g.provider, m.id);
                let current = self.current.as_deref() == Some(full.as_str());
                rows.push(model_row(&g.provider, &m.id, Some(m), current));
            }
        }
        if !filter.is_empty() {
            rows.push(Row::Typed(filter.to_owned()));
        }
        rows
    }

    fn choices(&self) -> Vec<String> {
        self.rows()
            .iter()
            .filter_map(|r| r.choice().map(str::to_owned))
            .collect()
    }

    /// The selected choice, if any.
    pub fn chosen(&self) -> Option<String> {
        self.choices().into_iter().nth(self.selected)
    }

    fn clamp(&mut self) {
        let n = self.choices().len();
        self.selected = self.selected.min(n.saturating_sub(1));
    }

    pub fn on_key(&mut self, key: KeyEvent) -> ListKey {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected += 1;
                self.clamp();
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => {
                self.selected += 10;
                self.clamp();
            }
            KeyCode::Enter => {
                return match self.chosen() {
                    Some(c) => ListKey::Chosen(c),
                    None => ListKey::Empty,
                };
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
            }
            KeyCode::Char('u') if ctrl => {
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Char(c) if !ctrl && !c.is_whitespace() => {
                self.filter.push(c);
                self.selected = 0;
            }
            _ => return ListKey::Ignored,
        }
        ListKey::Handled
    }

    pub fn on_paste(&mut self, text: &str) {
        self.filter.push_str(text.trim());
        self.selected = 0;
    }
}

fn model_row(provider: &str, id: &str, info: Option<&ModelInfo>, current: bool) -> Row {
    Row::Model {
        full: format!("{provider}:{id}"),
        id: id.to_owned(),
        name: info.and_then(|m| m.name.clone()),
        context_window: info.and_then(|m| m.context_window),
        current,
    }
}

/// Case-insensitive subsequence match on `provider:id` or the display name.
fn matches(filter: &str, provider: &str, m: &ModelInfo) -> bool {
    let full = format!("{provider}:{}", m.id);
    subsequence(filter, &full) || m.name.as_deref().is_some_and(|n| subsequence(filter, n))
}

fn subsequence(needle: &str, hay: &str) -> bool {
    let mut hay = hay.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|c| hay.any(|h| h == c))
}

/// The `/model` picker.
pub struct ModelPicker {
    pub list: ModelList,
    /// Feedback for the last action: `(is_error, text)`.
    pub message: Option<(bool, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PickerAction {
    None,
    Close,
    /// Use `model` for this session, and as the default when `default`.
    Choose {
        model: String,
        default: bool,
    },
    Refresh,
}

impl ModelPicker {
    pub fn new(current: Option<String>) -> Self {
        Self {
            list: ModelList::new(current),
            message: None,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PickerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return PickerAction::Close,
            KeyCode::Char('r') if ctrl => {
                self.list.loading = true;
                return PickerAction::Refresh;
            }
            KeyCode::Char('d') if ctrl => {
                return match self.list.chosen() {
                    Some(model) => PickerAction::Choose {
                        model,
                        default: true,
                    },
                    None => PickerAction::None,
                };
            }
            _ => {}
        }
        self.message = None;
        match self.list.on_key(key) {
            ListKey::Chosen(model) => PickerAction::Choose {
                model,
                default: false,
            },
            ListKey::Empty => {
                self.message = Some((true, "type a model id, or /setup to add a provider".into()));
                PickerAction::None
            }
            ListKey::Handled | ListKey::Ignored => PickerAction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn group(provider: &str, ids: &[&str]) -> ProviderModels {
        ProviderModels {
            provider: provider.into(),
            error: None,
            message: None,
            fetched_at: Some(0),
            models: ids
                .iter()
                .map(|id| ModelInfo {
                    id: (*id).into(),
                    name: None,
                    context_window: None,
                    created: None,
                })
                .collect(),
        }
    }

    #[test]
    fn filters_and_offers_typed_text() {
        let mut l = ModelList::new(Some("openai:gpt-4o".into()));
        l.set_groups(vec![
            group("anthropic", &["claude-a", "claude-b"]),
            group("openai", &["gpt-4o", "gpt-4o-mini"]),
        ]);
        // Current first, then every model.
        assert_eq!(l.chosen().as_deref(), Some("openai:gpt-4o"));
        assert_eq!(l.choices().len(), 5);

        for c in "cb".chars() {
            l.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(l.choices(), ["anthropic:claude-b", "cb"]);
        l.on_key(key(KeyCode::Down));
        assert_eq!(l.on_key(key(KeyCode::Enter)), ListKey::Chosen("cb".into()));
        l.on_key(key(KeyCode::Down));
        assert_eq!(l.chosen().as_deref(), Some("cb"), "stays on the last row");
    }

    #[test]
    fn notes_failed_lists() {
        let mut l = ModelList::new(None);
        let mut g = group("local", &[]);
        g.message = Some("Couldn't reach localhost.".into());
        l.set_groups(vec![g]);
        assert_eq!(
            l.rows(),
            [Row::Header {
                title: "local".into(),
                note: Some("couldn't list models: Couldn't reach localhost.".into()),
            }]
        );
        assert_eq!(l.on_key(key(KeyCode::Enter)), ListKey::Empty);
    }

    #[test]
    fn picker_keys() {
        let mut p = ModelPicker::new(None);
        p.list.set_groups(vec![group("openai", &["gpt-4o"])]);
        assert_eq!(
            p.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            PickerAction::Choose {
                model: "openai:gpt-4o".into(),
                default: true
            }
        );
        assert_eq!(p.on_key(key(KeyCode::Esc)), PickerAction::Close);
        assert_eq!(
            p.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            PickerAction::Refresh
        );
    }
}
