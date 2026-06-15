pub mod backend;
mod banner;
mod bottom_pane;
pub mod markdown;
pub mod messages;
pub mod wrap;

use messages::wrapped_line_count;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::{Terminal, Viewport};
use std::io::{self, Stdout, Write};

use super::input::InputState;
use super::prompt::Prompt;
use super::theme::Theme;
use crate::chat::Message;
use crate::llm::TokenStats;
use crate::tools::ApprovalMode;
use crate::ui::pane_page::PanePage;
use backend::BoneBackend;

/// Minimum viewport rows: top-sep + input(1) + status.
pub(crate) const MIN_ROWS: u16 = 3;
pub use bottom_pane::PaneDraw;
pub(crate) use bottom_pane::clamped_pane_visible_rows;
pub use bottom_pane::{DEFAULT_PANE_ROWS, MAX_PANE_ROWS};
pub(crate) const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub type BoneTerminal = Terminal<BoneBackend<Stdout>>;

/// Status bar info passed from App to Renderer for each draw.
pub struct StatusInfo {
    pub model: String,
    pub token_stats: TokenStats,
    /// Live cumulative output-token estimate during streaming.
    pub streaming_completion_tokens: Option<u64>,
    pub streaming: bool,
    pub approval_mode: ApprovalMode,
    pub queue_len: usize,
    pub status_show: std::collections::HashMap<String, bool>,
    /// Formatted elapsed time string (e.g. "1:23") for the current turn.
    pub elapsed: Option<String>,
}

impl StatusInfo {
    pub fn show(&self, key: &str) -> bool {
        self.status_show.get(key).copied().unwrap_or(true)
    }
}

/// Owns all terminal rendering state and drawing logic.
pub struct Renderer {
    pub theme: Theme,
    /// Index of the first message NOT yet pushed to native scrollback.
    pub scrollback_cursor: usize,
    pub spinner_tick: usize,
    /// Byte offset of the current streaming assistant message already flushed
    /// to native scrollback via insert_before.
    pub streaming_source_flushed: usize,
    /// Number of stable rendered lines already inserted for the current response.
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
            streaming_source_flushed: 0,
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
        if let Err(err) = crossterm::execute!(io::stdout(), crossterm::event::EnableBracketedPaste)
        {
            crossterm::terminal::disable_raw_mode().ok();
            return Err(err);
        }
        let backend = BoneBackend::new(io::stdout());
        let terminal = Terminal::with_options(
            backend,
            ratatui::TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        );
        if terminal.is_err() {
            Self::shutdown_terminal().ok();
        }
        terminal
    }

    /// Recreate the terminal with a new viewport height.
    ///
    /// Since ratatui doesn't expose `set_viewport_height()`, we clear the
    /// old viewport, drop it, and create a fresh one. This is the same
    /// approach Codex uses — the cost is negligible and invisible.
    pub fn resize_viewport(term: &mut BoneTerminal, new_height: u16) -> io::Result<()> {
        // Clear the current viewport before swapping.
        term.clear()?;
        // Replace with a fresh terminal at the new height.
        Self::replace_terminal(term, new_height)
    }

    fn replace_terminal(term: &mut BoneTerminal, new_height: u16) -> io::Result<()> {
        let backend = BoneBackend::new(io::stdout());
        let new_term = Terminal::with_options(
            backend,
            ratatui::TerminalOptions {
                viewport: Viewport::Inline(new_height),
            },
        )?;
        *term = new_term;
        Ok(())
    }

    fn insert_lines_to_scrollback(
        &mut self,
        term: &mut BoneTerminal,
        lines: &[Line<'static>],
    ) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }

        let row_count = logical_lines_row_count(lines, term.size()?.width.max(1));
        term.insert_before(row_count, |buf| {
            let mut row = 0u16;
            for line in lines {
                let height = wrapped_line_count(line, buf.area.width.max(1));
                let area = Rect {
                    x: 0,
                    y: row,
                    width: buf.area.width,
                    height,
                };
                Paragraph::new(line.clone())
                    .wrap(Wrap { trim: false })
                    .render(area, buf);
                row = row.saturating_add(height);
            }
        })
    }

    /// Ensure the inline viewport height matches the content currently drawn
    /// in it. Streaming paths call this directly because they repaint without
    /// going through `App::redraw`.
    pub fn ensure_viewport_height(
        &mut self,
        term: &mut BoneTerminal,
        input: &InputState,
        prompt: Option<&Prompt>,
        pages: &[PanePage],
        active_page: usize,
        autocomplete: Option<&super::autocomplete::AutocompleteState>,
    ) -> io::Result<()> {
        let width = term.size()?.width;
        let desired = Self::desired_height(input, prompt, width, pages, active_page, autocomplete);
        if desired != self.viewport_height {
            Self::resize_viewport(term, desired)?;
            self.viewport_height = desired;
        }
        Ok(())
    }

    pub fn shutdown_terminal() -> io::Result<()> {
        let paste_result =
            crossterm::execute!(io::stdout(), crossterm::event::DisableBracketedPaste);
        let raw_result = crossterm::terminal::disable_raw_mode();
        paste_result.and(raw_result)
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

    pub fn render_banner(
        &self,
        term: &mut BoneTerminal,
        provider: &str,
        model: &str,
    ) -> io::Result<()> {
        banner::render(term, provider, model)
    }

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
        let user_background = self.theme.user_msg_bg;

        term.insert_before(logical_lines_row_count(&rendered, terminal_width), |buf| {
            let mut row = 0u16;
            for line in &rendered {
                let height = wrapped_line_count(line, buf.area.width.max(1));
                let msg_area = Rect {
                    x: 0,
                    y: row,
                    width: buf.area.width,
                    height,
                };
                if line
                    .spans
                    .iter()
                    .any(|span| span.style.bg == Some(user_background))
                {
                    buf.set_style(msg_area, ratatui::style::Style::default().bg(user_background));
                }
                Paragraph::new(line.clone())
                    .wrap(Wrap { trim: false })
                    .render(msg_area, buf);
                row = row.saturating_add(height);
            }
        })?;

        self.scrollback_cursor = messages.len();
        Ok(())
    }

    /// During streaming: flush complete source lines as soon as they are safe
    /// to render. Fenced code blocks and pipe tables are buffered until their
    /// final rendering is known.
    pub fn redraw_streaming_message(
        &mut self,
        content: &str,
        term: &mut BoneTerminal,
        args: &PaneDraw<'_>,
    ) -> io::Result<()> {
        self.ensure_viewport_height(term, args.input, None, args.pages, args.active_page, None)?; // autocomplete not active during streaming

        let safe_end = safe_markdown_prefix_end(content);
        if safe_end > self.streaming_source_flushed {
            let width = term.size()?.width.max(1);
            let rendered = messages::assistant_markdown_to_lines(&content[..safe_end], width);
            if self.streaming_lines_flushed < rendered.len() {
                self.insert_lines_to_scrollback(term, &rendered[self.streaming_lines_flushed..])?;
                self.streaming_lines_flushed = rendered.len();
            }
            self.streaming_source_flushed = safe_end;
        }

        // Redraw only composer/status UI. Incomplete assistant output is never
        // shown in the input viewport; it is inserted once markdown is stable.
        term.draw(|frame| self.draw_bottom_pane(frame, args, None))?;
        Ok(())
    }

    /// Flush all remaining lines from the streaming message, including
    /// the incomplete trailing paragraph that `redraw_streaming_message`
    /// holds back during streaming.
    pub fn finalize_streaming_message(
        &mut self,
        content: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let width = term.size()?.width.max(1);
        let rendered = messages::assistant_markdown_to_lines(content, width);
        if self.streaming_lines_flushed < rendered.len() {
            self.insert_lines_to_scrollback(term, &rendered[self.streaming_lines_flushed..])?;
        }
        self.streaming_source_flushed = content.len();
        self.streaming_lines_flushed = rendered.len();

        if !content.is_empty() || self.streaming_source_flushed > 0 {
            self.insert_lines_to_scrollback(term, &[ratatui::text::Line::raw("")])?;
        }
        Ok(())
    }

    /// Advance the spinner and redraw bottom pane.
    pub fn tick_spinner(&mut self, term: &mut BoneTerminal, args: &PaneDraw<'_>) -> io::Result<()> {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        self.ensure_viewport_height(term, args.input, None, args.pages, args.active_page, None)?;
        term.draw(|frame| self.draw_bottom_pane(frame, args, None))?;
        Ok(())
    }
}

fn logical_lines_row_count(lines: &[Line<'static>], width: u16) -> u16 {
    lines
        .iter()
        .map(|line| wrapped_line_count(line, width))
        .sum()
}

pub fn safe_markdown_prefix_end(content: &str) -> usize {
    let mut safe_end = 0;
    let mut in_fence: Option<(char, usize)> = None;
    let mut pending_pipe: Option<usize> = None;
    let mut in_table = false;

    for (start, line_with_newline) in complete_lines(content) {
        let line = line_with_newline
            .trim_end_matches('\n')
            .trim_end_matches('\r');
        let trimmed = line.trim();
        let end = start + line_with_newline.len();

        if let Some((fc, fl)) = in_fence {
            if is_closing_fence(trimmed, fc, fl) {
                in_fence = None;
                safe_end = end;
            }
            continue;
        }

        if let Some(fence) = opening_fence(trimmed) {
            in_fence = Some(fence);
            continue;
        }

        if in_table {
            if trimmed.is_empty() {
                in_table = false;
                safe_end = end;
            } else if !is_pipe_line(trimmed) {
                in_table = false;
                safe_end = start;
            }
            continue;
        }

        if let Some(pipe_start) = pending_pipe.take()
            && is_table_delimiter(trimmed)
        {
            in_table = true;
            safe_end = safe_end.min(pipe_start);
            continue;
        }

        if is_pipe_line(trimmed) {
            pending_pipe = Some(start);
            continue;
        }

        if trimmed.is_empty() {
            safe_end = end;
        }
    }

    safe_end
}

fn complete_lines(content: &str) -> impl Iterator<Item = (usize, &str)> {
    content
        .split_inclusive('\n')
        .scan(0usize, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line))
        })
        .filter(|(_, line)| line.ends_with('\n'))
}

fn opening_fence(trimmed: &str) -> Option<(char, usize)> {
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let c = bytes[0];
    if c != b'`' && c != b'~' {
        return None;
    }
    let len = bytes.iter().take_while(|&&b| b == c).count();
    if len >= 3 {
        Some((c as char, len))
    } else {
        None
    }
}

fn is_closing_fence(trimmed: &str, fence_char: char, fence_len: usize) -> bool {
    let bytes = trimmed.as_bytes();
    if bytes.len() < fence_len {
        return false;
    }
    let matching = bytes.iter().take(fence_len).all(|&b| b == fence_char as u8);
    matching
        && (bytes.len() == fence_len || trimmed[fence_len..].chars().all(|ch| ch.is_whitespace()))
}

fn is_pipe_line(trimmed: &str) -> bool {
    trimmed.contains('|')
}

fn is_table_delimiter(trimmed: &str) -> bool {
    if !is_pipe_line(trimmed) {
        return false;
    }
    let trimmed = trimmed.trim_matches('|').trim();
    let mut saw_cell = false;
    for cell in trimmed.split('|') {
        let cell = cell.trim();
        if cell.is_empty() {
            return false;
        }
        let cell = cell.trim_matches(':');
        if cell.len() < 3 || !cell.chars().all(|ch| ch == '-') {
            return false;
        }
        saw_cell = true;
    }
    saw_cell
}
