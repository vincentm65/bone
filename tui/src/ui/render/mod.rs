//! Rendering pipeline: backend, messages, markdown, wrap, and pane drawing.

pub mod backend;
mod bottom_pane;
pub mod markdown;
pub mod messages;
pub mod wrap;

use messages::wrapped_line_count;
use ratatui::layout::Rect;
use ratatui::style::Color;
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

/// Largest inline-viewport height permitted for a given terminal height.
///
/// We deliberately reserve at least one row above the viewport whenever the
/// terminal has at least two rows, so the inline viewport never occupies the
/// *entire* usable screen. Heights zero and one use one Ratatui row as a
/// defensive fallback because no row can be reserved.
///
/// A full-screen inline viewport forces ratatui's `insert_before` down a fragile
/// "borrow the top viewport line and scroll it into scrollback" path (see
/// `insert_before_scrolling_regions`' `viewport_area.height ==
/// last_known_area.height` branch), which intermittently strands bottom-pane
/// rows (the input field, wrapped command preview, and `────` separators) in
/// scrollback. Keeping one row free guarantees the robust partial-screen scroll
/// path is used whenever possible.
pub(crate) fn max_viewport_height(terminal_height: u16) -> u16 {
    terminal_height.saturating_sub(1).max(1)
}

pub(crate) fn initial_viewport_height(terminal_height: u16) -> u16 {
    MIN_ROWS.min(max_viewport_height(terminal_height))
}
pub use bottom_pane::PaneDraw;
pub(crate) use bottom_pane::approval_pane_lines;
pub(crate) use bottom_pane::clamped_pane_visible_rows;
pub use bottom_pane::{DEFAULT_PANE_ROWS, MAX_PANE_ROWS};
pub use bottom_pane::{InputPreset, InputStyle};

pub type BoneTerminal = Terminal<BoneBackend<Stdout>>;

fn terminal_color_rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0x00, 0x00, 0x00),
        Color::Red => (0xCD, 0x31, 0x31),
        Color::Green => (0x0D, 0xBC, 0x79),
        Color::Yellow => (0xE5, 0xE5, 0x10),
        Color::Blue => (0x24, 0x72, 0xC8),
        Color::Magenta => (0xBC, 0x3F, 0xBC),
        Color::Cyan => (0x11, 0xA8, 0xCD),
        Color::Gray => (0xC0, 0xC0, 0xC0),
        Color::DarkGray => (0x80, 0x80, 0x80),
        Color::LightRed => (0xF1, 0x4C, 0x4C),
        Color::LightGreen => (0x23, 0xD1, 0x8B),
        Color::LightYellow => (0xF5, 0xF5, 0x43),
        Color::LightBlue => (0x3B, 0x8E, 0xEA),
        Color::LightMagenta => (0xD6, 0x70, 0xD6),
        Color::LightCyan => (0x29, 0xB8, 0xDB),
        Color::White => (0xFF, 0xFF, 0xFF),
        Color::Indexed(_) | Color::Reset => (0xD4, 0xD4, 0xD4),
    }
}

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
    /// Lua-defined status segments (`bone.api.ui.set_statusline`), appended to
    /// the native status bar. Empty when Lua has not set one.
    pub lua_status: Vec<crate::runtime::view::StatusSegment>,
    /// Resolved spinner frames for the currently-selected style.
    pub spinner_frames: Vec<String>,
    /// Resolved frame speed in ms (override or style default).
    pub spinner_speed_ms: u64,
    /// Resolved rotating thinking-text phrases for the selected preset.
    pub spinner_texts: Vec<String>,
    /// Whether thinking-text phrases rotate while streaming.
    pub spinner_text_rotate: bool,
    /// Thinking-text rotation speed in ms/phrase; 0 means one phrase per spinner cycle.
    pub spinner_text_speed_ms: u64,
    /// Raw elapsed milliseconds of the current turn (for frame indexing).
    pub spinner_elapsed_ms: u64,
}

impl StatusInfo {
    pub fn show(&self, key: &str) -> bool {
        self.status_show.get(key).copied().unwrap_or(true)
    }
}

/// Owns all terminal rendering state and drawing logic.
pub struct Renderer {
    pub theme: Theme,
    pub input_style: InputStyle,
    /// Index of the first message NOT yet pushed to native scrollback.
    pub scrollback_cursor: usize,
    /// Byte offset of the current streaming assistant message already flushed
    /// to native scrollback via insert_before.
    pub streaming_source_flushed: usize,
    /// Terminal size at last successful draw (for stale-size detection).
    pub last_size: Option<(u16, u16)>,
    /// Current inline viewport height (resized dynamically).
    pub viewport_height: u16,
    /// Whether the last line pushed to scrollback was blank. Used to dedup
    /// consecutive blank lines so streamed messages (which bypass
    /// `msg_to_lines`' surrounding blanks) and flushed messages keep a single
    /// blank line of separation.
    scrollback_last_blank: bool,
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
            input_style: InputStyle::default(),
            scrollback_cursor: 0,
            streaming_source_flushed: 0,
            last_size: None,
            viewport_height: MIN_ROWS,
            scrollback_last_blank: false,
        }
    }

    pub fn apply_terminal_background(bg: Option<Color>) -> io::Result<bool> {
        if let Some(color) = bg {
            let (r, g, b) = terminal_color_rgb(color);
            print!("\x1b]11;rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\");
            io::stdout().flush()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn reset_terminal_background() -> io::Result<()> {
        print!("\x1b]111\x1b\\");
        io::stdout().flush()
    }

    /// Drop blank lines that would create a run of two or more consecutive
    /// blanks in scrollback (given the last line already there), and record
    /// whether the result ends blank. The single chokepoint that keeps message
    /// spacing to exactly one blank line.
    fn dedup_scrollback_blanks(&mut self, lines: &[Line<'static>]) -> Vec<Line<'static>> {
        let line_is_blank = |l: &Line<'static>| l.spans.iter().all(|s| s.content.trim().is_empty());
        let mut out = Vec::with_capacity(lines.len());
        let mut prev_blank = self.scrollback_last_blank;
        for line in lines {
            let blank = line_is_blank(line);
            if blank && prev_blank {
                continue;
            }
            out.push(line.clone());
            prev_blank = blank;
        }
        self.scrollback_last_blank = prev_blank;
        out
    }

    /// Insert a single blank separator line after the last scrollback content
    /// (deduped, so it's a no-op if already separated). Used to give streamed
    /// messages — which bypass `msg_to_lines`' surrounding blanks — a trailing
    /// blank: after the final reply (so it doesn't touch the input) and when a
    /// streamed assistant message is followed by a tool row.
    pub fn flush_separator(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        self.insert_lines_to_scrollback(term, &[Line::raw("")])
    }

    /// Create a new terminal in inline-viewport mode.
    ///
    /// Starts at `MIN_ROWS` (3 lines). The viewport is resized dynamically
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

    pub fn resize_viewport(
        term: &mut BoneTerminal,
        _old_height: u16,
        new_height: u16,
    ) -> io::Result<()> {
        // Clear the current viewport region, then recreate the terminal at the
        // new height. Use ratatui's `term.clear()` rather than manual cursor
        // movement: it clears the viewport's actual tracked area and positions
        // the replacement inline viewport at the old viewport top, so UI rows
        // do not leak into scrollback when the pane grows or shrinks. Crossterm
        // queues the clear; flush it before the replacement queries the cursor.
        term.clear()?;
        Write::flush(term.backend_mut())?;
        Self::replace_terminal(term, new_height)
    }

    /// Wipe the visible screen *and* native scrollback, home the cursor, then
    /// rebuild the inline viewport so ratatui's tracked area matches the new
    /// terminal size. Used after a physical resize: the emulator has reflowed
    /// the old viewport into an unknown number of rows, so the only reliable way
    /// to clear it is a hard reset followed by re-flushing history from scratch.
    ///
    /// `\x1b[2J` clears the screen, `\x1b[3J` clears the scrollback buffer, and
    /// `\x1b[H` homes the cursor so the rebuilt viewport starts at the top and
    /// is pushed down to the bottom as history is re-flushed (mirroring startup).
    pub fn hard_reset_viewport(term: &mut BoneTerminal, height: u16) -> io::Result<()> {
        crossterm::queue!(
            term.backend_mut(),
            crossterm::style::Print("\x1b[2J\x1b[3J\x1b[H"),
        )?;
        Write::flush(term.backend_mut())?;
        Self::replace_terminal(term, height)
    }

    /// Reset the scrollback render cursor so the next flush re-renders all
    /// messages from the top. Used by the resize rebuild.
    pub fn reset_scrollback_state(&mut self) {
        self.scrollback_cursor = 0;
        self.scrollback_last_blank = false;
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
        let lines = self.dedup_scrollback_blanks(lines);
        if lines.is_empty() {
            return Ok(());
        }
        let lines = &lines[..];

        // `insert_before` allocates its temp buffer with `viewport_area.width`,
        // which can lag `term.size().width` until the next autoresize/draw (e.g.
        // a pending Resize). Counting rows against the live size while rendering
        // into the narrower viewport buffer under-allocates and panics inside
        // ratatui's Buffer index. Always measure against the viewport width.
        let w = scrollback_insert_width(term);
        let row_count = logical_lines_row_count(lines, w);
        term.insert_before(row_count, |buf| {
            render_scrollback_lines(lines, buf);
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
        running: usize,
    ) -> io::Result<()> {
        let size = term.size()?;
        let desired = self
            .desired_height(
                input,
                prompt,
                size.width,
                pages,
                active_page,
                autocomplete,
                running,
            )
            .min(max_viewport_height(size.height));
        let old = self.viewport_height;
        if desired != old {
            Self::resize_viewport(term, old, desired)?;
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
        // Match `insert_before`'s buffer width (viewport), not the possibly-stale
        // live terminal size — see `insert_lines_to_scrollback`.
        let terminal_width = scrollback_insert_width(term);
        let rendered =
            messages::msg_to_lines(new_msgs, &self.theme, prev_role, terminal_width, false);
        // Collapse a leading blank against an already-blank scrollback tail so
        // streamed messages (no trailing blank) and flushed messages keep a
        // single blank of separation.
        let rendered = self.dedup_scrollback_blanks(&rendered);
        let user_background = self.theme.user_msg_bg;

        let row_count = logical_lines_row_count(&rendered, terminal_width);
        term.insert_before(row_count, |buf| {
            render_scrollback_lines_with_bg(&rendered, buf, Some(user_background));
        })?;

        self.scrollback_cursor = messages.len();
        Ok(())
    }

    /// During streaming: flush complete source lines as soon as they are safe
    /// to render. Fenced code blocks and pipe tables are buffered until their
    /// final rendering is known.
    pub fn flush_streaming_message(
        &mut self,
        content: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        self.flush_fragment(
            content,
            safe_markdown_prefix_end(content, self.streaming_source_flushed),
            term,
        )
    }

    /// Flush all remaining lines from the streaming message, including
    /// the incomplete trailing paragraph that `flush_streaming_message`
    /// holds back during streaming.
    pub fn finalize_streaming_message(
        &mut self,
        content: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        self.flush_fragment(content, content.len(), term)
    }

    /// Render `content[streaming_source_flushed..end]` as a standalone block
    /// fragment and push it to scrollback. `end` is always a block boundary
    /// (`safe_markdown_prefix_end` while streaming, `content.len()` at finalize),
    /// so the slice renders identically to the same span inside a full-message
    /// render — except for the inter-block blank that `render_markdown` trims at
    /// fragment edges, which we re-insert at the seam. Rendering only the new
    /// slice (rather than the whole prefix every delta) keeps streaming O(N)
    /// and highlights each code block exactly once.
    fn flush_fragment(
        &mut self,
        content: &str,
        end: usize,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        if end <= self.streaming_source_flushed {
            return Ok(());
        }
        let width = scrollback_insert_width(term);
        let fragment = &content[self.streaming_source_flushed..end];
        let mut rendered = messages::assistant_markdown_to_lines(fragment, width, &self.theme);
        if !rendered.is_empty() && self.streaming_source_flushed > 0 {
            // Restore the one blank separator render_markdown trims at the seam.
            // dedup_scrollback_blanks collapses any accidental double.
            rendered.insert(0, Line::raw(""));
        }
        self.insert_lines_to_scrollback(term, &rendered)?;
        self.streaming_source_flushed = end;
        Ok(())
    }

    /// Redraw the bottom pane during streaming (elapsed-time spinner advances).
    pub fn tick_spinner(&mut self, term: &mut BoneTerminal, args: &PaneDraw<'_>) -> io::Result<()> {
        self.ensure_viewport_height(
            term,
            args.input,
            None,
            args.pages,
            args.active_page,
            None,
            args.running.len(),
        )?;
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

/// Width of the temporary buffer `Terminal::insert_before` will allocate.
///
/// Must be used for both row-count precomputation and content wrapping so the
/// draw closure never writes past the allocated height.
fn scrollback_insert_width(term: &mut BoneTerminal) -> u16 {
    term.get_frame().area().width.max(1)
}

/// Paint logical lines into an `insert_before` temp buffer, wrapping each line
/// to `buf.area.width` and never advancing past `buf.area.height`.
fn render_scrollback_lines(lines: &[Line<'static>], buf: &mut ratatui::buffer::Buffer) {
    render_scrollback_lines_with_bg(lines, buf, None);
}

fn render_scrollback_lines_with_bg(
    lines: &[Line<'static>],
    buf: &mut ratatui::buffer::Buffer,
    user_background: Option<Color>,
) {
    let mut row = 0u16;
    let width = buf.area.width;
    for line in lines {
        let remaining = buf.area.height.saturating_sub(row);
        if remaining == 0 {
            break;
        }
        // Clamp to remaining rows so a line_count/render mismatch cannot OOB.
        let height = wrapped_line_count(line, width.max(1)).min(remaining);
        let area = Rect {
            x: 0,
            y: row,
            width,
            height,
        };
        if let Some(bg) = user_background {
            if line.spans.iter().any(|span| span.style.bg == Some(bg)) {
                buf.set_style(area, ratatui::style::Style::default().bg(bg));
            }
        }
        Paragraph::new(line.clone())
            .wrap(Wrap { trim: false })
            .render(area, buf);
        row = row.saturating_add(height);
    }
}

pub fn safe_markdown_prefix_end(content: &str, from: usize) -> usize {
    let mut safe_end = from;
    let mut in_fence: Option<(char, usize)> = None;
    let mut pending_pipe: Option<usize> = None;
    let mut in_table = false;

    for (start, line_with_newline) in complete_lines(content, from) {
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

#[cfg(test)]
#[path = "render_tests.rs"]
mod render_tests;

fn complete_lines(content: &str, from: usize) -> impl Iterator<Item = (usize, &str)> {
    content[from..]
        .split_inclusive('\n')
        .scan(from, |offset, line| {
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
