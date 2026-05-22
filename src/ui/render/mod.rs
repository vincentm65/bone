mod banner;
mod messages;
mod streaming;
pub mod wrap;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};
use ratatui::{Frame, Terminal, Viewport};
use std::io::{self, Stdout, Write};

use super::input::InputState;
use super::prompt::Prompt;
use super::theme::Theme;
use crate::chat::Message;
use crate::llm::TokenStats;
use crate::tools::types::ApprovalMode;

/// Fixed viewport height — **never** changes at runtime.
///
/// This is the single most important constant for scrollback stability.
/// The inline viewport is created once at startup and never resized.
/// `insert_before` simply pushes lines above it — the viewport itself
/// can never end up in scrollback.
///
/// Layout (always 4 rows):
///   row 0 — separator
///   row 1 — input field (or inline prompt options)
///   row 2 — separator
///   row 3 — status bar
pub(crate) const BOTTOM_ROWS: u16 = 4;
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
        }
    }

    /// Create a new terminal in inline-viewport mode with a **fixed** height.
    ///
    /// The viewport height is constant (`BOTTOM_ROWS`) and never changes.
    /// This prevents the viewport from ever being recreated, which is what
    /// causes its content to leak into scrollback.
    pub fn init_terminal() -> io::Result<BoneTerminal> {
        crossterm::terminal::enable_raw_mode()?;
        let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
        Terminal::with_options(
            backend,
            ratatui::TerminalOptions {
                viewport: Viewport::Inline(BOTTOM_ROWS),
            },
        )
    }

    pub fn shutdown_terminal() -> io::Result<()> {
        crossterm::terminal::disable_raw_mode()
    }

    /// Leave the shell prompt below Bone's inline viewport on exit.
    pub fn prepare_exit(term: &mut BoneTerminal) -> io::Result<()> {
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

    // ------------------------------------------------------------------
    // Bottom pane — fixed 4-row layout
    // ------------------------------------------------------------------

    /// Draw the bottom pane into the fixed inline viewport.
    ///
    /// Layout (always exactly `BOTTOM_ROWS` = 4 rows):
    ///   row 0 — separator
    ///   row 1 — input field OR inline prompt selector
    ///   row 2 — separator
    ///   row 3 — status bar
    pub fn draw_bottom_pane(
        &self,
        frame: &mut Frame,
        input: &InputState,
        status_info: &StatusInfo,
        prompt: Option<&Prompt>,
    ) {
        self.draw_bottom_pane_with_tick(frame, input, status_info, self.spinner_tick, prompt);
    }

    /// Draw the bottom pane with an explicit spinner tick (used during stream wait).
    pub fn draw_bottom_pane_with_tick(
        &self,
        frame: &mut Frame,
        input: &InputState,
        status_info: &StatusInfo,
        tick: usize,
        prompt: Option<&Prompt>,
    ) {
        let area = frame.area();
        frame.render_widget(Clear, area);
        let sep = "─".repeat(area.width as usize);

        let mut y = area.y;

        // ── Row 0: top separator ──
        frame.render_widget(
            Paragraph::new(sep.clone()).style(Style::default().fg(self.theme.input_border)),
            Rect { y, height: 1, ..area },
        );
        y += 1;

        // ── Row 1: input field or inline prompt ──
        if let Some(prompt) = prompt {
            // Show all options on one line: "▶ Accept  Advise  Cancel — title"
            let mut spans: Vec<Span> = Vec::new();
            for (i, option) in prompt.options.iter().enumerate() {
                let selected = i == prompt.selected;
                if selected {
                    spans.push(Span::styled(
                        format!(" ▶ {} ", option),
                        Style::default()
                            .fg(ratatui::style::Color::White)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled(
                        format!("   {} ", option),
                        Style::default().fg(ratatui::style::Color::DarkGray),
                    ));
                }
            }
            // Append title truncated to remaining width
            let title = format!("  {}", prompt.title);
            spans.push(Span::styled(
                title,
                Style::default().fg(self.theme.system_msg),
            ));
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect { y, height: 1, ..area },
            );
        } else {
            // Normal input — single line, text wraps visually but we only
            // render one row.  The user sees the tail end of long input.
            let chars: Vec<char> = input.buffer.chars().collect();
            let pos = input.cursor_pos.min(chars.len());
            let before: String = chars[..pos].iter().collect();
            let at_cursor = chars.get(pos).unwrap_or(&' ');
            let after: String = chars[pos..].iter().skip(1).collect();

            let input_line = Line::from(vec![
                Span::raw("> "),
                Span::raw(before),
                Span::styled(
                    at_cursor.to_string(),
                    Style::default().add_modifier(Modifier::REVERSED),
                ),
                Span::raw(after),
            ]);

            // Wrap into the single input row.
            frame.render_widget(
                Paragraph::new(input_line).wrap(Wrap { trim: false }),
                Rect { y, height: 1, ..area },
            );
        }
        y += 1;

        // ── Row 2: bottom separator ──
        frame.render_widget(
            Paragraph::new(sep).style(Style::default().fg(self.theme.input_border)),
            Rect { y, height: 1, ..area },
        );
        y += 1;

        // ── Row 3: status bar ──
        let mut status_spans: Vec<Span> = vec![
            Span::styled(
                status_info.model.to_string(),
                Style::default().fg(self.theme.status_text),
            ),
            Span::styled(" | ", Style::default().fg(self.theme.status_text)),
            Span::styled(
                status_info.approval_mode.label().to_string(),
                Style::default().fg(match status_info.approval_mode {
                    ApprovalMode::Safe => self.theme.approval_safe,
                    ApprovalMode::Edits => self.theme.approval_edits,
                    ApprovalMode::Danger => self.theme.approval_danger,
                }),
            ),
            Span::styled(" | ", Style::default().fg(self.theme.status_text)),
            Span::styled(
                status_info.token_stats.display(),
                Style::default().fg(self.theme.status_text),
            ),
        ];

        if status_info.queue_len > 0 {
            status_spans.push(Span::styled(
                " | ",
                Style::default().fg(self.theme.status_text),
            ));
            status_spans.push(Span::styled(
                format!("📥 {}", status_info.queue_len),
                Style::default().fg(self.theme.status_text),
            ));
        }

        if status_info.streaming {
            status_spans.push(Span::styled(
                " | ",
                Style::default().fg(self.theme.status_text),
            ));
            status_spans.push(Span::styled(
                format!("{} thinking", SPINNER[tick % SPINNER.len()]),
                Style::default().fg(self.theme.status_text),
            ));
        }

        frame.render_widget(
            Paragraph::new(Line::from(status_spans)),
            Rect { y, height: 1, ..area },
        );
    }
}
