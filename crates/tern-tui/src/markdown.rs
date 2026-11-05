//! Light inline-markdown styling for already-wrapped assistant lines.
//!
//! Handles `**bold**`, `*italic*`, `` `code` ``, fenced blocks, `#` headings and
//! `-`/`*` bullets. State carries across wrapped lines. Anything unmatched is
//! shown as typed. Emphasis is bold/italic/underline on the default foreground.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme;

pub fn style_lines(lines: Vec<String>) -> Vec<Line<'static>> {
    let (mut bold, mut italic, mut code, mut fence) = (false, false, false, false);
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            out.push(Line::from(Span::styled(line, theme::accent())));
            continue;
        }
        if fence {
            out.push(Line::from(Span::styled(line, theme::accent())));
            continue;
        }
        let mut rest = line.as_str();
        let mut base = Style::new();
        let mut prefix = String::new();
        let trimmed = rest.trim_start();
        let indent = rest.len() - trimmed.len();
        if trimmed.starts_with('#') {
            let hashes = trimmed.chars().take_while(|c| *c == '#').count();
            if hashes <= 6 && trimmed[hashes..].starts_with(' ') {
                base = base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                rest = trimmed[hashes + 1..].trim_start();
            }
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            prefix = format!("{}• ", &rest[..indent]);
            rest = item;
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        if !prefix.is_empty() {
            spans.push(Span::raw(prefix));
        }
        let mut buf = String::new();
        let chars: Vec<char> = rest.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next_word = chars.get(i + 1).is_some_and(|n| !n.is_whitespace());
            let prev_word = i > 0 && !chars[i - 1].is_whitespace();
            let toggle = if c == '`' {
                Some((1, 0))
            } else if !code && c == '*' && chars.get(i + 1) == Some(&'*') {
                Some((2, 1))
            } else if !code && c == '*' && next_word != prev_word {
                Some((1, 2))
            } else {
                None
            };
            let Some((width, which)) = toggle else {
                buf.push(c);
                i += 1;
                continue;
            };
            if !buf.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut buf),
                    run_style(base, bold, italic, code),
                ));
            }
            match which {
                0 => code = !code,
                1 => bold = !bold,
                _ => italic = !italic,
            }
            i += width;
        }
        if !buf.is_empty() {
            spans.push(Span::styled(buf, run_style(base, bold, italic, code)));
        }
        out.push(Line::from(spans));
    }
    out
}

fn run_style(base: Style, bold: bool, italic: bool, code: bool) -> Style {
    let mut s = base;
    if bold {
        s = s.add_modifier(Modifier::BOLD);
    }
    if italic {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if code {
        s = s.patch(theme::accent());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn strips_markers_and_styles_runs() {
        let l = style_lines(vec!["a **bold** and *it* and `c`".into()]);
        assert_eq!(text(&l[0]), "a bold and it and c");
        assert!(l[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert!(l[0].spans[3].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn headings_bullets_and_fences() {
        let l = style_lines(vec![
            "## Title".into(),
            "- item".into(),
            "```".into(),
            "**raw**".into(),
        ]);
        assert_eq!(text(&l[0]), "Title");
        assert_eq!(text(&l[1]), "• item");
        assert_eq!(text(&l[3]), "**raw**");
    }

    #[test]
    fn bold_carries_across_wrapped_lines() {
        let l = style_lines(vec!["**start".into(), "end** done".into()]);
        assert!(l[1].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(!l[1].spans[1].style.add_modifier.contains(Modifier::BOLD));
    }
}
