//! One place for every colour the TUI uses.
//!
//! Rules: text (body, secondary, input, status) always uses the terminal's own
//! foreground, so it is as legible as the user's terminal theme; emphasis comes
//! from bold/italic/reverse, never from a grey. Accent hues are fixed xterm-256
//! indexed colours chosen to hold >= 3:1 against both a dark (#1e1e1e) and a
//! light (#ffffff) background (see the contrast test), and they only decorate
//! glyphs, borders and bold labels. `NO_COLOR` strips every hue.

use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

fn no_color() -> bool {
    static NO: OnceLock<bool> = OnceLock::new();
    *NO.get_or_init(|| std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()))
}

pub(crate) const ACCENT: Color = Color::Indexed(30);
pub(crate) const OK: Color = Color::Indexed(28);
pub(crate) const WARN: Color = Color::Indexed(100);
pub(crate) const ERR: Color = Color::Indexed(167);
pub(crate) const LINK: Color = Color::Indexed(31);

fn hue(c: Color) -> Style {
    if no_color() {
        Style::new()
    } else {
        Style::new().fg(c)
    }
}

/// Secondary text: the default foreground, set apart by position, not colour.
pub(crate) fn muted() -> Style {
    Style::new()
}
/// Notices and hints: default foreground in italics.
pub(crate) fn note() -> Style {
    Style::new().add_modifier(Modifier::ITALIC)
}
pub(crate) fn accent() -> Style {
    hue(ACCENT)
}
pub(crate) fn ok() -> Style {
    hue(OK)
}
pub(crate) fn warn() -> Style {
    hue(WARN)
}
pub(crate) fn err() -> Style {
    hue(ERR)
}
pub(crate) fn link() -> Style {
    hue(LINK).add_modifier(Modifier::UNDERLINED)
}
/// The status bar: terminal colours swapped, always maximum contrast.
pub(crate) fn status_bar() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(i: u8) -> (u8, u8, u8) {
        let lv = [0u8, 95, 135, 175, 215, 255];
        let i = i as usize - 16;
        (lv[i / 36], lv[i / 6 % 6], lv[i % 6])
    }
    fn lum((r, g, b): (u8, u8, u8)) -> f64 {
        let f = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
    }
    fn ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn accents_hold_contrast_on_dark_and_light() {
        let dark = (0x1e, 0x1e, 0x1e);
        let light = (0xff, 0xff, 0xff);
        for (name, c) in [
            ("accent", ACCENT),
            ("ok", OK),
            ("warn", WARN),
            ("err", ERR),
            ("link", LINK),
        ] {
            let Color::Indexed(i) = c else { panic!() };
            for (bg_name, bg) in [("dark", dark), ("light", light)] {
                let r = ratio(rgb(i), bg);
                assert!(r >= 3.0, "{name} on {bg_name}: {r:.2}:1");
            }
        }
    }

    #[test]
    fn text_styles_set_no_foreground() {
        // Body, secondary and status text inherit the terminal's foreground.
        for s in [muted(), note(), status_bar()] {
            assert_eq!(s.fg, None);
            assert_eq!(s.bg, None);
        }
    }
}
