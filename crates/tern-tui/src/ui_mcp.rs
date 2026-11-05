//! Rendering the `/mcp` panel.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tern_core::config::McpTransport;
use tern_core::mcp::{McpErrorKind, McpState, McpStatus};

use crate::mcp::{Form, McpPanel, Mode, Step, describe_state};
use crate::theme;

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
        .map(|d| d.as_millis())
        .unwrap_or(0);
    SPINNER[(ms / 100) as usize % SPINNER.len()]
}

fn dot(s: &McpStatus) -> Span<'static> {
    match s.state {
        McpState::Running { .. } => Span::styled("● ", theme::ok()),
        McpState::Disabled => Span::styled("○ ", dim()),
        McpState::Unavailable { .. } => Span::styled("✕ ", theme::err()),
        McpState::Connecting => Span::styled("◌ ", theme::warn()),
        McpState::NotStarted => Span::styled("· ", dim()),
    }
}

fn transport(t: McpTransport) -> &'static str {
    match t {
        McpTransport::Stdio => "stdio",
        McpTransport::Http => "http ",
    }
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(width.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// The panel, centred over `area`.
pub fn draw_mcp(frame: &mut Frame<'_>, panel: &McpPanel, area: Rect) {
    let width = area.width.saturating_sub(4).min(96);
    let inner_width = width.saturating_sub(4) as usize;
    let room = (area.height as usize).saturating_sub(8).max(3);
    let now = chrono::Utc::now().timestamp();

    let (title, mut body, help): (String, Vec<Line<'static>>, Vec<(&str, &str)>) = match &panel.mode
    {
        Mode::List | Mode::ConfirmRemove { .. } => {
            let mut body = Vec::new();
            if panel.items.is_empty() {
                body.push(Line::from(Span::styled(
                    "No MCP servers yet. Press a to add one, or p to paste a config from a \
                     server's README.",
                    dim(),
                )));
            }
            let name_w = panel
                .items
                .iter()
                .map(|s| s.name.chars().count())
                .max()
                .unwrap_or(0)
                .clamp(8, 24);
            let first = (panel.selected + 1).saturating_sub(room);
            for (i, s) in panel.items.iter().enumerate().skip(first).take(room) {
                let selected = i == panel.selected;
                let state = describe_state(s, now);
                let used = 2 + 2 + name_w + 2 + 5 + 2;
                body.push(Line::from(vec![
                    Span::styled(if selected { "› " } else { "  " }, key()),
                    dot(s),
                    Span::styled(
                        format!("{:name_w$}", s.name),
                        if selected { bold() } else { Style::new() },
                    ),
                    Span::raw("  "),
                    Span::styled(transport(s.config.transport), dim()),
                    Span::raw("  "),
                    Span::raw(truncate(&state, inner_width.saturating_sub(used))),
                ]));
            }
            let help = match &panel.mode {
                Mode::ConfirmRemove { name } => {
                    body.push(Line::default());
                    body.push(Line::from(Span::styled(
                        format!("Remove {name}?"),
                        theme::warn().add_modifier(Modifier::BOLD),
                    )));
                    vec![
                        ("y", "remove"),
                        ("x", "remove and its unused secrets"),
                        ("any other key", "cancel"),
                    ]
                }
                _ => vec![
                    ("Enter", "details"),
                    ("a", "add"),
                    ("p", "paste config"),
                    ("e", "edit"),
                    ("Space", "on/off"),
                    ("r", "restart"),
                    ("d", "remove"),
                    ("Esc", "close"),
                ],
            };
            (" MCP servers ".into(), body, help)
        }
        Mode::Detail { name, tool, logs } => {
            let body = match panel.status(name) {
                Some(s) => detail_lines(s, *tool, *logs, inner_width, room, now),
                None => vec![Line::from(Span::styled("This server is gone.", dim()))],
            };
            (
                format!(" MCP · {name} "),
                body,
                vec![
                    ("Space", "show/hide tool"),
                    ("l", "logs"),
                    ("e", "edit"),
                    ("r", "restart"),
                    ("d", "remove"),
                    ("Esc", "back"),
                ],
            )
        }
        Mode::Paste(text) => {
            let mut body = vec![
                Line::from(Span::styled(
                    "Paste the mcpServers JSON from a server's README (Claude Desktop, Cursor and \
                     VS Code formats all work).",
                    dim(),
                )),
                Line::default(),
            ];
            let lines: Vec<&str> = text.lines().collect();
            if lines.is_empty() {
                body.push(Line::from(vec![Span::raw("▌")]));
            }
            let first = lines.len().saturating_sub(room);
            for l in &lines[first..] {
                body.push(Line::from(truncate(l, inner_width)));
            }
            (
                " MCP · paste config ".into(),
                body,
                vec![("Enter", "read it"), ("Ctrl+U", "clear"), ("Esc", "cancel")],
            )
        }
        Mode::Form(form) => form_view(form, inner_width, room),
    };

    body.push(Line::default());
    body.push(match &panel.message {
        Some((true, m)) => Line::from(Span::styled(truncate(m, inner_width * 2), theme::err())),
        Some((false, m)) => Line::from(Span::styled(truncate(m, inner_width * 2), theme::ok())),
        None => Line::default(),
    });
    let mut keys = Vec::new();
    for (k, what) in help {
        keys.push(Span::styled(format!("[{k}]"), key()));
        keys.push(Span::styled(format!(" {what}  "), dim()));
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
        .title(title)
        .border_style(theme::accent());
    let inner = block.inner(popup).inner(ratatui::layout::Margin::new(1, 0));
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(body).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

fn detail_lines(
    s: &McpStatus,
    tool: usize,
    logs: bool,
    width: usize,
    room: usize,
    now: i64,
) -> Vec<Line<'static>> {
    let c = &s.config;
    let mut out = vec![Line::from(vec![dot(s), Span::raw(describe_state(s, now))])];
    let mut row = |label: &str, value: String| {
        out.push(Line::from(vec![
            Span::styled(format!("{label:12}"), dim()),
            Span::raw(truncate(&value, width.saturating_sub(12))),
        ]));
    };
    if let Some(info) = &s.server_info {
        row("server", info.clone());
    }
    match c.transport {
        McpTransport::Stdio => {
            let mut cmd = c.command.clone().unwrap_or_default();
            for a in &c.args {
                cmd.push(' ');
                cmd.push_str(&crate::mcp::quote(a));
            }
            row("command", cmd);
            if let Some(cwd) = &c.cwd {
                row("directory", cwd.clone());
            }
            if !c.env.is_empty() {
                row(
                    "env",
                    c.env
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
        McpTransport::Http => row("url", c.url.clone().unwrap_or_default()),
    }
    let secrets: Vec<String> = c
        .secret_names()
        .map(|n| {
            if s.missing_secrets.iter().any(|m| m == n) {
                format!("{n} (missing)")
            } else {
                format!("{n} ✓")
            }
        })
        .collect();
    if !secrets.is_empty() {
        row("secrets", secrets.join(", "));
    }
    let mut flags = Vec::new();
    if c.requires_approval {
        flags.push("asks before each call");
    }
    if c.idempotent {
        flags.push("safe to retry");
    }
    if !flags.is_empty() {
        row("calls", flags.join(", "));
    }
    out.push(Line::default());

    if logs {
        out.push(Line::from(Span::styled("stderr (last lines)", bold())));
        if s.stderr_tail.is_empty() {
            out.push(Line::from(Span::styled("nothing written", dim())));
        }
        let first = s.stderr_tail.len().saturating_sub(room);
        for l in &s.stderr_tail[first..] {
            out.push(Line::from(Span::styled(truncate(l, width), dim())));
        }
        return out;
    }
    if s.tools.is_empty() {
        out.push(Line::from(Span::styled(
            "No tools listed yet; they show once the server has connected.",
            dim(),
        )));
    } else {
        let shown = s.tools.iter().filter(|t| !t.disabled).count();
        out.push(Line::from(Span::styled(
            format!("Tools ({shown} of {} shown to the model)", s.tools.len()),
            bold(),
        )));
    }
    let left = room.saturating_sub(out.len()).max(3);
    let first = (tool + 1).saturating_sub(left);
    for (i, t) in s.tools.iter().enumerate().skip(first).take(left) {
        out.push(tool_line(
            i == tool,
            !t.disabled,
            &t.remote,
            &t.description,
            width,
        ));
    }
    out
}

fn tool_line(selected: bool, on: bool, name: &str, desc: &str, width: usize) -> Line<'static> {
    let mark = if on { "[x] " } else { "[ ] " };
    let used = 2 + 4 + name.chars().count() + 2;
    Line::from(vec![
        Span::styled(if selected { "› " } else { "  " }, key()),
        Span::raw(mark),
        Span::styled(name.to_owned(), if on { Style::new() } else { dim() }),
        Span::raw("  "),
        Span::styled(
            truncate(
                desc.lines().next().unwrap_or(""),
                width.saturating_sub(used),
            ),
            dim(),
        ),
    ])
}

fn form_view(
    form: &Form,
    width: usize,
    room: usize,
) -> (
    String,
    Vec<Line<'static>>,
    Vec<(&'static str, &'static str)>,
) {
    let title = match &form.old {
        Some(n) => format!(" MCP · edit {n} "),
        None => " MCP · add server ".into(),
    };
    let mut body = Vec::new();
    let help = match &form.step {
        Step::Fields => {
            let fields = form.fields();
            let focused = form.focused();
            for (i, f) in fields.iter().enumerate() {
                let selected = i == form.focus;
                let mut value = form.value(*f);
                if selected && !f.is_toggle() {
                    value.push('▌');
                }
                body.push(Line::from(vec![
                    Span::styled(if selected { "› " } else { "  " }, key()),
                    Span::styled(
                        format!("{:22}", f.label()),
                        if selected { bold() } else { dim() },
                    ),
                    Span::raw(truncate(&value, width.saturating_sub(24))),
                ]));
            }
            body.push(Line::default());
            body.push(Line::from(Span::styled(focused.hint().to_owned(), dim())));
            vec![
                ("Enter", "next"),
                ("↑↓/Tab", "move"),
                ("Space", "toggle"),
                ("Esc", "cancel"),
            ]
        }
        Step::Secrets => {
            body.push(Line::from(Span::styled(
                "This server needs secrets that aren't set. They're stored in secrets.toml, \
                 never in config.toml.",
                dim(),
            )));
            body.push(Line::default());
            for (i, (name, value)) in form.secrets.iter().enumerate() {
                let selected = i == form.secret_focus;
                let n = value.chars().count();
                let shown = if n == 0 {
                    Span::styled("type or paste the value", dim())
                } else {
                    Span::raw(format!("{} ({n} characters)", "•".repeat(n.min(16))))
                };
                body.push(Line::from(vec![
                    Span::styled(if selected { "› " } else { "  " }, key()),
                    Span::styled(format!("{name:24}"), if selected { bold() } else { dim() }),
                    shown,
                    Span::raw(if selected { "▌" } else { "" }),
                ]));
            }
            vec![
                ("Enter", "test"),
                ("↑↓", "move"),
                ("Ctrl+U", "clear"),
                ("Esc", "back"),
            ]
        }
        Step::Testing => {
            body.push(Line::from(format!(
                "{} Starting {} and listing its tools…",
                spinner(),
                form.name.trim()
            )));
            vec![]
        }
        Step::Saving => {
            body.push(Line::from(format!("{} Saving…", spinner())));
            vec![]
        }
        Step::Failed(e) => {
            let what = match e.kind {
                McpErrorKind::CommandNotFound => "Command not found",
                McpErrorKind::SpawnFailed => "Couldn't start the command",
                McpErrorKind::ExitedEarly => "The server exited while starting",
                McpErrorKind::Timeout => "The server didn't answer in time",
                McpErrorKind::Unauthorized => "The server refused the credentials",
                McpErrorKind::Unreachable => "Couldn't reach the server",
                McpErrorKind::Protocol => "The server didn't speak MCP as expected",
                McpErrorKind::MissingSecret => "A secret is missing",
                McpErrorKind::Invalid => "The config isn't valid",
            };
            body.push(Line::from(Span::styled(
                what,
                theme::err().add_modifier(Modifier::BOLD),
            )));
            body.push(Line::from(e.message.clone()));
            if !e.stderr_tail.is_empty() {
                body.push(Line::default());
                body.push(Line::from(Span::styled("stderr:", dim())));
                let first = e
                    .stderr_tail
                    .len()
                    .saturating_sub(room.saturating_sub(4).max(3));
                for l in &e.stderr_tail[first..] {
                    body.push(Line::from(Span::styled(truncate(l, width), dim())));
                }
            }
            vec![
                ("e", "edit"),
                ("t", "test again"),
                ("s", "save without testing"),
                ("Esc", "cancel"),
            ]
        }
        Step::Tools => {
            if let Some(v) = &form.verified {
                body.push(Line::from(Span::styled(
                    format!(
                        "Connected{} — {} tools. Unchecked tools are hidden from the model.",
                        v.server_info
                            .as_ref()
                            .map(|i| format!(" to {i}"))
                            .unwrap_or_default(),
                        v.tools.len()
                    ),
                    theme::ok(),
                )));
            }
            body.push(Line::default());
            let first = (form.tool_cursor + 1).saturating_sub(room.saturating_sub(2).max(3));
            for (i, (t, on)) in form.tools.iter().enumerate().skip(first).take(room) {
                body.push(tool_line(
                    i == form.tool_cursor,
                    *on,
                    &t.remote,
                    &t.description,
                    width,
                ));
            }
            vec![("Enter", "save"), ("Space", "show/hide"), ("Esc", "back")]
        }
    };
    (title, body, help)
}
