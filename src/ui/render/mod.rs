pub mod backend;
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
pub(crate) use bottom_pane::approval_pane_lines;
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
    /// Lua-defined status segments (`bone.api.ui.set_statusline`), appended to
    /// the native status bar. Empty when Lua has not set one.
    pub lua_status: Vec<crate::runtime::view::StatusSegment>,
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
            scrollback_cursor: 0,
            spinner_tick: 0,
            streaming_source_flushed: 0,
            streaming_lines_flushed: 0,
            last_size: None,
            viewport_height: MIN_ROWS,
            scrollback_last_blank: false,
        }
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

    pub fn resize_viewport(
        term: &mut BoneTerminal,
        _old_height: u16,
        new_height: u16,
    ) -> io::Result<()> {
        // Clear the current viewport region, then recreate the terminal at the
        // new height. Use ratatui's `term.clear()` rather than manual cursor
        // movement: it clears the viewport's actual tracked area and positions
        // the replacement inline viewport at the old viewport top, so UI rows
        // do not leak into scrollback when the pane grows or shrinks.
        term.clear()?;
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

        let size = term.size()?;
        let w = size.width.max(1);

        // When viewport fills the screen, bypass ratatui's insert_before
        // (which flushes per-step causing flicker) and write directly via
        // crossterm in a single atomic flush.
        #[cfg(not(windows))]
        if self.viewport_height >= size.height {
            return self.scrollback_insert_direct(term, lines, w, None);
        }

        let row_count = logical_lines_row_count(lines, w);
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

    /// Insert scrollback lines directly via crossterm in a single atomic flush.
    ///
    /// Used when the viewport fills the entire terminal screen, because
    /// ratatui's `insert_before_scrolling_regions` flushes between draw+scroll
    /// steps in that code path, causing visible flicker as the terminal
    /// renders intermediate states.
    ///
    /// The approach mirrors what ratatui does semantically but in one write:
    ///   1. Draw scrollback content at the top of the screen (overwriting
    ///      viewport rows).
    ///   2. ScrollUp to push those rows into terminal scrollback.
    ///   3. Redraw the borrowed viewport rows at the new (now-blank) bottom.
    #[cfg(not(windows))]
    fn scrollback_insert_direct(
        &mut self,
        term: &mut BoneTerminal,
        lines: &[Line<'static>],
        term_width: u16,
        user_bg: Option<ratatui::style::Color>,
    ) -> io::Result<()> {
        use ratatui::backend::Backend;
        use ratatui::buffer::{Buffer, Cell};
        use ratatui::style::Style;

        let h = term.size()?.height;
        let n = logical_lines_row_count(lines, term_width).min(h);
        if n == 0 {
            return Ok(());
        }

        // 1. Render scrollback lines into a temporary buffer at (0, 0)..(w, n)
        let mut sb_buf = Buffer::empty(Rect::new(0, 0, term_width, n));
        {
            let mut row: u16 = 0;
            for line in lines {
                let height = wrapped_line_count(line, term_width);
                if row >= n {
                    break;
                }
                let area = Rect {
                    x: 0,
                    y: row,
                    width: term_width,
                    height: (height).min(n - row),
                };
                if let Some(bg) = user_bg
                    && line.spans.iter().any(|span| span.style.bg == Some(bg))
                {
                    sb_buf.set_style(area, Style::default().bg(bg));
                }
                ratatui::widgets::Paragraph::new(line.clone())
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .render(area, &mut sb_buf);
                row = row.saturating_add(height);
            }
        }

        // 2. Snapshot the top N rows of the current viewport buffer.
        let top_cells: Vec<(u16, u16, Cell)> = {
            let buf = term.current_buffer_mut();
            let area = buf.area();
            let top_n = n.min(area.height);
            let w = term_width.min(area.width);
            let mut cells = Vec::with_capacity((top_n as usize) * (w as usize));
            for y in 0..top_n {
                for x in 0..w {
                    cells.push((x, y, buf.cell((x, y)).cloned().unwrap_or_default()));
                }
            }
            cells
        };

        // 3. Collect scrollback cells at (0, 0)..(w-1, n-1)
        let sb_cells: Vec<(u16, u16, Cell)> = {
            let mut cells = Vec::with_capacity((n as usize) * (term_width as usize));
            for y in 0..n {
                for x in 0..term_width {
                    cells.push((x, y, sb_buf.cell((x, y)).cloned().unwrap_or_default()));
                }
            }
            cells
        };

        // 4. Queue all operations: draw scrollback at top → scroll up →
        //    redraw borrowed rows at bottom. Then flush once.
        let backend = term.backend_mut();

        // 4a. Draw scrollback cells at top of screen
        backend.draw(sb_cells.iter().map(|(x, y, c)| (*x, *y, c)))?;

        // 4b. Reset scroll region to full screen, then scroll up N.
        //     \x1b[r resets to full screen; \x1b[{n}S scrolls up N lines.
        {
            let writer = backend.inner.writer_mut();
            crossterm::queue!(
                writer,
                crossterm::style::Print(format!("\x1b[r\x1b[{}S", n)),
            )?;
        }

        // 4c. Redraw the borrowed viewport rows at the new bottom
        let new_bottom_start = h.saturating_sub(n);
        backend.draw(
            top_cells
                .iter()
                .map(|(x, y, c)| (*x, new_bottom_start + *y, c)),
        )?;

        // 4d. One flush for everything — terminal renders single atomic frame
        Write::flush(backend)?;

        Ok(())
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
        let size = term.size()?;
        let desired =
            Self::desired_height(input, prompt, size.width, pages, active_page, autocomplete)
                .min(size.height.max(1));
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
        let terminal_width = term.size()?.width;
        let rendered = messages::msg_to_lines(new_msgs, &self.theme, prev_role, terminal_width);
        // Collapse a leading blank against an already-blank scrollback tail so
        // streamed messages (no trailing blank) and flushed messages keep a
        // single blank of separation.
        let rendered = self.dedup_scrollback_blanks(&rendered);
        let user_background = self.theme.user_msg_bg;

        let row_count = logical_lines_row_count(&rendered, terminal_width);
        // The optimized direct-crossterm path (`scrollback_insert_direct`) uses
        // terminal scrolling regions that conhost doesn't support, so it is
        // Windows-excluded. Critically the `insert_before` fallback must still
        // run on Windows — keeping it inside the `cfg(not(windows))` else branch
        // compiled the entire write out on Windows, so no committed message
        // (user input, tool rows) ever reached scrollback there. Mirroring
        // `insert_lines_to_scrollback`, gate only the fast path and let the
        // portable `insert_before` always execute. `w`/`h` are fast-path only,
        // so they're scoped under the same guard to avoid dead bindings on
        // Windows.
        #[cfg(not(windows))]
        {
            let w = terminal_width.max(1);
            let h = term.size()?.height;
            if self.viewport_height >= h {
                self.scrollback_insert_direct(term, &rendered, w, Some(user_background))?;
                self.scrollback_cursor = messages.len();
                return Ok(());
            }
        }

        term.insert_before(row_count, |buf| {
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
                    buf.set_style(
                        msg_area,
                        ratatui::style::Style::default().bg(user_background),
                    );
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
    pub fn flush_streaming_message(
        &mut self,
        content: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
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
        Ok(())
    }

    /// Flush all remaining lines from the streaming message, including
    /// the incomplete trailing paragraph that `flush_streaming_message`
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
