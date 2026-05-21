mod banner;
mod messages;
mod streaming;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};
use ratatui::{Frame, Terminal, Viewport};
use std::io::{self, Stdout};

use super::input::InputState;
use super::prompt::Prompt;
use super::theme::Theme;
use crate::chat::Message;
use crate::tools::types::ApprovalMode;

/// How many rows the bottom pane (input + status) occupies.
pub(crate) const BOTTOM_ROWS: u16 = 4;
pub(crate) const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub type BoneTerminal = Terminal<ratatui::backend::CrosstermBackend<Stdout>>;

/// Status bar info passed from App to Renderer for each draw.
pub struct StatusInfo {
    pub provider: String,
    pub model: String,
    pub msg_count: usize,
    pub streaming: bool,
    pub queue_len: usize,
    pub approval_mode: ApprovalMode,
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
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            theme: Theme::default(),
            scrollback_cursor: 0,
            spinner_tick: 0,
            streaming_lines_flushed: 0,
        }
    }

    /// Create a new terminal in inline-viewport mode.
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

    /// Recreate the inline viewport with a different height (e.g. to
    /// accommodate a prompt panel).  Raw mode stays enabled.
    pub fn resize_viewport(term: &mut BoneTerminal, new_height: u16) -> io::Result<()> {
        term.clear()?;
        *term = Terminal::with_options(
            ratatui::backend::CrosstermBackend::new(io::stdout()),
            ratatui::TerminalOptions {
                viewport: Viewport::Inline(new_height),
            },
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Banner
    // ------------------------------------------------------------------

    /// Render the startup banner into native scrollback (called once at launch).
    pub fn render_banner(
        &mut self,
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
        let rendered: Vec<Line<'static>> = messages::msg_to_lines(new_msgs, &self.theme, prev_role);
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
    // Bottom pane (status bar + input field + optional prompt)
    // ------------------------------------------------------------------

    /// Draw the bottom pane: separator + input + separator + status.
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
        let line = "─".repeat(area.width as usize);
        let mut y = area.y;

        // ── Input separator (top border) ──
        frame.render_widget(
            Paragraph::new(line.clone()).style(Style::default().fg(self.theme.input_border)),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
        y += 1;

        // ── Optional prompt panel (between separator and input) ──
        if let Some(prompt) = prompt {
            // Title
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {} ", prompt.title),
                    Style::default()
                        .fg(self.theme.status_text)
                        .add_modifier(Modifier::BOLD),
                )),
                Rect {
                    y,
                    height: 1,
                    ..area
                },
            );
            y += 1;

            // Options
            for (i, option) in prompt.options.iter().enumerate() {
                let selected = i == prompt.selected;
                let style = if selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let prefix = if selected { " > " } else { "   " };
                frame.render_widget(
                    Paragraph::new(Span::styled(format!("{}{}", prefix, option), style)),
                    Rect {
                        y,
                        height: 1,
                        ..area
                    },
                );
                y += 1;
            }
        }

        // ── Input line (hidden while prompt is active) ──
        if prompt.is_none() {
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

            frame.render_widget(
                Paragraph::new(input_line),
                Rect {
                    y,
                    height: 1,
                    ..area
                },
            );
            y += 1;
        }

        // ── Status separator ──
        frame.render_widget(
            Paragraph::new(line).style(Style::default().fg(self.theme.input_border)),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
        y += 1;

        // ── Status bar (always at the very bottom) ──
        let thinking = if status_info.streaming {
            format!(" │ {} thinking", SPINNER[tick % SPINNER.len()])
        } else {
            Default::default()
        };
        let queued = if status_info.queue_len > 0 {
            format!(" │ queued: {}", status_info.queue_len)
        } else {
            Default::default()
        };
        let status = Line::from(vec![
            Span::styled(
                format!(" {} │ {}  ", status_info.provider, status_info.model),
                Style::default().fg(self.theme.status_text),
            ),
            Span::styled(
                format!(" {} ", status_info.approval_mode.label()),
                Style::default().fg(match status_info.approval_mode {
                    ApprovalMode::Safe => self.theme.approval_safe,
                    ApprovalMode::Edits => self.theme.approval_edits,
                    ApprovalMode::Danger => self.theme.approval_danger,
                }),
            ),
            Span::styled(
                format!("│ msgs: {}{}{}", status_info.msg_count, thinking, queued),
                Style::default().fg(self.theme.status_text),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(status),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
    }
}
