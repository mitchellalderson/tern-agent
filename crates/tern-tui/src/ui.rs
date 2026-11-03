//! Rendering.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Entry, ToolState};
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
        out.push(Line::from(vec![
            Span::styled("[y]", warn),
            Span::raw(" yes once   "),
            Span::styled("[a]", warn),
            Span::raw(format!(
                " always ({} {})   ",
                p.request.call.name, p.request.pattern
            )),
            Span::styled("[n]", warn),
            Span::raw(" no"),
        ]));
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
    if app.approval.is_none() {
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
