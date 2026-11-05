//! Drawing the `/setup` panel and the `/model` picker.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tern_core::config::ProviderKind;

use crate::models::{ModelList, ModelPicker, Row};
use crate::setup::{Form, MANAGE, SetupPanel, Step};
use crate::theme;
use tern_core::presets::PRESETS;

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn dim() -> Style {
    theme::muted()
}

fn key() -> Style {
    theme::accent().add_modifier(Modifier::BOLD)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn spinner() -> char {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    SPINNER[(ms / 100) as usize % SPINNER.len()]
}

fn choice(selected: bool, label: String, note: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(if selected { "› " } else { "  " }, key()),
        Span::styled(label, if selected { bold() } else { Style::new() }),
        Span::raw("  "),
        Span::styled(note.to_owned(), dim()),
    ])
}

fn field(label: &str, value: String, focused: bool, placeholder: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{label}: "),
        if focused { key() } else { dim() },
    )];
    if value.is_empty() && !placeholder.is_empty() {
        spans.push(Span::styled(placeholder.to_owned(), dim()));
    } else {
        spans.push(Span::raw(value));
    }
    if focused {
        spans.push(Span::raw("▌"));
    }
    Line::from(spans)
}

fn help(keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (k, what) in keys {
        spans.push(Span::styled(format!("[{k}]"), key()));
        spans.push(Span::styled(format!(" {what}   "), dim()));
    }
    Line::from(spans)
}

fn message(m: &Option<(bool, String)>) -> Line<'static> {
    match m {
        Some((true, m)) => Line::from(Span::styled(m.clone(), theme::warn())),
        Some((false, m)) => Line::from(Span::styled(m.clone(), theme::ok())),
        None => Line::default(),
    }
}

/// Draw `body` in a bordered box centred over `area`.
fn popup(frame: &mut Frame<'_>, area: Rect, title: &str, body: Vec<Line<'static>>) {
    let width = area.width.saturating_sub(4).min(88);
    let height = (body.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(theme::accent());
    let inner = block.inner(rect).inner(ratatui::layout::Margin::new(1, 0));
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(body).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

/// The model rows that fit in `room` lines, scrolled to the selection.
fn model_rows(list: &ModelList, room: usize) -> Vec<Line<'static>> {
    if list.loading && list.groups.is_empty() {
        return vec![Line::from(Span::styled(
            format!("{} Loading models…", spinner()),
            dim(),
        ))];
    }
    let rows = list.rows();
    let mut lines = Vec::new();
    let mut selected_line = 0;
    let mut choice_idx = 0;
    for row in &rows {
        match row {
            Row::Header { title, note } => {
                let mut spans = vec![Span::styled(
                    title.clone(),
                    dim().add_modifier(Modifier::BOLD),
                )];
                if let Some(n) = note {
                    spans.push(Span::styled(format!("  {n}"), theme::warn()));
                }
                lines.push(Line::from(spans));
            }
            Row::Model {
                id,
                name,
                context_window,
                current,
                ..
            } => {
                let selected = choice_idx == list.selected;
                if selected {
                    selected_line = lines.len();
                }
                let mut note = String::new();
                if let Some(n) = name.as_ref().filter(|n| *n != id) {
                    note.push_str(n);
                }
                if let Some(w) = context_window {
                    if !note.is_empty() {
                        note.push_str(" · ");
                    }
                    note.push_str(&format!("{}k context", w / 1000));
                }
                if *current {
                    note.push_str(if note.is_empty() {
                        "current"
                    } else {
                        " · current"
                    });
                }
                lines.push(choice(selected, id.clone(), &note));
                choice_idx += 1;
            }
            Row::Typed(text) => {
                let selected = choice_idx == list.selected;
                if selected {
                    selected_line = lines.len();
                }
                lines.push(choice(selected, format!("Use \"{text}\""), "as typed"));
                choice_idx += 1;
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No models listed. Type a model id.",
            dim(),
        )));
    }
    let room = room.max(3);
    let first = (selected_line + 1).saturating_sub(room);
    lines.into_iter().skip(first).take(room).collect()
}

fn filter_line(list: &ModelList) -> Line<'static> {
    field(
        "Filter",
        list.filter.clone(),
        true,
        "type to filter, or a model id",
    )
}

pub fn draw_picker(frame: &mut Frame<'_>, picker: &ModelPicker, area: Rect) {
    let room = (area.height as usize).saturating_sub(8);
    let mut body = vec![filter_line(&picker.list), Line::default()];
    body.extend(model_rows(&picker.list, room));
    body.push(Line::default());
    body.push(message(&picker.message));
    body.push(help(&[
        ("Enter", "use"),
        ("Ctrl+D", "use and make default"),
        ("Ctrl+R", "refresh"),
        ("Esc", "close"),
    ]));
    popup(frame, area, "model", body);
}

fn key_source(p: &tern_core::models::ProviderStatus) -> String {
    match (p.key_source.as_str(), &p.key_hint) {
        ("env", Some(h)) => format!("key {h} from environment"),
        ("file", Some(h)) => format!("key {h}"),
        ("missing", _) => "key missing".into(),
        _ => "no key".into(),
    }
}

fn kind_label(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "Anthropic",
        ProviderKind::OpenaiCompat => "OpenAI-compatible",
    }
}

pub fn draw_setup(frame: &mut Frame<'_>, panel: &SetupPanel, area: Rect) {
    let mut body: Vec<Line<'static>> = Vec::new();
    if let Some(intro) = &panel.intro {
        body.push(Line::from(Span::styled(intro.clone(), bold())));
        body.push(Line::default());
    }
    let keys: Vec<(&str, &str)> = match &panel.step {
        Step::List => {
            body.push(Line::from(Span::styled("Model providers", dim())));
            for (i, p) in panel.providers.iter().enumerate() {
                let mut note = format!("{} · {}", kind_label(p.kind), key_source(p));
                if p.is_default {
                    note.push_str(" · default");
                }
                if let Some(m) = &p.message {
                    note.push_str(&format!(" · {m}"));
                }
                body.push(choice(i == panel.selected, p.name.clone(), &note));
            }
            body.push(choice(
                panel.selected == panel.providers.len(),
                "+ Add a provider".into(),
                "",
            ));
            vec![
                ("Enter", "open"),
                ("a", "add"),
                ("d", "remove"),
                ("Esc", "close"),
            ]
        }
        Step::Manage { name, selected } => {
            body.push(Line::from(Span::styled(name.clone(), bold())));
            for (i, label) in MANAGE.iter().enumerate() {
                body.push(choice(i == *selected, (*label).into(), ""));
            }
            vec![("Enter", "choose"), ("Esc", "back")]
        }
        Step::ConfirmRemove(name) => {
            body.push(Line::from(Span::styled(
                format!("Remove {name}?"),
                theme::warn().add_modifier(Modifier::BOLD),
            )));
            body.push(Line::from(Span::styled(
                "Its API key is removed too, unless something else uses it.",
                dim(),
            )));
            vec![
                ("y", "remove"),
                ("k", "remove, keep the key"),
                ("any other key", "cancel"),
            ]
        }
        Step::Kind { selected } => {
            body.push(Line::from(Span::styled("Connect a provider", bold())));
            body.push(Line::from(Span::styled(
                "Pick where your models come from. You'll only need an API key next.",
                dim(),
            )));
            body.push(Line::default());
            for (i, p) in PRESETS.iter().enumerate() {
                body.push(choice(
                    i == *selected,
                    format!("{} {}", i + 1, p.label),
                    p.blurb,
                ));
            }
            vec![("Enter", "choose"), ("1-9", "pick"), ("Esc", "back")]
        }
        Step::Endpoint { form, field: f } => {
            body.push(Line::from(Span::styled("OpenAI-compatible server", bold())));
            body.push(Line::default());
            body.push(field(
                "Base URL",
                form.base_url.clone(),
                *f == 0,
                "https://api.openai.com/v1",
            ));
            body.push(field("Name", form.name.clone(), *f == 1, "(from the URL)"));
            body.push(Line::default());
            body.push(Line::from(Span::styled(
                "The address the server's API lives at. It usually ends in /v1.",
                dim(),
            )));
            vec![("Enter", "next"), ("Tab", "switch field"), ("Esc", "back")]
        }
        Step::Key(form) | Step::Verifying(form) => {
            key_step(&mut body, form, matches!(panel.step, Step::Verifying(_)));
            if matches!(panel.step, Step::Verifying(_)) {
                vec![]
            } else {
                vec![
                    ("Enter", "test connection"),
                    ("Ctrl+E", "use key from environment"),
                    ("Ctrl+U", "clear"),
                    ("Esc", "back"),
                ]
            }
        }
        Step::Model {
            form,
            list,
            make_default,
            ..
        }
        | Step::Saving {
            form,
            list,
            make_default,
            ..
        } => {
            let saving = matches!(panel.step, Step::Saving { .. });
            let default_only = matches!(
                panel.step,
                Step::Model {
                    default_only: true,
                    ..
                }
            );
            let title = if default_only {
                format!("Default model for {}", form.label())
            } else {
                format!("Choose a model for {}", form.label())
            };
            body.push(Line::from(Span::styled(title, bold())));
            body.push(filter_line(list));
            body.push(Line::default());
            let room = (area.height as usize).saturating_sub(body.len() + 8);
            body.extend(model_rows(list, room));
            body.push(Line::default());
            if !default_only {
                body.push(Line::from(vec![
                    Span::styled("Make it the default: ", dim()),
                    Span::styled(if *make_default { "yes" } else { "no" }, bold()),
                ]));
            }
            if saving {
                body.push(Line::from(Span::styled(
                    format!("{} Saving…", spinner()),
                    dim(),
                )));
                vec![]
            } else if default_only {
                vec![("Enter", "use"), ("Esc", "back")]
            } else {
                vec![
                    ("Enter", "save"),
                    ("Tab", "toggle default"),
                    ("Esc", "back"),
                ]
            }
        }
    };
    body.push(Line::default());
    body.push(message(&panel.message));
    body.push(help(&keys));
    popup(frame, area, "setup", body);
}

fn key_step(body: &mut Vec<Line<'static>>, form: &Form, verifying: bool) {
    let what = match (&form.preset, &form.existing) {
        (_, Some(name)) => format!("New API key for {name}"),
        (Some(p), None) if p.needs_key => format!("Paste your {} API key", p.label),
        (Some(p), None) if p.local => format!("{} runs here, so there's no key", p.label),
        _ => "API key".into(),
    };
    body.push(Line::from(Span::styled(what, bold())));
    if form.existing.is_none() {
        match form.preset {
            Some(p) if p.id != "other" => {
                body.push(Line::from(Span::styled(p.key_hint, dim())));
                if let Some(url) = p.key_url {
                    body.push(Line::from(vec![
                        Span::styled("Get one at ", dim()),
                        Span::styled(url, theme::link()),
                    ]));
                }
                body.push(Line::from(Span::styled(
                    format!("Talks to {}", form.base_url),
                    dim(),
                )));
            }
            _ => body.push(Line::from(Span::styled(
                format!(
                    "For {}. Leave it blank if the server doesn't need one.",
                    form.base_url
                ),
                dim(),
            ))),
        }
    }
    body.push(Line::default());
    let masked = SetupPanel::masked(form);
    body.push(field("Key", masked, !verifying, "paste it here"));
    if verifying {
        body.push(Line::default());
        body.push(Line::from(Span::styled(
            format!("{} Checking with {}…", spinner(), form.label()),
            dim(),
        )));
    }
}
