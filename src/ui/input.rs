/// Input field state
///
/// `cursor_pos` tracks a **character** offset (not byte offset) so multi-byte
/// Unicode graphemes don't cause panics during insertion/deletion.
#[derive(Debug, Default)]
pub struct InputState {
    pub buffer: String,
    pub cursor_pos: usize,
    /// History of sent messages (up/down arrow to navigate)
    pub history: Vec<String>,
    pub history_index: Option<usize>,
}

impl InputState {
    // ------------------------------------------------------------------
    // Unicode-safe cursor helpers
    // ------------------------------------------------------------------

    /// Convert the char-index `cursor_pos` into a byte index for String ops.
    fn byte_pos(&self) -> usize {
        self.buffer
            .char_indices()
            .nth(self.cursor_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.buffer.len())
    }

    /// Insert a single character at the cursor and advance the cursor.
    pub fn insert_char(&mut self, c: char) {
        let bp = self.byte_pos();
        self.buffer.insert(bp, c);
        self.cursor_pos += 1;
    }

    /// Delete the character **before** the cursor (Backspace).
    pub fn delete_backward(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let prev_char_idx = self.cursor_pos - 1;
        let prev_byte = self
            .buffer
            .char_indices()
            .nth(prev_char_idx)
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.buffer.remove(prev_byte);
        self.cursor_pos = prev_char_idx;
    }

    /// Delete the word before the cursor (Ctrl+W).
    pub fn delete_word_backward(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let chars: Vec<char> = self.buffer.chars().collect();
        let start = self.cursor_pos;

        let mut boundary = start;
        while boundary > 0 && chars[boundary - 1].is_whitespace() {
            boundary -= 1;
        }
        while boundary > 0 && !chars[boundary - 1].is_whitespace() {
            boundary -= 1;
        }

        let byte_start = self
            .buffer
            .char_indices()
            .nth(boundary)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let byte_end = self.byte_pos();
        self.buffer.replace_range(byte_start..byte_end, "");
        self.cursor_pos = boundary;
    }

    /// Move cursor to the beginning of the buffer.
    pub fn cursor_to_start(&mut self) {
        self.cursor_pos = 0;
    }

    /// Move cursor to the end of the buffer.
    pub fn cursor_to_end(&mut self) {
        self.cursor_pos = self.buffer.chars().count();
    }

    /// Clear the entire buffer (Ctrl+U).
    pub fn clear_buffer(&mut self) {
        self.buffer.clear();
        self.cursor_pos = 0;
    }

    pub fn reset(&mut self) {
        if !self.buffer.is_empty() {
            self.history.push(self.buffer.clone());
        }
        self.buffer.clear();
        self.cursor_pos = 0;
        self.history_index = None;
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = self.history_index.unwrap_or(self.history.len());
        if idx > 0 {
            self.history_index = Some(idx - 1);
            self.buffer = self.history[idx - 1].clone();
            self.cursor_pos = self.buffer.chars().count();
        }
    }

    pub fn history_down(&mut self) {
        if let Some(idx) = self.history_index {
            if idx + 1 < self.history.len() {
                self.history_index = Some(idx + 1);
                self.buffer = self.history[idx + 1].clone();
            } else {
                self.history_index = None;
                self.buffer.clear();
            }
            self.cursor_pos = self.buffer.chars().count();
        }
    }
}
