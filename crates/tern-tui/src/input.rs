//! A small multi-line text editor for the input box.

use crate::wrap::{hard_wrap, str_width};

#[derive(Debug, Default, Clone)]
pub struct Input {
    text: String,
    /// Byte offset of the cursor; always on a char boundary.
    cursor: usize,
}

impl Input {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn clear(&mut self) {
        self.take();
    }

    pub fn insert(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.text.replace_range(self.cursor..next, "");
        }
    }

    pub fn left(&mut self) {
        if let Some(p) = self.prev_boundary() {
            self.cursor = p;
        }
    }

    pub fn right(&mut self) {
        if let Some(n) = self.next_boundary() {
            self.cursor = n;
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i);
    }

    /// Move up a line, keeping the column. Returns false on the first line.
    pub fn up(&mut self) -> bool {
        let start = self.line_start();
        if start == 0 {
            return false;
        }
        let col = self.text[start..self.cursor].chars().count();
        let prev_start = self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        self.cursor = advance(&self.text, prev_start, col, start - 1);
        true
    }

    /// Move down a line, keeping the column. Returns false on the last line.
    pub fn down(&mut self) -> bool {
        let Some(nl) = self.text[self.cursor..].find('\n').map(|i| self.cursor + i) else {
            return false;
        };
        let col = self.text[self.line_start()..self.cursor].chars().count();
        let next_start = nl + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map_or(self.text.len(), |i| next_start + i);
        self.cursor = advance(&self.text, next_start, col, next_end);
        true
    }

    /// Delete back to the start of the line (Ctrl+U).
    pub fn kill_line(&mut self) {
        let start = self.line_start();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Visual rows when hard-wrapped to `width`, and the cursor's (row, col).
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let mut rows = Vec::new();
        let mut cursor = (0, 0);
        let mut offset = 0;
        for line in self.text.split('\n') {
            let wrapped = hard_wrap(line, width);
            if (offset..=offset + line.len()).contains(&self.cursor) {
                let before = str_width(&line[..self.cursor - offset]);
                let (r, c) = (before / width.max(1), before % width.max(1));
                // A cursor exactly at a wrap point sits at the end of the row.
                let (r, c) = if c == 0 && r > 0 && r == wrapped.len() {
                    (r - 1, width)
                } else {
                    (r, c)
                };
                cursor = (rows.len() + r, c);
            }
            offset += line.len() + 1;
            rows.extend(wrapped);
        }
        (rows, cursor)
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }
}

/// Byte offset `col` chars after `start`, but not past `limit`.
fn advance(text: &str, start: usize, col: usize, limit: usize) -> usize {
    text[start..limit]
        .char_indices()
        .nth(col)
        .map_or(limit, |(i, _)| start + i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> Input {
        let mut i = Input::default();
        i.insert_str(s);
        i
    }

    #[test]
    fn editing() {
        let mut i = typed("héllo");
        i.backspace();
        assert_eq!(i.text(), "héll");
        i.home();
        i.right();
        i.right();
        i.backspace();
        assert_eq!(i.text(), "hll");
        i.delete();
        assert_eq!(i.text(), "hl");
        i.end();
        i.insert('!');
        assert_eq!(i.take(), "hl!");
        assert!(i.is_empty());
    }

    #[test]
    fn vertical_movement_keeps_column() {
        let mut i = typed("abcdef\nxy\nlonger line");
        assert!(i.up());
        assert_eq!(&i.text()[..i.cursor], "abcdef\nxy");
        assert!(i.up());
        assert_eq!(&i.text()[..i.cursor], "ab");
        assert!(!i.up());
        assert!(i.down());
        assert!(i.down());
        assert_eq!(&i.text()[..i.cursor], "abcdef\nxy\nlo");
        assert!(!i.down());
    }

    #[test]
    fn layout_tracks_cursor_through_wraps_and_newlines() {
        let i = typed("abcdefgh\nij");
        let (rows, cur) = i.layout(5);
        assert_eq!(rows, ["abcde", "fgh", "ij"]);
        assert_eq!(cur, (2, 2));
        let mut j = typed("abcde");
        assert_eq!(j.layout(5).1, (0, 5));
        j.home();
        assert_eq!(j.layout(5).1, (0, 0));
    }

    #[test]
    fn kill_line() {
        let mut i = typed("one\ntwo three");
        i.kill_line();
        assert_eq!(i.text(), "one\n");
    }
}
