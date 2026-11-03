//! Display-width-aware line wrapping.

use unicode_width::UnicodeWidthChar;

/// Word-wrap `text` to `width` columns. Explicit newlines are kept; words
/// longer than a line are split.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut cur_w = 0;
        for word in split_keep_spaces(line) {
            let w = str_width(word);
            if cur_w + w <= width {
                cur.push_str(word);
                cur_w += w;
                continue;
            }
            if word.trim().is_empty() {
                // A space that doesn't fit ends the line.
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
                continue;
            }
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur).trim_end().to_owned());
                cur_w = 0;
            }
            for ch in word.chars() {
                let cw = ch.width().unwrap_or(0);
                if cur_w + cw > width && !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(ch);
                cur_w += cw;
            }
        }
        out.push(cur.trim_end().to_owned());
    }
    out
}

/// Hard-wrap at exactly `width` columns, no word breaking. Used for the
/// input box so cursor positions are easy to compute.
pub fn hard_wrap(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = vec![String::new()];
    let mut w = 0;
    for ch in line.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > width {
            out.push(String::new());
            w = 0;
        }
        if let Some(last) = out.last_mut() {
            last.push(ch);
        }
        w += cw;
    }
    out
}

pub fn str_width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Split into alternating runs of non-space and space characters.
fn split_keep_spaces(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_space = None;
    for (i, ch) in s.char_indices() {
        let sp = ch == ' ';
        if in_space.is_some_and(|prev| prev != sp) {
            out.push(&s[start..i]);
            start = i;
        }
        in_space = Some(sp);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_words() {
        assert_eq!(wrap("the quick brown fox", 10), ["the quick", "brown fox"]);
        assert_eq!(wrap("a\n\nb", 10), ["a", "", "b"]);
        assert_eq!(wrap("", 10), [""]);
    }

    #[test]
    fn splits_long_words_and_respects_wide_chars() {
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("日本語テキスト", 6), ["日本語", "テキス", "ト"]);
        for line in wrap("lorem ipsum dolor sit amet consectetur", 7) {
            assert!(str_width(&line) <= 7, "{line:?}");
        }
    }

    #[test]
    fn hard_wrap_is_exact() {
        assert_eq!(hard_wrap("abcdef", 4), ["abcd", "ef"]);
        assert_eq!(hard_wrap("", 4), [""]);
        assert_eq!(hard_wrap("abcd", 4), ["abcd"]);
    }
}
