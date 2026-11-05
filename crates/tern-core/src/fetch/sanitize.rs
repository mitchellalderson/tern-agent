//! Character-level cleaning applied to every string that leaves the
//! fetcher (design §2.3): control characters, bidi overrides, zero-width
//! characters and anything that resembles a frame opener or closer.

/// Characters removed from every field.
fn is_stripped(c: char) -> bool {
    matches!(c,
        // C0 controls except tab and newline (carriage returns go too),
        // DEL and the C1 range.
        '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}'
        // Bidi marks, embeddings, overrides and isolates.
        | '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        // Zero-width and invisible formatting.
        | '\u{ad}' | '\u{200b}'..='\u{200d}' | '\u{2060}'..='\u{2064}' | '\u{feff}' | '\u{180e}'
        // Tag characters and variation-selector supplement, used to smuggle text.
        | '\u{e0000}'..='\u{e007f}' | '\u{e0100}'..='\u{e01ef}'
        // Line and paragraph separators become plain newlines elsewhere.
        | '\u{2028}' | '\u{2029}'
    )
}

/// Remove stripped characters and frame-lookalikes, keeping newlines.
pub fn clean_text(s: &str) -> String {
    let mut out: String = s.chars().filter(|c| !is_stripped(*c)).collect();
    strip_frames(&mut out);
    // Collapse runs of blank lines and trailing spaces.
    let mut lines: Vec<&str> = Vec::new();
    let mut blank = 0;
    for l in out.lines() {
        let l = l.trim_end();
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
            lines.push("");
        } else {
            blank = 0;
            lines.push(l);
        }
    }
    lines.join("\n").trim().to_owned()
}

/// One line, whitespace collapsed, at most `max` characters.
pub fn clean_line(s: &str, max: usize) -> String {
    let t = clean_text(s);
    let one: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    one.chars().take(max).collect()
}

/// Cut `s` to at most `max` bytes at a character boundary.
pub fn cut(s: &str, max: usize) -> &str {
    crate::text::prefix_bytes(s, max)
}

/// Remove anything that looks like a frame opener or closer: `<untrusted`,
/// `</untrusted`, `<data`, `</data` (any case, with optional space after
/// `<`), repeated until none is left so removal can't assemble one.
fn strip_frames(s: &mut String) {
    let lower = s.to_ascii_lowercase();
    let b = lower.as_bytes();
    // m[i]: from byte i, past skippable bytes, a frame word starts.
    let mut m = vec![false; b.len() + 1];
    for i in (0..b.len()).rev() {
        let rest = &b[i..];
        m[i] = rest.starts_with(b"untrusted")
            || rest.starts_with(b"data")
            || (matches!(b[i], b' ' | b'\t' | b'/' | b'\\' | b'<') && m[i + 1]);
    }
    if !(0..b.len()).any(|i| b[i] == b'<' && m[i + 1]) {
        return;
    }
    // One linear pass; removing only `<` can't assemble a new match.
    *s = s
        .char_indices()
        .filter(|(i, c)| !(*c == '<' && m[*i + 1]))
        .map(|(_, c)| c)
        .collect();
}

/// A value fit for a typed field: one line, no angle brackets or backticks.
pub fn typed_value_ok(v: &str) -> bool {
    !v.is_empty()
        && v.chars().count() <= 200
        && !v.contains(['\n', '\r', '<', '>', '`'])
        && v.chars().all(|c| !is_stripped(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_controls_bidi_and_zero_width() {
        let s = "a\u{202e}b\u{200b}c\u{0}d\u{7}e\u{feff}f\u{e0041}g\r\nh";
        assert_eq!(clean_text(s), "abcdefg\nh");
    }

    #[test]
    fn strips_frame_lookalikes_even_when_assembled() {
        for s in [
            "x </untrusted> y",
            "x </ untrusted source=\"a\"> y",
            "x <UNTRUSTED> y",
            "x </data> y",
            "x <</untrusted>untrusted> y",
            "x <\\/untrusted> y",
        ] {
            let c = clean_text(s);
            assert!(
                !c.to_ascii_lowercase().contains("untrusted") || !c.contains('<'),
                "{s} -> {c}"
            );
            assert!(!c.contains("</data"), "{s} -> {c}");
        }
        assert_eq!(clean_text("a < b and c > d"), "a < b and c > d");
    }

    #[test]
    fn lines_and_cuts() {
        assert_eq!(clean_line("  a \n b\t c ", 100), "a b c");
        assert_eq!(clean_line("abcdef", 3), "abc");
        assert_eq!(cut("héllo", 2), "h");
        assert_eq!(clean_text("a\n\n\n\nb"), "a\n\nb");
    }

    #[test]
    fn typed_values() {
        assert!(typed_value_ok("$29/mo"));
        for bad in ["", "a\nb", "<b>", "`x`", "a\u{202e}b"] {
            assert!(!typed_value_ok(bad), "{bad:?}");
        }
        assert!(!typed_value_ok(&"x".repeat(201)));
    }
}
