//! Drawing the `/settings` screen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::settings::{Focus, Mode, Section, SettingsPanel};
use crate::theme;

fn clip(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn draw_settings(frame: &mut Frame<'_>, panel: &SettingsPanel, area: Rect) {
    let dim = theme::muted();
    let key = theme::accent().add_modifier(Modifier::BOLD);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let width = area.width.saturating_sub(2).min(112);
    let height = area.height.min(30);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .title(" settings ")
        .border_style(theme::accent());
    let inner = block.inner(popup).inner(Margin::new(1, 0));
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let searching = !panel.query.is_empty() || panel.mode == Mode::Search;
    let detail = panel.detail();
    let footer_rows = (detail.len() as u16).min(5) + 3;
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(footer_rows),
    ])
    .areas(inner);

    // Header: the section's intro, or the search, and the restart banner.
    let first = if searching {
        Line::from(vec![
            Span::styled("search: ", key),
            Span::raw(panel.query.clone()),
            Span::styled(
                if panel.mode == Mode::Search {
                    "▏"
                } else {
                    ""
                },
                key,
            ),
        ])
    } else {
        Line::from(Span::styled(panel.section.intro(), dim))
    };
    let second = match panel.pending() {
        0 => Line::default(),
        1 => Line::from(Span::styled(
            "↻ 1 saved change applies when tern restarts",
            theme::warn(),
        )),
        n => Line::from(Span::styled(
            format!("↻ {n} saved changes apply when tern restarts"),
            theme::warn(),
        )),
    };
    frame.render_widget(Paragraph::new(vec![first, second]), head);

    // Body: sections on the left, rows on the right.
    let [nav, rows] = Layout::horizontal([Constraint::Length(26), Constraint::Min(20)]).areas(body);
    let nav_lines: Vec<Line<'static>> = Section::ALL
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let here = !searching && *s == panel.section;
            let marker = if here && panel.focus == Focus::Sections {
                "› "
            } else if here {
                "• "
            } else {
                "  "
            };
            Line::from(vec![
                Span::styled(marker, key),
                Span::styled(format!("{} ", i + 1), dim),
                Span::styled(s.title(), if here { bold } else { Style::new() }),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(nav_lines), nav);

    let lines = panel.lines();
    let rows = rows.inner(Margin::new(1, 0));
    let room = rows.height as usize;
    let first = (panel.selected + 1).saturating_sub(room);
    let name_width = lines
        .iter()
        .map(|l| l.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(14, 34);
    let mut out: Vec<Line<'static>> = Vec::new();
    if lines.is_empty() {
        out.push(Line::from(Span::styled(
            if searching {
                "No setting matches."
            } else {
                "Nothing here."
            },
            dim,
        )));
    }
    for (i, l) in lines.iter().enumerate().skip(first).take(room) {
        let selected = i == panel.selected && panel.focus == Focus::Rows;
        let value_width = (rows.width as usize).saturating_sub(name_width + 7);
        let value_style = if l.risky {
            theme::warn()
        } else if l.action {
            theme::accent()
        } else {
            Style::new()
        };
        let mut spans = vec![
            Span::styled(if selected { "› " } else { "  " }, key),
            Span::styled(
                format!("{:name_width$}", clip(&l.name, name_width)),
                if selected { bold } else { Style::new() },
            ),
            Span::raw("  "),
            Span::styled(clip(&l.value, value_width), value_style),
        ];
        if !l.marks.is_empty() {
            spans.push(Span::styled(format!(" {}", l.marks), theme::warn()));
        }
        if l.risky {
            spans.push(Span::styled(" !", theme::warn()));
        }
        out.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(out), rows);

    // Footer: what the row is, the prompt or message, the keys.
    let mut f: Vec<Line<'static>> = detail
        .iter()
        .take(5)
        .map(|d| Line::from(Span::styled(clip(d, inner.width as usize), dim)))
        .collect();
    while f.len() < footer_rows as usize - 3 {
        f.push(Line::default());
    }
    f.push(match (&panel.mode, &panel.message) {
        (Mode::Edit { key: k, buf }, m) => {
            let mut spans = vec![
                Span::styled(format!("{k} = "), key),
                Span::raw(buf.clone()),
                Span::styled("▏", key),
            ];
            if let Some((true, m)) = m {
                spans.push(Span::styled(format!("   {m}"), theme::err()));
            }
            Line::from(spans)
        }
        (_, Some((true, m))) => Line::from(Span::styled(m.clone(), theme::err())),
        (_, Some((false, m))) => Line::from(Span::styled(m.clone(), theme::ok())),
        _ => Line::default(),
    });
    let help: &[(&str, &str)] = match panel.mode {
        Mode::Edit { .. } => &[("Enter", "save"), ("Esc", "cancel")],
        Mode::Search => &[("Enter", "keep"), ("Esc", "clear")],
        Mode::Browse => &[
            ("↑↓", "move"),
            ("Tab", "sections/rows"),
            ("Enter", "change/open"),
            ("←→", "choices"),
            ("r", "reset"),
            ("/", "search"),
            ("Esc", "close"),
        ],
    };
    let mut keys = Vec::new();
    for (k, what) in help {
        keys.push(Span::styled(format!("[{k}]"), key));
        keys.push(Span::styled(format!(" {what}   "), dim));
    }
    f.push(Line::from(keys));
    f.insert(0, Line::default());
    f.truncate(footer_rows as usize);
    frame.render_widget(Paragraph::new(f), foot);
}
