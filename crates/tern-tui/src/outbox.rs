//! The `/outbox` panel: writes that waited for approval while nobody was
//! there (docs/personal-agent-tooling-spec.md §5.2). Approve, edit the
//! arguments and approve, or reject with a note.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use tern_core::outbox::OutboxItem;

use crate::Backend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    List,
    /// Editing the selected item's arguments (JSON) before approving.
    Edit(String),
    /// Typing a note for rejecting the selected item.
    Reject(String),
}

pub struct OutboxPanel {
    pub items: Vec<OutboxItem>,
    pub selected: usize,
    pub mode: Mode,
    /// Feedback for the last action: `(is_error, text)`.
    pub message: Option<(bool, String)>,
}

/// What a decision sends to the backend.
pub struct Decide {
    pub approve: bool,
    pub input: Option<Value>,
    pub note: Option<String>,
}

impl OutboxPanel {
    pub fn open(backend: &dyn Backend) -> Result<Self, String> {
        Ok(Self {
            items: backend.outbox()?,
            selected: 0,
            mode: Mode::List,
            message: None,
        })
    }

    fn refresh(&mut self, backend: &dyn Backend) {
        match backend.outbox() {
            Ok(items) => {
                self.items = items;
                self.selected = self.selected.min(self.items.len().saturating_sub(1));
            }
            Err(e) => self.message = Some((true, e)),
        }
    }

    fn decide(&mut self, backend: &dyn Backend, d: Decide) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        let id = item.id;
        let approve = d.approve;
        match backend.decide_outbox(id, d) {
            Ok(_) => {
                self.refresh(backend);
                let what = if approve {
                    "approved; it runs now"
                } else {
                    "rejected"
                };
                self.message = Some((false, format!("#{id} {what}")));
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
                    KeyCode::Char('a' | 'y') => self.decide(
                        backend,
                        Decide {
                            approve: true,
                            input: None,
                            note: None,
                        },
                    ),
                    KeyCode::Char('e') | KeyCode::Enter => {
                        if let Some(o) = self.items.get(self.selected) {
                            return self.set_mode(Mode::Edit(o.input.to_string()));
                        }
                    }
                    KeyCode::Char('r' | 'n') => {
                        if !self.items.is_empty() {
                            return self.set_mode(Mode::Reject(String::new()));
                        }
                    }
                    KeyCode::Char('g') => self.refresh(backend),
                    _ => {}
                }
                Mode::List
            }
            Mode::Edit(mut text) => match key.code {
                KeyCode::Esc => Mode::List,
                KeyCode::Enter => match serde_json::from_str::<Value>(&text) {
                    Ok(v @ Value::Object(_)) => {
                        self.decide(
                            backend,
                            Decide {
                                approve: true,
                                input: Some(v),
                                note: None,
                            },
                        );
                        Mode::List
                    }
                    _ => {
                        self.message = Some((true, "the arguments must be a JSON object".into()));
                        Mode::Edit(text)
                    }
                },
                KeyCode::Backspace => {
                    text.pop();
                    Mode::Edit(text)
                }
                KeyCode::Char('u') if ctrl => Mode::Edit(String::new()),
                KeyCode::Char(c) if !ctrl => {
                    self.message = None;
                    text.push(c);
                    Mode::Edit(text)
                }
                _ => Mode::Edit(text),
            },
            Mode::Reject(mut note) => match key.code {
                KeyCode::Esc => Mode::List,
                KeyCode::Enter => {
                    let note = Some(note.trim().to_owned()).filter(|n| !n.is_empty());
                    self.decide(
                        backend,
                        Decide {
                            approve: false,
                            input: None,
                            note,
                        },
                    );
                    Mode::List
                }
                KeyCode::Backspace => {
                    note.pop();
                    Mode::Reject(note)
                }
                KeyCode::Char('u') if ctrl => Mode::Reject(String::new()),
                KeyCode::Char(c) if !ctrl => {
                    note.push(c);
                    Mode::Reject(note)
                }
                _ => Mode::Reject(note),
            },
        };
        true
    }

    fn set_mode(&mut self, mode: Mode) -> bool {
        self.mode = mode;
        true
    }

    /// Pasted text goes into the field being edited.
    pub fn on_paste(&mut self, text: &str) {
        let text = text.trim_end_matches(['\r', '\n']);
        match &mut self.mode {
            Mode::Edit(s) | Mode::Reject(s) => s.push_str(text),
            Mode::List => {}
        }
    }
}
