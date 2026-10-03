//! Editable multi-line text with a cursor. Used by prompt buffers and the
//! command line.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBuffer {
    lines: Vec<String>,
    /// (line, column in chars). The column may equal the line length (insert
    /// position after the last char).
    row: usize,
    col: usize,
}

impl Default for TextBuffer {
    fn default() -> Self {
        TextBuffer {
            lines: vec![String::new()],
            row: 0,
            col: 0,
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
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn line(&self) -> &str {
        &self.lines[self.row]
    }

    pub fn insert_char(&mut self, c: char) {
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
        let at = byte_at(self.line(), self.col);
        let rest = self.lines[self.row].split_off(at);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    pub fn backspace(&mut self) {
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
        let at = byte_at(self.line(), self.col);
        if at == self.lines[self.row].len() {
            return self.delete();
        }
        self.lines[self.row].truncate(at);
    }

    /// Delete from line start to the cursor (ctrl+u).
    pub fn delete_to_line_start(&mut self) {
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
