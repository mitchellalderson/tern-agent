//! The `/secrets` panel: list, set and remove entries in `secrets.toml`
//! without the value ever reaching the transcript, input history or model.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tern_core::SecretStatus;

use crate::Backend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    List,
    /// Typing a new secret's name.
    Name(String),
    /// Typing the value for `name`. Shown only as a count of characters.
    Value { name: String, value: String },
    ConfirmRemove(String),
}

pub struct SecretsPanel {
    pub items: Vec<SecretStatus>,
    pub selected: usize,
    pub mode: Mode,
    /// Feedback for the last action: `(is_error, text)`.
    pub message: Option<(bool, String)>,
}

impl SecretsPanel {
    pub fn open(backend: &dyn Backend, name: Option<&str>) -> Result<Self, String> {
        let mut panel = Self {
            items: backend.secrets()?,
            selected: 0,
            mode: Mode::List,
            message: None,
        };
        if let Some(name) = name {
            tern_core::secrets::check_name(name).map_err(|e| e.to_string())?;
            panel.select(name);
            panel.mode = Mode::Value {
                name: name.to_owned(),
                value: String::new(),
            };
        }
        Ok(panel)
    }

    fn select(&mut self, name: &str) {
        if let Some(i) = self.items.iter().position(|s| s.name == name) {
            self.selected = i;
        }
    }

    fn refresh(&mut self, backend: &dyn Backend, select: &str) {
        match backend.secrets() {
            Ok(items) => {
                self.items = items;
                self.selected = self.selected.min(self.items.len().saturating_sub(1));
                self.select(select);
            }
            Err(e) => self.message = Some((true, e)),
        }
    }

    /// Handle a key. Returns false when the panel should close.
    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mode = std::mem::replace(&mut self.mode, Mode::List);
        self.mode = match mode {
            Mode::List => {
                self.message = None;
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return false,
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.selected = self.selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1));
                    }
                    KeyCode::Char('n' | 'a') => return self.set_mode(Mode::Name(String::new())),
                    KeyCode::Enter | KeyCode::Char('e') => {
                        if let Some(s) = self.items.get(self.selected) {
                            return self.set_mode(Mode::Value {
                                name: s.name.clone(),
                                value: String::new(),
                            });
                        }
                    }
                    KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => {
                        match self.items.get(self.selected) {
                            Some(s) if s.in_file => {
                                return self.set_mode(Mode::ConfirmRemove(s.name.clone()));
                            }
                            Some(s) if s.in_env => {
                                self.message = Some((
                                    true,
                                    format!(
                                        "{} comes from the environment; unset it there",
                                        s.name
                                    ),
                                ));
                            }
                            Some(s) => {
                                self.message =
                                    Some((true, format!("{} isn't set", s.name)));
                            }
                            None => {}
                        }
                    }
                    _ => {}
                }
                Mode::List
            }
            Mode::Name(mut name) => match key.code {
                KeyCode::Esc => Mode::List,
                KeyCode::Enter => match tern_core::secrets::check_name(name.trim()) {
                    Ok(()) => Mode::Value {
                        name: name.trim().to_owned(),
                        value: String::new(),
                    },
                    Err(e) => {
                        self.message = Some((true, e.to_string()));
                        Mode::Name(name)
                    }
                },
                KeyCode::Backspace => {
                    name.pop();
                    Mode::Name(name)
                }
                KeyCode::Char('u') if ctrl => Mode::Name(String::new()),
                KeyCode::Char(c) if !ctrl => {
                    self.message = None;
                    // Secret names are conventionally upper case.
                    name.push(c.to_ascii_uppercase());
                    Mode::Name(name)
                }
                _ => Mode::Name(name),
            },
            Mode::Value { name, mut value } => match key.code {
                KeyCode::Esc => Mode::List,
                KeyCode::Enter => {
                    if value.trim().is_empty() {
                        self.message = Some((true, "type or paste a value first".into()));
                        return self.set_mode(Mode::Value { name, value });
                    }
                    match backend.set_secret(&name, &value) {
                        Ok(()) => {
                            self.refresh(backend, &name);
                            self.message = Some((false, format!("saved {name}")));
                        }
                        Err(e) => self.message = Some((true, e)),
                    }
                    Mode::List
                }
                KeyCode::Backspace => {
                    value.pop();
                    Mode::Value { name, value }
                }
                KeyCode::Char('u') if ctrl => Mode::Value {
                    name,
                    value: String::new(),
                },
                KeyCode::Char(c) if !ctrl => {
                    self.message = None;
                    value.push(c);
                    Mode::Value { name, value }
                }
                _ => Mode::Value { name, value },
            },
            Mode::ConfirmRemove(name) => {
                if let KeyCode::Char('y' | 'Y') = key.code {
                    match backend.remove_secret(&name) {
                        Ok(_) => {
                            self.refresh(backend, &name);
                            self.message = Some((false, format!("removed {name}")));
                        }
                        Err(e) => self.message = Some((true, e)),
                    }
                }
                Mode::List
            }
        };
        true
    }

    fn set_mode(&mut self, mode: Mode) -> bool {
        self.mode = mode;
        true
    }

    /// Pasted text goes into the field being edited, never the chat input.
    pub fn on_paste(&mut self, text: &str) {
        let text = text.trim_end_matches(['\r', '\n']);
        match &mut self.mode {
            Mode::Name(name) => name.push_str(&text.trim().to_ascii_uppercase()),
            Mode::Value { value, .. } => value.push_str(text),
            Mode::List | Mode::ConfirmRemove(_) => {}
        }
    }
}
