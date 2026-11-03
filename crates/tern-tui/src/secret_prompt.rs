//! The inline prompt for a `secret_request`: one masked field per secret.
//! Values go back to the runtime only; they never reach the transcript,
//! the input history or the model.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::PendingSecret;

pub struct SecretPrompt {
    pub pending: PendingSecret,
    /// The field being typed into.
    pub field: usize,
    pub values: Vec<String>,
    /// The prompt can open while the user is typing a chat message; until
    /// they press Enter to start, keys and pastes are ignored, so the rest
    /// of that message can't be saved as the secret.
    pub armed: bool,
}

/// How a key left the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Done {
    Save,
    Skip,
}

impl SecretPrompt {
    pub fn new(pending: PendingSecret) -> Self {
        let n = pending.request.asks.len();
        Self {
            pending,
            field: 0,
            values: vec![String::new(); n],
            armed: false,
        }
    }

    pub fn call_id(&self) -> Option<&str> {
        self.pending.request.call_id.as_deref()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Done> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = self.values.len().saturating_sub(1);
        if !self.armed {
            match key.code {
                KeyCode::Esc => return Some(Done::Skip),
                KeyCode::Enter => self.armed = true,
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Esc => return Some(Done::Skip),
            KeyCode::Enter if self.field < last => self.field += 1,
            KeyCode::Enter => return Some(Done::Save),
            KeyCode::Tab | KeyCode::Down => self.field = (self.field + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => self.field = self.field.saturating_sub(1),
            KeyCode::Backspace => {
                self.values[self.field].pop();
            }
            KeyCode::Char('u') if ctrl => self.values[self.field].clear(),
            KeyCode::Char(c) if !ctrl => self.values[self.field].push(c),
            _ => {}
        }
        None
    }

    /// A paste goes into the current field; a trailing newline from the
    /// clipboard is dropped rather than submitting.
    pub fn on_paste(&mut self, text: &str) {
        if !self.armed {
            return;
        }
        let text = text.trim_end_matches(['\r', '\n']);
        self.values[self.field].push_str(&text.replace(['\r', '\n'], ""));
    }

    /// Answer the request. Skipping, or leaving a field empty, skips it.
    pub fn finish(self, done: Done) {
        let values = match done {
            Done::Skip => Default::default(),
            Done::Save => self
                .pending
                .request
                .asks
                .iter()
                .zip(self.values)
                .filter(|(_, v)| !v.trim().is_empty())
                .map(|(a, v)| (a.name.clone(), v))
                .collect(),
        };
        let _ = self.pending.reply.send(values);
    }
}
