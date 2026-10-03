//! Editable multi-line text with a cursor and an optional selection. Used by
//! the prompt.

/// A position: (line, column in chars), both from 0.
pub type Pos = (usize, usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBuffer {
    lines: Vec<String>,
    /// (line, column in chars). The column may equal the line length (insert
    /// position after the last char).
    row: usize,
    col: usize,
    /// The other end of the selection; the cursor is one end.
    anchor: Option<Pos>,
}

impl Default for TextBuffer {
    fn default() -> Self {
        TextBuffer {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            anchor: None,
        }
    }
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

impl TextBuffer {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(str::to_owned).collect();
        self.row = self.lines.len() - 1;
        self.col = char_len(&self.lines[self.row]);
        self.anchor = None;
    }

    /// The nearest valid position.
    pub fn clamp(&self, (row, col): Pos) -> Pos {
        let row = row.min(self.lines.len() - 1);
        (row, col.min(char_len(&self.lines[row])))
    }

    /// Chars before `pos`, counting each line break as one.
    pub fn offset(&self, pos: Pos) -> usize {
        let (row, col) = self.clamp(pos);
        self.lines[..row]
            .iter()
            .map(|l| char_len(l) + 1)
            .sum::<usize>()
            + col
    }

    /// The position `offset` chars in (clamped to the end).
    pub fn position(&self, mut offset: usize) -> Pos {
        for (i, l) in self.lines.iter().enumerate() {
            let n = char_len(l);
            if offset <= n {
                return (i, offset);
            }
            offset -= n + 1;
        }
        let last = self.lines.len() - 1;
        (last, char_len(&self.lines[last]))
    }

    pub fn set_cursor(&mut self, pos: Pos) {
        (self.row, self.col) = self.clamp(pos);
    }

    /// The selected range, start first; `None` when nothing is selected.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.clamp(self.anchor?);
        let cursor = (self.row, self.col);
        match anchor.cmp(&cursor) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some((anchor, cursor)),
            std::cmp::Ordering::Greater => Some((cursor, anchor)),
        }
    }

    /// Select from `anchor` to `cursor` (the cursor moves there).
    pub fn select(&mut self, anchor: Pos, cursor: Pos) {
        self.anchor = Some(self.clamp(anchor));
        self.set_cursor(cursor);
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Delete the selection, if any. Returns whether there was one.
    pub fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((a, b)) => {
                self.replace_range(a, b, "");
                true
            }
            None => {
                self.anchor = None;
                false
            }
        }
    }

    /// The text between two positions (in either order).
    pub fn range_text(&self, a: Pos, b: Pos) -> String {
        let (a, b) = (self.offset(a), self.offset(b));
        let (a, b) = (a.min(b), a.max(b));
        self.text().chars().skip(a).take(b - a).collect()
    }

    /// Replace the text between two positions (in either order) with `with`;
    /// the cursor goes after the new text and the selection is dropped.
    pub fn replace_range(&mut self, a: Pos, b: Pos, with: &str) {
        let (a, b) = (self.offset(a), self.offset(b));
        let (a, b) = (a.min(b), a.max(b));
        let with: String = with.chars().filter(|&c| c != '\r').collect();
        let text = self.text();
        let mut chars = text.chars();
        let mut out: String = chars.by_ref().take(a).collect();
        out.push_str(&with);
        out.extend(chars.skip(b - a));
        self.set_text(&out);
        let at = self.position(a + char_len(&with));
        self.set_cursor(at);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn line(&self) -> &str {
        &self.lines[self.row]
    }

    /// Typing replaces the selection.
    pub fn insert_char(&mut self, c: char) {
        self.delete_selection();
        if c == '\n' {
            return self.newline();
        }
        let at = byte_at(self.line(), self.col);
        self.lines[self.row].insert(at, c);
        self.col += 1;
    }

    /// Insert text that may contain newlines (e.g. a paste).
    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            match c {
                '\r' => {}
                c => self.insert_char(c),
            }
        }
    }

    pub fn newline(&mut self) {
        self.delete_selection();
        let at = byte_at(self.line(), self.col);
        let rest = self.lines[self.row].split_off(at);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    pub fn backspace(&mut self) {
        self.anchor = None;
        if self.col > 0 {
            self.col -= 1;
            let at = byte_at(self.line(), self.col);
            self.lines[self.row].remove(at);
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = char_len(self.line());
            self.lines[self.row].push_str(&line);
        }
    }

    pub fn delete(&mut self) {
        self.anchor = None;
        if self.col < char_len(self.line()) {
            let at = byte_at(self.line(), self.col);
            self.lines[self.row].remove(at);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    /// Delete the word before the cursor (ctrl+w).
    pub fn delete_word_back(&mut self) {
        if self.col == 0 {
            return self.backspace();
        }
        let start = self.word_back_col();
        let line = &mut self.lines[self.row];
        let (a, b) = (byte_at(line, start), byte_at(line, self.col));
        line.replace_range(a..b, "");
        self.col = start;
    }

    /// Delete from the cursor to the end of the line, or join the next line
    /// when already at the end (ctrl+k).
    pub fn delete_to_line_end(&mut self) {
        self.anchor = None;
        let at = byte_at(self.line(), self.col);
        if at == self.lines[self.row].len() {
            return self.delete();
        }
        self.lines[self.row].truncate(at);
    }

    /// Delete from line start to the cursor (ctrl+u).
    pub fn delete_to_line_start(&mut self) {
        self.anchor = None;
        let at = byte_at(self.line(), self.col);
        self.lines[self.row].replace_range(..at, "");
        self.col = 0;
    }

    pub fn left(&mut self) {
        self.col = self.col.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.col = (self.col + 1).min(char_len(self.line()));
    }

    pub fn up(&mut self) -> bool {
        if self.row == 0 {
            return false;
        }
        self.row -= 1;
        self.col = self.col.min(char_len(self.line()));
        true
    }

    pub fn down(&mut self) -> bool {
        if self.row + 1 >= self.lines.len() {
            return false;
        }
        self.row += 1;
        self.col = self.col.min(char_len(self.line()));
        true
    }

    pub fn line_start(&mut self) {
        self.col = 0;
    }

    pub fn line_end(&mut self) {
        self.col = char_len(self.line());
    }

    pub fn word_forward(&mut self) {
        let chars: Vec<char> = self.line().chars().collect();
        let mut i = self.col;
        if i >= chars.len() {
            if self.down() {
                self.col = 0;
            }
            return;
        }
        let start = class(chars[i]);
        while i < chars.len() && class(chars[i]) == start && start != Class::Space {
            i += 1;
        }
        while i < chars.len() && class(chars[i]) == Class::Space {
            i += 1;
        }
        self.col = i;
    }

    pub fn word_back(&mut self) {
        if self.col == 0 {
            if self.up() {
                self.line_end();
            }
            return;
        }
        self.col = self.word_back_col();
    }

    fn word_back_col(&self) -> usize {
        let chars: Vec<char> = self.line().chars().collect();
        let mut i = self.col;
        while i > 0 && class(chars[i - 1]) == Class::Space {
            i -= 1;
        }
        if i > 0 {
            let c = class(chars[i - 1]);
            while i > 0 && class(chars[i - 1]) == c {
                i -= 1;
            }
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits() {
        let mut t = TextBuffer::default();
        t.insert_str("héllo wörld\r\nsecond");
        assert_eq!(t.lines(), ["héllo wörld", "second"]);
        assert_eq!(t.cursor(), (1, 6));
        t.backspace();
        t.line_start();
        t.backspace();
        assert_eq!(t.text(), "héllo wörldsecon");
        t.delete_word_back();
        assert_eq!(t.text(), "héllo secon");
        t.line_end();
        t.delete_word_back();
        assert_eq!(t.text(), "héllo ");
        t.delete_to_line_start();
        assert!(t.is_empty());
    }

    #[test]
    fn positions_ranges_and_selection() {
        let mut t = TextBuffer::default();
        t.set_text("ab\ncdé\n");
        assert_eq!(t.offset((1, 2)), 5);
        assert_eq!(t.position(5), (1, 2));
        assert_eq!(t.position(99), (2, 0));
        assert_eq!(t.clamp((9, 9)), (2, 0));
        assert_eq!(t.range_text((1, 3), (0, 1)), "b\ncdé");
        t.replace_range((0, 1), (1, 1), "X\r\nY");
        assert_eq!(t.text(), "aX\nYdé\n");
        assert_eq!(t.cursor(), (1, 1));
        t.select((0, 0), (0, 2));
        assert_eq!(t.selection(), Some(((0, 0), (0, 2))));
        t.insert_char('z');
        assert_eq!(t.text(), "z\nYdé\n");
        assert_eq!(t.selection(), None);
        t.select((1, 3), (1, 1));
        assert_eq!(t.selection(), Some(((1, 1), (1, 3))));
        assert!(t.delete_selection());
        assert_eq!(t.text(), "z\nY\n");
        assert!(!t.delete_selection());
    }

    #[test]
    fn motions_and_kill_to_end() {
        let mut t = TextBuffer::default();
        t.set_text("foo.bar  baz");
        t.line_start();
        t.word_forward();
        assert_eq!(t.cursor().1, 3);
        t.word_forward();
        assert_eq!(t.cursor().1, 4);
        t.word_forward();
        assert_eq!(t.cursor().1, 9);
        t.word_back();
        assert_eq!(t.cursor().1, 4);
        t.delete_to_line_end();
        assert_eq!(t.text(), "foo.");
        t.insert_str("\nnext");
        t.up();
        t.line_end();
        t.delete_to_line_end();
        assert_eq!(t.text(), "foo.next");
    }
}
