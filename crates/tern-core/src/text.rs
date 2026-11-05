//! UTF-8-safe slicing helpers for untrusted text.
//!
//! Anything that cuts a `&str` at a byte budget (truncation markers, entity
//! look-ahead, previews) must go through these: a bare `&s[..n]` panics when
//! `n` lands inside a multibyte character, and web pages, tool output and
//! model text are all attacker-influenced.

/// Largest char boundary `<= idx` (clamped to `s.len()`).
pub fn floor_char_boundary(s: &str, idx: usize) -> usize {
    s.floor_char_boundary(idx)
}

/// Smallest char boundary `>= idx` (clamped to `s.len()`).
pub fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    s.ceil_char_boundary(idx)
}

/// The longest prefix of `s` that is at most `max` bytes and ends on a char
/// boundary.
pub fn prefix_bytes(s: &str, max: usize) -> &str {
    &s[..floor_char_boundary(s, max)]
}

/// The longest suffix of `s` that is at most `max` bytes and starts on a char
/// boundary.
pub fn suffix_bytes(s: &str, max: usize) -> &str {
    let start = ceil_char_boundary(s, s.len().saturating_sub(max));
    &s[start..]
}

/// Slice `s[start..end]` without panicking: both ends are clamped to the
/// string and moved inward to char boundaries (`start` up, `end` down).
pub fn slice_bytes(s: &str, start: usize, end: usize) -> &str {
    let end = floor_char_boundary(s, end);
    let start = ceil_char_boundary(s, start).min(end);
    &s[start..end]
}

/// Like `&s[start..end]` but `None` unless both offsets are in range, ordered
/// and on char boundaries. Use when an offset is claimed by untrusted input
/// and must be validated rather than silently moved.
pub fn checked_slice(s: &str, start: usize, end: usize) -> Option<&str> {
    s.get(start..end)
}

/// Truncate a `String` in place to at most `max` bytes on a char boundary.
pub fn truncate_string(s: &mut String, max: usize) {
    if s.len() > max {
        s.truncate(floor_char_boundary(s, max));
    }
}

/// Lossy decode of arbitrary bytes, capped at `max` bytes of output text.
pub fn lossy_prefix(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    prefix_bytes(&s, max).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLES: &[&str] = &[
        "",
        "ascii only",
        "a💫b💫c",
        "日本語のテキスト",
        "e\u{301}e\u{301}e\u{301}",
        "👨\u{200d}👩\u{200d}👧",
        "\u{fffd}\u{fffd}x",
    ];

    #[test]
    fn every_offset_is_safe() {
        for s in SAMPLES {
            for i in 0..=s.len() + 3 {
                let p = prefix_bytes(s, i);
                assert!(p.len() <= i && s.starts_with(p));
                let q = suffix_bytes(s, i);
                assert!(q.len() <= i && s.ends_with(q));
                for j in 0..=s.len() + 3 {
                    let m = slice_bytes(s, i, j);
                    assert!(s.contains(m));
                    assert_eq!(checked_slice(s, i, j).is_some(), s.get(i..j).is_some());
                }
                let mut o = (*s).to_owned();
                truncate_string(&mut o, i);
                assert!(o.len() <= i && s.starts_with(&o));
            }
        }
    }

    #[test]
    fn straddling_emoji() {
        assert_eq!(prefix_bytes("a💫b", 3), "a");
        assert_eq!(prefix_bytes("a💫b", 5), "a💫");
        assert_eq!(suffix_bytes("a💫b", 3), "b");
        assert_eq!(checked_slice("a💫b", 0, 3), None);
    }

    #[test]
    fn lossy_invalid_utf8() {
        let bytes = [b'a', 0xF0, 0x9F, b'b', 0xFF, b'c'];
        let s = lossy_prefix(&bytes, 4);
        assert!(s.len() <= 4);
        assert!(!s.is_empty());
    }
}
