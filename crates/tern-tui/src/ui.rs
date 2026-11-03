//! Rendering.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, Entry, ToolState};
use crate::secrets::{Mode, SecretsPanel};
use crate::wrap::wrap;

const MAX_INPUT_ROWS: usize = 8;
const MAX_EXPANDED_OUTPUT_LINES: usize = 40;
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let input_width = area.width.saturating_sub(2).max(1) as usize;
    let (rows, cursor) = app.input.layout(input_width);
    let input_rows = rows.len().clamp(1, MAX_INPUT_ROWS);
    let [transcript, input, status] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(input_rows as u16 + 2),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_transcript(frame, app, transcript);
    draw_input(frame, app, input, &rows, cursor, input_rows);
    draw_status(frame, app, status);
    if let Some(panel) = &app.secrets {
        draw_secrets(frame, panel, transcript);
    }
}

/// The `/secrets` panel, centred over `area`.
fn draw_secrets(frame: &mut Frame<'_>, panel: &SecretsPanel, area: Rect) {
    let dim = Style::new().fg(Color::DarkGray);
    let key = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let width = area.width.saturating_sub(4).min(80);
    let inner_width = width.saturating_sub(4) as usize;
    let name_width = panel
        .items
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(12, 32);

    let mut body: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "Stored in secrets.toml (mode 0600). Environment variables take precedence.",
            dim,
        )),
        Line::default(),
    ];
    if panel.items.is_empty() {
        body.push(Line::from(Span::styled(
            "No secrets yet, and nothing in config refers to one.",
            dim,
        )));
    }
    // Rows of chrome around the list: help, blank, prompt, message, borders.
    let chrome = body.len() + 7;
    let room = (area.height as usize).saturating_sub(chrome).max(1);
    let first = (panel.selected + 1).saturating_sub(room);
    for (i, s) in panel.items.iter().enumerate().skip(first).take(room) {
        let selected = i == panel.selected && panel.mode == Mode::List;
        let (state, color) = match (s.in_env, s.in_file) {
            (true, true) => ("env (overrides file)", Color::Yellow),
            (true, false) => ("env", Color::Green),
            (false, true) => ("set", Color::Green),
            (false, false) => ("missing", Color::Red),
        };
        let used = if s.used_by.is_empty() {
            "unused".to_owned()
        } else {
            s.used_by.join(", ")
        };
        let name = format!("{:name_width$}", s.name);
        let mut line = Line::from(vec![
            Span::styled(if selected { "› " } else { "  " }, key),
            Span::styled(
                name,
                if selected {
                    Style::new().add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                },
            ),
            Span::raw("  "),
            Span::styled(format!("{state:8}"), Style::new().fg(color)),
            Span::raw("  "),
            Span::styled(used, dim),
        ]);
        if line.width() > inner_width {
            let text: String = line
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .chars()
                .take(inner_width)
                .collect();
            line = Line::from(text);
        }
        body.push(line);
    }
    body.push(Line::default());

    let (prompt, help): (Line<'static>, Vec<(&str, &str)>) = match &panel.mode {
        Mode::List => (
            Line::default(),
            vec![
                ("Enter", "set"),
                ("n", "new"),
                ("d", "remove"),
                ("↑↓", "move"),
                ("Esc", "close"),
            ],
        ),
        Mode::Name(name) => (
            Line::from(vec![
                Span::styled("Name: ", key),
                Span::raw(name.clone()),
                Span::raw("▌"),
            ]),
            vec![("Enter", "next"), ("Esc", "cancel")],
        ),
        Mode::Value { name, value } => {
            let n = value.chars().count();
            let dots = "•".repeat(n.min(24));
            let count = if n == 0 {
                "type or paste the value".to_owned()
            } else {
                format!("{n} characters")
            };
            (
                Line::from(vec![
                    Span::styled(format!("New value for {name}: "), key),
                    Span::raw(dots),
                    Span::raw("▌  "),
                    Span::styled(count, dim),
                ]),
                vec![("Enter", "save"), ("Ctrl+U", "clear"), ("Esc", "cancel")],
            )
        }
        Mode::ConfirmRemove(name) => (
            Line::from(Span::styled(
                format!("Remove {name} from secrets.toml?"),
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )),
            vec![("y", "remove"), ("any other key", "cancel")],
        ),
    };
    body.push(prompt);
    body.push(match &panel.message {
        Some((true, m)) => Line::from(Span::styled(m.clone(), Style::new().fg(Color::Red))),
        Some((false, m)) => Line::from(Span::styled(m.clone(), Style::new().fg(Color::Green))),
        None => Line::default(),
    });
    let mut keys = Vec::new();
    for (k, what) in help {
        keys.push(Span::styled(format!("[{k}]"), key));
        keys.push(Span::styled(format!(" {what}   "), dim));
    }
    body.push(Line::from(keys));

    let height = (body.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .title(" secrets ")
        .border_style(Style::new().fg(Color::Cyan));
    let inner = block.inner(popup).inner(ratatui::layout::Margin::new(1, 0));
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(body), inner);
}

fn draw_transcript(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let width = area.width.saturating_sub(1).max(1) as usize;
    let lines = transcript_lines(app, width);
    let height = area.height as usize;
    let max_scroll = lines.len().saturating_sub(height);
    app.scroll = app.scroll.min(max_scroll);
    let end = lines.len() - app.scroll;
    let start = end.saturating_sub(height);
    let visible: Vec<Line<'static>> = lines[start..end].to_vec();
    frame.render_widget(Paragraph::new(visible), area);
}

/// Every transcript line at `width`, oldest first.
pub fn transcript_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    if app.entries.is_empty() {
        out.push(Line::from(
            "tern — type a message and press Enter. /help for commands.".dark_gray(),
        ));
    }
    let dim = Style::new().fg(Color::DarkGray);
    for entry in &app.entries {
        if !out.is_empty() {
            out.push(Line::default());
        }
        match entry {
            Entry::User(text) => {
                for (i, l) in wrap(text, width.saturating_sub(2)).into_iter().enumerate() {
                    let prefix = if i == 0 { "› " } else { "  " };
                    out.push(Line::from(vec![
                        Span::styled(
                            prefix,
                            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(l, Style::new().add_modifier(Modifier::BOLD)),
                    ]));
                }
            }
            Entry::Assistant { text, streaming } => {
                let mut lines = wrap(text, width);
                if *streaming && let Some(last) = lines.last_mut() {
                    last.push('▌');
                }
                out.extend(lines.into_iter().map(Line::from));
            }
            Entry::Tool {
                summary,
                state,
                output,
                ..
            } => {
                let (icon, color) = match state {
                    ToolState::Running => ("⋯", Color::Yellow),
                    ToolState::Ok => ("✓", Color::Green),
                    ToolState::Failed => ("✗", Color::Red),
                    ToolState::Denied => ("⊘", Color::Red),
                    ToolState::Interrupted => ("!", Color::Yellow),
                };
                let first = output.lines().next().unwrap_or_default();
                let mut head = format!("{icon} {summary}");
                if !app.expanded && !first.is_empty() {
                    head.push_str(" — ");
                    head.push_str(first);
                }
                let head: String = head.chars().take(width.max(4)).collect();
                out.push(Line::from(Span::styled(head, Style::new().fg(color))));
                if app.expanded && !output.is_empty() {
                    for (n, l) in wrap(output, width.saturating_sub(4))
                        .into_iter()
                        .enumerate()
                    {
                        if n == MAX_EXPANDED_OUTPUT_LINES {
                            out.push(Line::from(Span::styled("  │ …", dim)));
                            break;
                        }
                        out.push(Line::from(Span::styled(format!("  │ {l}"), dim)));
                    }
                }
            }
            Entry::Summary(text) => {
                out.push(Line::from(Span::styled(
                    "(earlier messages were summarized; Ctrl+O to show)",
                    dim.add_modifier(Modifier::ITALIC),
                )));
                if app.expanded {
                    out.extend(
                        wrap(text, width)
                            .into_iter()
                            .map(|l| Line::from(Span::styled(l, dim))),
                    );
                }
            }
            Entry::Notice(text) => out.extend(
                wrap(text, width)
                    .into_iter()
                    .map(|l| Line::from(Span::styled(l, dim))),
            ),
            Entry::Error(text) => out.extend(
                wrap(text, width)
                    .into_iter()
                    .map(|l| Line::from(Span::styled(l, Style::new().fg(Color::Red)))),
            ),
        }
    }
    if let Some(p) = &app.approval {
        out.push(Line::default());
        let warn = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
        for l in wrap(&format!("Allow {}?", p.request.summary), width) {
            out.push(Line::from(Span::styled(l, warn)));
        }
        let mut options = vec![Span::styled("[y]", warn), Span::raw(" yes once   ")];
        if let Some(pattern) = &p.request.pattern {
            options.push(Span::styled("[a]", warn));
            options.push(Span::raw(format!(
                " always ({} {pattern})   ",
                p.request.call.name
            )));
        }
        options.extend([Span::styled("[n]", warn), Span::raw(" no")]);
        out.push(Line::from(options));
    }
    out
}

fn draw_input(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    rows: &[String],
    cursor: (usize, usize),
    shown: usize,
) {
    let title = if app.approval.is_some() {
        " answer the approval above: y / a / n "
    } else if app.is_running() {
        " message (Enter stops the run and sends) "
    } else {
        " message "
    };
    let block = Block::new()
        .borders(Borders::TOP | Borders::BOTTOM)
        .title(title)
        .dark_gray();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Keep the cursor row visible.
    let first = (cursor.0 + 1).saturating_sub(shown);
    let visible: Vec<Line<'static>> = rows
        .iter()
        .skip(first)
        .take(shown)
        .map(|r| Line::from(r.clone()))
        .collect();
    frame.render_widget(Paragraph::new(visible), inner);
    if app.approval.is_none() && app.secrets.is_none() {
        let x = inner.x + (cursor.1 as u16).min(inner.width.saturating_sub(1));
        let y = inner.y + (cursor.0 - first) as u16;
        frame.set_cursor_position(Position::new(x, y));
    }
}

fn draw_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let model = app.model.as_deref().unwrap_or("no model");
    let session = app
        .session
        .as_ref()
        .map(|s| s.title.clone().unwrap_or_else(|| s.id.clone()))
        .unwrap_or_else(|| "new session".into());
    let session: String = session.chars().take(40).collect();
    let left = format!(" tern · {model} · {session}");
    let right = if let Some(h) = &app.hint {
        h.clone()
    } else if app.approval.is_some() {
        "approval needed".into()
    } else if let Some(r) = &app.running {
        let secs = r.started.elapsed().as_secs();
        let frame_idx = (r.started.elapsed().as_millis() / 100) as usize % SPINNER.len();
        format!("{} running {secs}s · Ctrl+C to stop", SPINNER[frame_idx])
    } else if let Some(u) = &app.last_usage {
        format!(
            "last turn {} in / {} out",
            u.input_tokens + u.cache_read_tokens + u.cache_write_tokens,
            u.output_tokens
        )
    } else {
        "ready".into()
    };
    let pad =
        (area.width as usize).saturating_sub(left.chars().count() + right.chars().count() + 1);
    let line = Line::from(vec![
        Span::raw(left),
        Span::raw(" ".repeat(pad)),
        Span::raw(right),
        Span::raw(" "),
    ])
    .style(Style::new().bg(Color::DarkGray).fg(Color::White));
    frame.render_widget(Paragraph::new(line), area);
}
