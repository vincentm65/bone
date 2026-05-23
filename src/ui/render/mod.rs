mod banner;
mod bottom_pane;
mod messages;
mod streaming;
pub mod wrap;

use ratatui::layout::Rect;

use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, Viewport};
use std::io::{self, Stdout, Write};

use super::input::InputState;
use super::prompt::Prompt;
use super::theme::Theme;
use crate::chat::Message;
use crate::llm::TokenStats;
use crate::tools::types::ApprovalMode;

/// Minimum viewport rows: top-sep + input(1) + bottom-sep + status.
pub(crate) const MIN_ROWS: u16 = 4;
pub(crate) const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub type BoneTerminal = Terminal<ratatui::backend::CrosstermBackend<Stdout>>;

/// Status bar info passed from App to Renderer for each draw.
pub struct StatusInfo {
    pub model: String,
    pub token_stats: TokenStats,
    pub streaming: bool,
    pub approval_mode: ApprovalMode,
    pub queue_len: usize,
}

/// Owns all terminal rendering state and drawing logic.
pub struct Renderer {
    pub theme: Theme,
    /// Index of the first message NOT yet pushed to native scrollback.
    pub scrollback_cursor: usize,
    pub spinner_tick: usize,
    /// Number of lines of the current streaming assistant message already
    /// flushed to native scrollback via insert_before.
    pub streaming_lines_flushed: usize,
    /// Terminal size at last successful draw (for stale-size detection).
    pub last_size: Option<(u16, u16)>,
    /// Current inline viewport height (resized dynamically).
    pub viewport_height: u16,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            theme: Theme::default(),
            scrollback_cursor: 0,
            spinner_tick: 0,
            streaming_lines_flushed: 0,
            last_size: None,
            viewport_height: MIN_ROWS,
        }
    }

    /// Create a new terminal in inline-viewport mode.
    ///
    /// Starts at `MIN_ROWS` (4 lines). The viewport is resized dynamically
    /// via `resize_viewport()` as the input field grows or shrinks.
    pub fn init_terminal(height: u16) -> io::Result<BoneTerminal> {
        crossterm::terminal::enable_raw_mode()?;
        let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
        Terminal::with_options(
            backend,
            ratatui::TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )
    }

    /// Recreate the terminal with a new viewport height.
    ///
    /// Since ratatui doesn't expose `set_viewport_height()`, we clear the
    /// old viewport, drop it, and create a fresh one. This is the same
    /// approach Codex uses — the cost is negligible and invisible.
    pub fn resize_viewport(term: &mut Option<BoneTerminal>, new_height: u16) -> io::Result<()> {
        if let Some(mut t) = term.take() {
            t.clear()?;
            drop(t);
        }
        let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
        let new_term = Terminal::with_options(
            backend,
            ratatui::TerminalOptions {
                viewport: Viewport::Inline(new_height),
            },
        )?;
        *term = Some(new_term);
        Ok(())
    }

    pub fn shutdown_terminal() -> io::Result<()> {
        crossterm::terminal::disable_raw_mode()
    }

    /// Clear the inline viewport and leave a clean exit in scrollback.
    ///
    /// This is the "Codex handoff trick": wipe the viewport so stale UI
    /// (input field, status bar, spinner) doesn't linger, then print a
    /// closing marker so the user sees a clean text seam where the TUI
    /// ended and normal terminal output resumes.
    pub fn prepare_exit(term: &mut BoneTerminal) -> io::Result<()> {
        term.clear()?;
        crossterm::execute!(term.backend_mut(), crossterm::style::Print("\r\n"))?;
        io::stdout().flush()
    }

    // ------------------------------------------------------------------
    // Banner
    // ------------------------------------------------------------------

    pub fn render_banner(
        &self,
        term: &mut BoneTerminal,
        provider: &str,
        model: &str,
    ) -> io::Result<()> {
        banner::render(term, provider, model)
    }

    // ------------------------------------------------------------------
    // Scrollback / messages
    // ------------------------------------------------------------------

    /// Push messages that haven't been flushed yet into native terminal scrollback.
    ///
    /// Uses `insert_before` which inserts lines *above* the fixed inline
    /// viewport. The viewport itself stays in place and never enters scrollback.
    pub fn flush_new_to_scrollback(
        &mut self,
        messages: &[Message],
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        if self.scrollback_cursor >= messages.len() {
            return Ok(());
        }

        let start = self.scrollback_cursor;
        let new_msgs = &messages[start..];
        let prev_role = if start > 0 {
            Some(messages[start - 1].role)
        } else {
            None
        };
        let terminal_width = term.size()?.width;
        let rendered: Vec<Line<'static>> =
            messages::msg_to_lines(new_msgs, &self.theme, prev_role, terminal_width);
        let line_count = rendered.len() as u16;

        term.insert_before(line_count, |buf| {
            for (row, line) in rendered.iter().enumerate() {
                let msg_area = Rect {
                    x: 0,
                    y: row as u16,
                    width: buf.area.width,
                    height: 1,
                };
                Paragraph::new(line.clone()).render(msg_area, buf);
            }
        })?;

        self.scrollback_cursor = messages.len();
        Ok(())
    }

    // ------------------------------------------------------------------
    // Streaming
    // ------------------------------------------------------------------

    /// During streaming: flush only complete lines of the assistant message.
    pub fn redraw_streaming_message(
        &mut self,
        content: &str,
        term: &mut BoneTerminal,
        input: &InputState,
        status_info: &StatusInfo,
    ) -> io::Result<()> {
        streaming::redraw(self, content, term, input, status_info)
    }

    /// Flush all remaining lines from the streaming message.
    pub fn finalize_streaming_message(
        &mut self,
        content: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        streaming::finalize(self, content, term)
    }

    /// Advance the spinner and redraw bottom pane.
    pub fn tick_spinner(
        &mut self,
        term: &mut BoneTerminal,
        input: &InputState,
        status_info: &StatusInfo,
    ) -> io::Result<()> {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        term.draw(|frame| self.draw_bottom_pane(frame, input, status_info, None))?;
        Ok(())
    }
}
