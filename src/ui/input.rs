use crossterm::event::{KeyCode, KeyModifiers};

/// Result of applying a key to the input state.
/// Callers use this to decide side-effects (queue, redraw, etc.).
#[derive(Debug)]
pub enum InputAction {
    /// Buffer changed or cursor moved — needs redraw.
    Redraw,
    /// User pressed Enter with non-empty text.
    Submit,
    /// User pressed Ctrl+C.
    Cancel,
    /// User pressed Ctrl+D — clear the queue.
    ClearQueue,
    /// User pressed BackTab — cycle approval mode.
    CycleMode,
    /// User pressed Esc — clear the buffer.
    Escape,
    /// Open the system editor.
    OpenEditor,
    /// Key was not handled — no action needed.
    None,
}

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

    /// Convert the char-index cursor position to a byte index for String ops.
    fn byte_pos(&self) -> usize {
        self.buffer
            .char_indices()
            .nth(self.cursor_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.buffer.len())
    }

    /// Insert a character at the cursor.
    pub fn insert_char(&mut self, c: char) {
        let bp = self.byte_pos();
        self.buffer.insert(bp, c);
        self.cursor_pos += 1;
    }

    /// Delete the character before the cursor (Backspace).
    pub fn delete_backward(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let prev_char_idx = self.cursor_pos - 1;
        // Find the byte range of the character to remove.
        let (start_byte, ch) = self
            .buffer
            .char_indices()
            .nth(prev_char_idx)
            .map(|(i, c)| (i, c))
            .unwrap_or((0, '\0'));
        self.buffer.replace_range(start_byte..start_byte + ch.len_utf8(), "");
        self.cursor_pos = prev_char_idx;
    }

    /// Delete the character after the cursor (Delete).
    pub fn delete_forward(&mut self) {
        if self.cursor_pos >= self.buffer.chars().count() {
            return;
        }
        let byte = self.byte_pos();
        let next_byte = self
            .buffer
            .char_indices()
            .nth(self.cursor_pos + 1)
            .map(|(i, _)| i)
            .unwrap_or(self.buffer.len());
        self.buffer.replace_range(byte..next_byte, "");
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

    /// Delete to end of line (Ctrl+K).
    pub fn kill_to_end(&mut self) {
        let byte_start = self.byte_pos();
        self.buffer.truncate(byte_start);
    }

    /// Move cursor one word backward (Alt+Left).
    pub fn cursor_word_backward(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut pos = self.cursor_pos;
        while pos > 0 && chars[pos - 1].is_whitespace() {
            pos -= 1;
        }
        while pos > 0 && !chars[pos - 1].is_whitespace() {
            pos -= 1;
        }
        self.cursor_pos = pos;
    }

    /// Move cursor one word forward (Alt+Right).
    pub fn cursor_word_forward(&mut self) {
        let len = self.buffer.chars().count();
        if self.cursor_pos >= len {
            return;
        }
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut pos = self.cursor_pos;
        while pos < len && !chars[pos].is_whitespace() {
            pos += 1;
        }
        while pos < len && chars[pos].is_whitespace() {
            pos += 1;
        }
        self.cursor_pos = pos;
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
            // Deduplicate: remove previous occurrence if it exists
            if let Some(pos) = self.history.iter().rposition(|s| s == &self.buffer) {
                self.history.remove(pos);
            }
            self.history.push(self.buffer.clone());
        }
        self.buffer.clear();
        self.cursor_pos = 0;
        self.history_index = None;
    }

    /// Yields the action to take (redraw, submit, etc.). Single source
    /// of truth for key handling — used by the main loop and streaming drain.
    pub fn apply_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> InputAction {
        // Ctrl+C always cancels.
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            return InputAction::Cancel;
        }

        // Alt (Option) shortcuts.
        if modifiers.contains(KeyModifiers::ALT) {
            match code {
                KeyCode::Left => {
                    self.cursor_word_backward();
                    return InputAction::Redraw;
                }
                KeyCode::Right => {
                    self.cursor_word_forward();
                    return InputAction::Redraw;
                }
                _ => return InputAction::None,
            }
        }

        // Ctrl shortcuts.
        if modifiers.contains(KeyModifiers::CONTROL) {
            return match code {
                KeyCode::Char('a') => {
                    self.cursor_to_start();
                    InputAction::Redraw
                }
                KeyCode::Char('e') => {
                    self.cursor_to_end();
                    InputAction::Redraw
                }
                KeyCode::Char('w') => {
                    self.delete_word_backward();
                    InputAction::Redraw
                }
                KeyCode::Char('u') => {
                    self.clear_buffer();
                    InputAction::Redraw
                }
                KeyCode::Char('k') => {
                    self.kill_to_end();
                    InputAction::Redraw
                }
                KeyCode::Char('d') => InputAction::ClearQueue,
                KeyCode::Char('x') => InputAction::OpenEditor,
                _ => InputAction::None,
            };
        }

        // No modifiers.
        match code {
            KeyCode::BackTab => InputAction::CycleMode,
            KeyCode::Enter => {
                if self.buffer.trim().is_empty() {
                    InputAction::None
                } else {
                    InputAction::Submit
                }
            }
            KeyCode::Char(c) => {
                self.insert_char(c);
                InputAction::Redraw
            }
            KeyCode::Backspace => {
                self.delete_backward();
                InputAction::Redraw
            }
            KeyCode::Delete => {
                self.delete_forward();
                InputAction::Redraw
            }
            KeyCode::Left => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                }
                InputAction::Redraw
            }
            KeyCode::Right => {
                if self.cursor_pos < self.buffer.chars().count() {
                    self.cursor_pos += 1;
                }
                InputAction::Redraw
            }
            KeyCode::Home => {
                self.cursor_to_start();
                InputAction::Redraw
            }
            KeyCode::End => {
                self.cursor_to_end();
                InputAction::Redraw
            }
            KeyCode::Up => {
                self.history_up();
                InputAction::Redraw
            }
            KeyCode::Down => {
                self.history_down();
                InputAction::Redraw
            }
            KeyCode::Esc => {
                self.clear_buffer();
                InputAction::Escape
            }
            _ => InputAction::None,
        }
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
