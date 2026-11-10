//! Rendering.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{AgentLine, AgentState, App, Entry, Overlay, ToolState};
use crate::secret_prompt::SecretPrompt;
use crate::secrets::{Mode, SecretsPanel};
use crate::theme;
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
    if let Some(panel) = &app.outbox {
        draw_outbox(frame, panel, transcript);
    }
    if let Some(panel) = &app.mcp {
        crate::ui_mcp::draw_mcp(frame, panel, transcript);
    }
    if let Some(panel) = &app.settings {
        crate::ui_settings::draw_settings(frame, panel, transcript);
    }
    if let Some(panel) = &app.setup {
        crate::ui_setup::draw_setup(frame, panel, transcript);
    }
    if let Some(picker) = &app.picker {
        crate::ui_setup::draw_picker(frame, picker, transcript);
    }
    if let Some(o) = &mut app.overlay {
        draw_overlay(frame, o, app.expanded, transcript);
    }
}

/// A sub-agent's transcript (`/agent <label>`), over `area`.
fn draw_overlay(frame: &mut Frame<'_>, o: &mut Overlay, expanded: bool, area: Rect) {
    let block = Block::new()
        .borders(Borders::ALL)
        .title(o.title.clone())
        .border_style(theme::accent());
    let inner = block.inner(area).inner(ratatui::layout::Margin::new(1, 0));
    let width = inner.width.max(1) as usize;
    let mut lines = Vec::new();
    entry_lines(&o.entries, expanded, width, &mut lines);
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("(no messages yet)", theme::note())));
    }
    let height = inner.height as usize;
    o.scroll = o.scroll.min(lines.len().saturating_sub(height));
    let end = lines.len() - o.scroll;
    let start = end.saturating_sub(height);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

/// The `/secrets` panel, centred over `area`.
fn draw_secrets(frame: &mut Frame<'_>, panel: &SecretsPanel, area: Rect) {
    let dim = theme::muted();
    let key = theme::accent().add_modifier(Modifier::BOLD);
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
            (true, true) => ("env (overrides file)", theme::warn()),
            (true, false) => ("env", theme::ok()),
            (false, true) => ("set", theme::ok()),
            (false, false) => ("missing", theme::err()),
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
            Span::styled(format!("{state:8}"), color),
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
                theme::warn().add_modifier(Modifier::BOLD),
            )),
            vec![("y", "remove"), ("any other key", "cancel")],
        ),
    };
    body.push(prompt);
    body.push(match &panel.message {
        Some((true, m)) => Line::from(Span::styled(m.clone(), theme::err())),
        Some((false, m)) => Line::from(Span::styled(m.clone(), theme::ok())),
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
        .border_style(theme::accent());
    let inner = block.inner(popup).inner(ratatui::layout::Margin::new(1, 0));
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(body), inner);
}

/// The `/outbox` panel, centred over `area`.
fn draw_outbox(frame: &mut Frame<'_>, panel: &crate::outbox::OutboxPanel, area: Rect) {
    use crate::outbox::Mode;
    let dim = theme::muted();
    let key = theme::accent().add_modifier(Modifier::BOLD);
    let warn = theme::warn();
    let width = area.width.saturating_sub(4).min(100);
    let inner_width = width.saturating_sub(4) as usize;
    let mut body: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "Writes that waited for approval while nobody was there. Approving runs one once.",
            dim,
        )),
        Line::default(),
    ];
    if panel.items.is_empty() {
        body.push(Line::from(Span::styled("Nothing is waiting for you.", dim)));
    }
    let chrome = body.len() + 9;
    let room = (area.height as usize).saturating_sub(chrome).max(1);
    let first = (panel.selected + 1).saturating_sub(room);
    for (i, o) in panel.items.iter().enumerate().skip(first).take(room) {
        let selected = i == panel.selected;
        let line = format!("#{:<4} {}", o.id, o.headline());
        let line: String = line.chars().take(inner_width.saturating_sub(2)).collect();
        body.push(Line::from(vec![
            Span::styled(if selected { "› " } else { "  " }, key),
            Span::styled(
                line,
                if selected {
                    Style::new().add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                },
            ),
        ]));
    }
    if let Some(o) = panel.items.get(panel.selected) {
        body.push(Line::default());
        if let Some(t) = &o.taint {
            body.push(Line::from(Span::styled(
                format!("⚠ {}", tern_core::approval::taint_note(t)),
                warn,
            )));
        }
        if o.local_only {
            body.push(Line::from(Span::styled(
                "Runs a command on this computer outside the sandbox.",
                warn,
            )));
        }
        let args: String = o.input.to_string().chars().take(inner_width).collect();
        body.push(Line::from(Span::styled(args, dim)));
    }
    body.push(Line::default());
    let (prompt, help): (Line<'static>, Vec<(&str, &str)>) = match &panel.mode {
        Mode::List => (
            Line::default(),
            vec![
                ("a", "approve"),
                ("e", "edit and approve"),
                ("r", "reject"),
                ("↑↓", "move"),
                ("Esc", "close"),
            ],
        ),
        Mode::Edit(text) => (
            Line::from(vec![
                Span::styled("Arguments: ", key),
                Span::raw(text.clone()),
                Span::raw("▌"),
            ]),
            vec![("Enter", "approve with these"), ("Esc", "cancel")],
        ),
        Mode::Reject(note) => (
            Line::from(vec![
                Span::styled("Note for tern (optional): ", key),
                Span::raw(note.clone()),
                Span::raw("▌"),
            ]),
            vec![("Enter", "reject"), ("Esc", "cancel")],
        ),
    };
    body.push(prompt);
    body.push(match &panel.message {
        Some((true, m)) => Line::from(Span::styled(m.clone(), theme::err())),
        Some((false, m)) => Line::from(Span::styled(m.clone(), theme::ok())),
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
        .title(" outbox ")
        .border_style(theme::accent());
    let inner = block.inner(popup).inner(ratatui::layout::Margin::new(1, 0));
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(body).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
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
        out.push(Line::from(Span::styled(
            "tern — type a message and press Enter. /help for commands.",
            theme::note(),
        )));
    }
    entry_lines(&app.entries, app.expanded, width, &mut out);
    if let Some(p) = &app.approval {
        out.push(Line::default());
        let warn = theme::warn().add_modifier(Modifier::BOLD);
        let who = p
            .request
            .agent_label
            .as_deref()
            .map(|l| format!("[{l}] "))
            .unwrap_or_default();
        for l in wrap(&format!("{who}Allow {}?", p.request.headline()), width) {
            out.push(Line::from(Span::styled(l, warn)));
        }
        if let Some(note) = p.request.taint_note() {
            for l in wrap(&format!("⚠ {note}"), width) {
                out.push(Line::from(Span::styled(l, theme::warn())));
            }
        }
        if let Some(note) = &p.request.notice {
            for l in wrap(note, width) {
                out.push(Line::from(Span::styled(l, theme::accent())));
            }
        }
        let mut options = vec![Span::styled("[y]", warn), Span::raw(" yes once   ")];
        if let Some(pattern) = &p.request.pattern {
            options.push(Span::styled("[a]", warn));
            options.push(Span::raw(format!(
                " always ({} {pattern})   ",
                p.request.call.name
            )));
        }
        if let Some(grant) = &p.request.grant {
            options.push(Span::styled("[r]", warn));
            options.push(Span::raw(format!(" this run ({})   ", grant.label)));
            if !grant.run_only {
                options.push(Span::styled("[c]", warn));
                options.push(Span::raw(" this conversation   "));
                options.push(Span::styled("[a]", warn));
                options.push(Span::raw(" always   "));
            }
        }
        if let Some((n, tools)) = app.bulk_summary() {
            options.push(Span::styled("[g]", warn));
            options.push(Span::raw(format!(
                " allow all {n} ({})   ",
                tools.join(", ")
            )));
            options.push(Span::styled("[x]", warn));
            options.push(Span::raw(format!(" deny all {n}   ")));
        }
        if let Some(host) = &p.request.egress_host {
            options.push(Span::styled("[r]", warn));
            options.push(Span::raw(" all sites this run   "));
            options.push(Span::styled("[h]", warn));
            options.push(Span::raw(format!(" always {host}   ")));
        }
        options.extend([Span::styled("[n]", warn), Span::raw(" no")]);
        out.push(Line::from(options));
    }
    if let Some(p) = &app.secret_prompt {
        secret_prompt_lines(p, width, &mut out);
    }
    if let Some(p) = &app.auth_prompt {
        auth_lines(&p.request, app.auth_pick, width, &mut out);
    }
    out
}

/// A sign-in card: the choice between signing in and a key, or the page
/// to open (or code to enter), with where it came from.
fn auth_lines(
    req: &tern_core::approval::AuthPrompt,
    pick: usize,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    use tern_core::approval::AuthCard;
    let head = theme::accent().add_modifier(Modifier::BOLD);
    let key = theme::accent();
    let dim = theme::muted();
    out.push(Line::default());
    let notes = match &req.card {
        AuthCard::Choose {
            title,
            options,
            notes,
        } => {
            out.push(Line::from(Span::styled(format!("🔐 {title}"), head)));
            for (i, o) in options.iter().enumerate() {
                let label = if o.recommended {
                    format!("{} (recommended)", o.label)
                } else {
                    o.label.clone()
                };
                out.push(Line::from(vec![
                    Span::raw(if i == pick { " ▸ " } else { "   " }),
                    Span::styled(format!("[{}] ", i + 1), key),
                    Span::styled(label, if i == pick { head } else { key }),
                ]));
                for l in wrap(&o.detail, width.saturating_sub(7)) {
                    out.push(Line::from(vec![Span::raw("       "), Span::styled(l, dim)]));
                }
            }
            out.push(Line::from(vec![
                Span::styled("   ↑/↓ Enter", key),
                Span::raw(" choose   "),
                Span::styled("Esc", key),
                Span::raw(" cancel"),
            ]));
            notes
        }
        AuthCard::SignIn {
            title,
            url,
            user_code,
            verification_uri,
            device_available,
            notes,
        } => {
            out.push(Line::from(Span::styled(format!("🔐 {title}"), head)));
            if let (Some(code), Some(at)) = (user_code, verification_uri) {
                out.push(Line::from(vec![
                    Span::raw("   Enter the code "),
                    Span::styled(code.clone(), head),
                    Span::raw(" at "),
                    Span::styled(at.clone(), key),
                ]));
            }
            if let Some(u) = url {
                for l in wrap(u, width.saturating_sub(3)) {
                    out.push(Line::from(vec![Span::raw("   "), Span::styled(l, dim)]));
                }
                let mut keys = vec![
                    Span::styled("   [Enter]", key),
                    Span::raw(" open in your browser   "),
                ];
                if *device_available {
                    keys.push(Span::styled("[d]", key));
                    keys.push(Span::raw(" use a code instead   "));
                }
                keys.extend([Span::styled("[Esc]", key), Span::raw(" cancel")]);
                out.push(Line::from(keys));
            } else {
                out.push(Line::from(vec![
                    Span::styled("   [Esc]", key),
                    Span::raw(" cancel"),
                ]));
            }
            notes
        }
    };
    for n in notes {
        for l in wrap(n, width.saturating_sub(3)) {
            out.push(Line::from(vec![Span::raw("   "), Span::styled(l, dim)]));
        }
    }
}

/// The agent's request for secrets: who asks and why (in the agent's own
/// words, marked as such), what would read each secret, and masked fields.
fn secret_prompt_lines(p: &SecretPrompt, width: usize, out: &mut Vec<Line<'static>>) {
    let req = &p.pending.request;
    let head = theme::accent().add_modifier(Modifier::BOLD);
    let key = theme::accent();
    let dim = theme::muted();
    out.push(Line::default());
    let who = match &req.agent_label {
        Some(l) => format!("Sub-agent {l} is"),
        None => "The agent is".to_owned(),
    };
    let what = if req.asks.len() == 1 {
        "a secret"
    } else {
        "secrets"
    };
    out.push(Line::from(Span::styled(
        format!("🔑 {who} asking for {what}"),
        head,
    )));
    for l in wrap(
        &format!("The agent says: \"{}\"", req.reason),
        width.saturating_sub(3),
    ) {
        out.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(l, Style::new().italic()),
        ]));
    }
    for (i, (ask, value)) in req.asks.iter().zip(&p.values).enumerate() {
        let current = i == p.field;
        let state = match (ask.in_env, ask.in_file) {
            (true, _) => "set in the environment, which wins over a saved value",
            (false, true) => "saved in secrets.toml; this replaces it",
            _ => "not set",
        };
        let used = if ask.used_by.is_empty() {
            "nothing uses this secret yet".to_owned()
        } else {
            format!("used by {}", ask.used_by.join("; "))
        };
        out.push(Line::from(vec![
            Span::raw(if current { " ▸ " } else { "   " }),
            Span::styled(ask.name.clone(), if current { head } else { key }),
            Span::styled(format!("  {state}"), dim),
        ]));
        for l in wrap(&used, width.saturating_sub(5)) {
            out.push(Line::from(vec![Span::raw("     "), Span::styled(l, dim)]));
        }
        let shown = if !p.armed {
            Span::styled("press Enter to type or paste the value", dim)
        } else if value.is_empty() {
            Span::styled("type or paste the value", dim)
        } else {
            Span::raw(format!(
                "{}  ({} characters)",
                tern_core::secrets::keys::mask_key(value),
                value.chars().count()
            ))
        };
        let mut field = vec![Span::raw("     "), shown];
        if current && p.armed {
            field.push(Span::raw("▌"));
        }
        out.push(Line::from(field));
    }
    let next = if !p.armed {
        " start   "
    } else if p.field + 1 < p.values.len() {
        " next   "
    } else {
        " save   "
    };
    let mut keys = vec![Span::styled("   [Enter]", key), Span::styled(next, dim)];
    if p.values.len() > 1 {
        keys.extend([Span::styled("[Tab]", key), Span::styled(" field   ", dim)]);
    }
    keys.extend([
        Span::styled("[Esc]", key),
        Span::styled(" skip   ", dim),
        Span::styled("only tern sees the value", dim),
    ]);
    out.push(Line::from(keys));
}

/// `0:42`, `12:03`.
fn clock(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// `⑂ research-1  Running shell cargo test · 3 tools · 0:42`, finishing as
/// `✓ research-1  1:12 · 9 tools · 48k tok → first line of the report`.
fn agent_line(a: &AgentLine, width: usize) -> Line<'static> {
    let (icon, color) = match a.state {
        AgentState::Queued => ("⏸", theme::muted()),
        AgentState::Running => ("⑂", theme::accent()),
        AgentState::Question => ("?", theme::warn()),
        AgentState::Approval => ("!", theme::warn()),
        AgentState::Completed => ("✓", theme::ok()),
        AgentState::Failed => ("✗", theme::err()),
        AgentState::Cancelled => ("⊘", theme::muted()),
    };
    let tools = match a.tools {
        1 => "1 tool".to_owned(),
        n => format!("{n} tools"),
    };
    let rest = match a.elapsed {
        None => {
            let activity = match a.state {
                AgentState::Approval => "Waiting for approval",
                _ => a.activity.as_str(),
            };
            format!("{activity} · {tools} · {}", clock(a.started.elapsed()))
        }
        Some(e) => {
            let mut t = format!("{} · {tools}", clock(e));
            if a.tokens > 0 {
                t.push_str(&format!(" · {}k tok", a.tokens.div_ceil(1000)));
            }
            let first = a.result.lines().find(|l| !l.trim().is_empty());
            if let Some(f) = first {
                t.push_str(" → ");
                t.push_str(f.trim());
            }
            t
        }
    };
    let text: String = format!("{icon} {}  {rest}", a.label)
        .chars()
        .take(width.max(4))
        .collect();
    let label_end = icon.len() + 1 + a.label.len();
    let (head, tail) = text.split_at(label_end.min(text.len()));
    Line::from(vec![
        Span::styled(head.to_owned(), color.add_modifier(Modifier::BOLD)),
        Span::styled(tail.to_owned(), theme::muted()),
    ])
}

/// The lines of `entries` at `width`, appended to `out`.
fn entry_lines(entries: &[Entry], expanded: bool, width: usize, out: &mut Vec<Line<'static>>) {
    let dim = theme::note();
    let mut prev_agent = false;
    for entry in entries {
        // Consecutive agent lines stay together, like a list.
        let agent = matches!(entry, Entry::Agent(_));
        if !out.is_empty() && !(agent && prev_agent) {
            out.push(Line::default());
        }
        prev_agent = agent;
        match entry {
            Entry::User(text) => {
                for (i, l) in wrap(text, width.saturating_sub(2)).into_iter().enumerate() {
                    let prefix = if i == 0 { "› " } else { "  " };
                    out.push(Line::from(vec![
                        Span::styled(prefix, theme::accent().add_modifier(Modifier::BOLD)),
                        Span::styled(l, Style::new().add_modifier(Modifier::BOLD)),
                    ]));
                }
            }
            Entry::Assistant { text, streaming } => {
                let mut lines = wrap(text, width);
                if *streaming && let Some(last) = lines.last_mut() {
                    last.push('▌');
                }
                out.extend(crate::markdown::style_lines(lines));
            }
            Entry::Tool {
                summary,
                state,
                output,
                ..
            } => {
                let (icon, color) = match state {
                    ToolState::Running => ("⋯", theme::warn()),
                    ToolState::Ok => ("✓", theme::ok()),
                    ToolState::Failed => ("✗", theme::err()),
                    ToolState::Denied => ("⊘", theme::err()),
                    ToolState::Interrupted => ("!", theme::warn()),
                };
                let first = output.lines().next().unwrap_or_default();
                let mut head = format!("{icon} {summary}");
                if !expanded && !first.is_empty() {
                    head.push_str(" — ");
                    head.push_str(first);
                }
                let head: String = head.chars().take(width.max(4)).collect();
                out.push(Line::from(Span::styled(head, color)));
                if expanded && !output.is_empty() {
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
                if expanded {
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
                    .map(|l| Line::from(Span::styled(l, theme::err()))),
            ),
            Entry::Agent(a) => out.push(agent_line(a, width)),
        }
    }
}

fn draw_input(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    rows: &[String],
    cursor: (usize, usize),
    shown: usize,
) {
    let title = if app.secret_prompt.is_some() {
        " enter the secret above · Esc skips "
    } else if app.auth_prompt.is_some() {
        " answer the sign-in card above · Esc cancels "
    } else if app.approval.is_some() {
        " answer the approval above: y / a / n "
    } else if app.is_running() {
        " message (Enter stops the run and sends) "
    } else {
        " message "
    };
    let block = Block::new()
        .borders(Borders::TOP | Borders::BOTTOM)
        .title(title)
        .border_style(theme::muted())
        .title_style(Style::new().add_modifier(Modifier::BOLD));
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
    if app.approval.is_none()
        && app.secret_prompt.is_none()
        && app.auth_prompt.is_none()
        && app.secrets.is_none()
        && app.setup.is_none()
        && app.mcp.is_none()
        && app.picker.is_none()
        && app.overlay.is_none()
    {
        let x = inner.x + (cursor.1 as u16).min(inner.width.saturating_sub(1));
        let y = inner.y + (cursor.0 - first) as u16;
        frame.set_cursor_position(Position::new(x, y));
    }
}

fn draw_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let model = app.model.as_deref().unwrap_or("no model — /setup");
    let session = app
        .session
        .as_ref()
        .map(|s| s.title.clone().unwrap_or_else(|| s.id.clone()))
        .unwrap_or_else(|| "new session".into());
    let session: String = session.chars().take(40).collect();
    let agent = app.profile.as_ref().map_or("tern", |p| p.name.as_str());
    // Where the composer posts, and everything waiting on the user.
    let place = app
        .channel
        .as_ref()
        .map(|c| format!("#{} · ", c.slug))
        .unwrap_or_default();
    let waiting = if app.needs_you > 0 {
        format!(" · {} need you", app.needs_you)
    } else {
        String::new()
    };
    // Several agents: who is working or needs you, at a glance.
    let fleet = match crate::fleet_summary(&app.fleet, app.needs_you == 0) {
        f if f.is_empty() => String::new(),
        f => format!(" · {f}"),
    };
    let code = if app.code_auto {
        " · code auto (not fully sandboxed)"
    } else {
        ""
    };
    let left = format!(" {place}{agent} · {model} · {session}{waiting}{fleet}{code}");
    let right = if let Some(h) = &app.hint {
        h.clone()
    } else if app.secret_prompt.is_some() {
        "secret needed".into()
    } else if app.auth_prompt.is_some() {
        "sign-in needed".into()
    } else if app.approval.is_some() {
        "approval needed".into()
    } else if let Some(r) = &app.running {
        let secs = r.started.elapsed().as_secs();
        let frame_idx = (r.started.elapsed().as_millis() / 100) as usize % SPINNER.len();
        match app.live_agents() {
            0 => format!("{} running {secs}s · Ctrl+C to stop", SPINNER[frame_idx]),
            1 => format!(
                "{} waiting on 1 agent · {secs}s · Ctrl+C to stop",
                SPINNER[frame_idx]
            ),
            n => format!(
                "{} waiting on {n} agents · {secs}s · Ctrl+C to stop",
                SPINNER[frame_idx]
            ),
        }
    } else if let Some(u) = &app.last_usage {
        let cached = u
            .cache_hit_rate()
            .filter(|r| *r > 0.0)
            .map(|r| format!(" ({:.0}% cached)", r * 100.0))
            .unwrap_or_default();
        format!(
            "last turn {} in{cached} / {} out",
            u.total_input(),
            u.output_tokens
        )
    } else {
        "ready".into()
    };
    // Routine spend against the daily cap, when there is one.
    let right = match &app.spend {
        Some(spend) if app.hint.is_none() => format!("{right} · {spend}"),
        _ => right,
    };
    let pad =
        (area.width as usize).saturating_sub(left.chars().count() + right.chars().count() + 1);
    let line = Line::from(vec![
        Span::raw(left),
        Span::raw(" ".repeat(pad)),
        Span::raw(right),
        Span::raw(" "),
    ])
    .style(theme::status_bar());
    frame.render_widget(Paragraph::new(line), area);
}
