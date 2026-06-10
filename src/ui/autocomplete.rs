//! Inline autocomplete dropdown for slash commands.
//!
//! Appears below the input field when the user types `/` and filters
//! in real-time as they type more characters. Shows up to 5 items at
//! a time with scroll support for longer lists.

use crate::ui::commands::BUILTINS;

/// Maximum number of suggestions shown at once.
pub const MAX_VISIBLE: usize = 5;

#[derive(Debug, Clone)]
pub struct AutocompleteState {
    /// All commands: (name, description), pre-sorted by name.
    all_commands: Vec<(String, String)>,
    /// Currently filtered matches.
    pub matches: Vec<(String, String)>,
    /// Index of the highlighted item in `matches`.
    pub selected: usize,
    /// Top index of the visible window within `matches`.
    pub scroll_offset: usize,
}

impl AutocompleteState {
    pub fn new(all_commands: Vec<(String, String)>) -> Self {
        let mut all_commands = all_commands;
        all_commands.sort_by(|a, b| a.0.cmp(&b.0));
        all_commands.dedup_by(|a, b| a.0 == b.0);
        let matches = all_commands.clone();
        let selected = 0;
        Self {
            all_commands,
            matches,
            selected,
            scroll_offset: 0,
        }
    }

    /// Re-filter matches based on the text typed after `/`.
    /// `query` is the part after the leading `/` (may be empty).
    pub fn update(&mut self, query: &str) {
        let q = query.to_lowercase();
        self.matches = self
            .all_commands
            .iter()
            .filter(|(name, _)| name.to_lowercase().starts_with(&q))
            .cloned()
            .collect();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    /// Move selection up. Wraps to bottom.
    pub fn up(&mut self) {
        if !self.matches.is_empty() {
            if self.selected > 0 {
                self.selected -= 1;
            } else {
                self.selected = self.matches.len() - 1;
            }
            self.clamp_scroll();
        }
    }

    /// Move selection down. Wraps to top.
    pub fn down(&mut self) {
        if !self.matches.is_empty() {
            if self.selected + 1 < self.matches.len() {
                self.selected += 1;
            } else {
                self.selected = 0;
            }
            self.clamp_scroll();
        }
    }

    /// Keep `selected` within the visible window `[scroll_offset, scroll_offset + MAX_VISIBLE)`.
    fn clamp_scroll(&mut self) {
        let max_offset = self.matches.len().saturating_sub(MAX_VISIBLE);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + MAX_VISIBLE {
            self.scroll_offset = self.selected.saturating_sub(MAX_VISIBLE - 1);
        }
        self.scroll_offset = self.scroll_offset.min(max_offset);
    }

    /// Get the currently selected command name.
    pub fn selected_command(&self) -> Option<&str> {
        self.matches
            .get(self.selected)
            .map(|(name, _)| name.as_str())
    }

    /// Number of visible rows this autocomplete needs.
    pub fn visible_rows(&self) -> u16 {
        MAX_VISIBLE as u16
    }

    /// Number of additional items below the visible window.
    pub fn more_count(&self) -> usize {
        self.matches
            .len()
            .saturating_sub(self.scroll_offset + MAX_VISIBLE)
    }

    /// Max display width of any command name in the matches list.
    pub fn max_name_width(&self) -> usize {
        self.matches
            .iter()
            .map(|(name, _)| name.len())
            .max()
            .unwrap_or(0)
            .max(6)
    }
}

/// Build the built-in commands list with descriptions.
pub fn builtin_commands() -> Vec<(String, String)> {
    BUILTINS
        .iter()
        .map(|(name, desc)| (name.to_string(), desc.to_string()))
        .collect()
}
