//! Read-only transcript viewer (a sub-agent's transcript, or a conversation).

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{Key, KeyCode};
use crate::Message;
use crate::messages::msg_to_lines_with_options;
use crate::theme::Theme;

pub const MOUSE_WHEEL_LINES: usize = 3;

pub struct TranscriptScreen {
    messages: Vec<Message>,
    expanded_shell_outputs: bool,
    /// Lines laid out at `width`, rebuilt when the frame width changes.
    lines: Vec<Line<'static>>,
    width: u16,
    height: usize,
    /// `None` until the first draw pins the view to the end.
    scroll: Option<usize>,
}

impl TranscriptScreen {
    pub fn new(messages: Vec<Message>, expanded_shell_outputs: bool) -> Self {
        Self {
            messages,
            expanded_shell_outputs,
            lines: Vec::new(),
            width: 0,
            height: 1,
            scroll: None,
        }
    }

    /// Returns true when the key closes the viewer.
    pub fn handle_key(&mut self, key: Key) -> bool {
        let scroll = self.scroll.unwrap_or(0);
        let scroll = match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('o') if key.ctrl => return true,
            KeyCode::Down | KeyCode::Char('j') => scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
            KeyCode::PageDown => scroll.saturating_add(self.height),
            KeyCode::PageUp => scroll.saturating_sub(self.height),
            KeyCode::Home => 0,
            KeyCode::End => self.max_scroll(),
            _ => return false,
        };
        self.scroll = Some(scroll.min(self.max_scroll()));
        false
    }

    /// Scroll by wheel notches (positive scrolls down).
    pub fn scroll_by(&mut self, lines: i64) {
        let scroll = self.scroll.unwrap_or(0) as i64 + lines;
        self.scroll = Some((scroll.max(0) as usize).min(self.max_scroll()));
    }

    /// Map the existing close hint in the footer to Escape.
    pub fn touch_key(&self, row: u16, col: u16, _width: u16, height: u16) -> Option<Key> {
        if row + 1 != height {
            return None;
        }
        let scroll = "↑/↓ PgUp/PgDn Home/End scroll";
        let close = " · q/Esc/Ctrl+O close";
        let start = scroll.chars().count() as u16;
        let end = start + close.chars().count() as u16;
        (start..end)
            .contains(&col)
            .then_some(Key::plain(KeyCode::Esc))
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.height)
    }

    pub fn draw(&mut self, frame: &mut ratatui::Frame, theme: &Theme) {
        let area = frame.area();
        if area.width.max(1) != self.width {
            self.width = area.width.max(1);
            self.lines = msg_to_lines_with_options(
                &self.messages,
                theme,
                None,
                self.width,
                true,
                self.expanded_shell_outputs,
                self.expanded_shell_outputs,
            );
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);
        self.height = chunks[0].height as usize;
        let scroll = self
            .scroll
            .unwrap_or_else(|| self.max_scroll())
            .min(self.max_scroll());
        self.scroll = Some(scroll);
        let visible = self
            .lines
            .iter()
            .skip(scroll)
            .take(self.height)
            .cloned()
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible), chunks[0]);
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "↑/↓ PgUp/PgDn Home/End scroll · q/Esc/Ctrl+O close",
                Style::default().fg(theme.status_text),
            )])),
            chunks[1],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_on_the_existing_close_hint_returns_escape() {
        let screen = TranscriptScreen::new(Vec::new(), false);
        let start = "↑/↓ PgUp/PgDn Home/End scroll".chars().count() as u16;
        assert_eq!(
            screen.touch_key(9, start + 4, 80, 10).map(|key| key.code),
            Some(KeyCode::Esc)
        );
        assert!(screen.touch_key(9, 0, 80, 10).is_none());
    }
}
