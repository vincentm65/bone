use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Frame, Terminal, Viewport};
use std::io::{self, Stdout};
use std::path::Path;

use super::input::{InputState, Message};
use super::theme::Theme;
use crate::llm::ChatRole;

/// How many rows the bottom pane (input + status) occupies.
const BOTTOM_ROWS: u16 = 4;
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub type BoneTerminal = Terminal<ratatui::backend::CrosstermBackend<Stdout>>;

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

    /// Render the startup banner into native scrollback (called once at launch).
    pub fn render_banner(
        &mut self,
        term: &mut BoneTerminal,
        provider: &str,
        model: &str,
    ) -> io::Result<()> {
        let version = env!("CARGO_PKG_VERSION");
        let cwd = std::env::current_dir()
            .unwrap_or_else(|_| Path::new(".").to_path_buf());
        let dir_display = format_short_dir(&cwd);

        let dim = Style::default().fg(Color::DarkGray);
        let bold_white = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
        let accent = Style::default().fg(Color::Cyan);
        let muted = Style::default().fg(Color::Gray);

        // Inner width between the vertical bars — spans full terminal.
        // " ╭" + inner + "╮" = terminal width
        let term_width = term.size().map(|s| s.width).unwrap_or(80) as usize;
        let inner = term_width.saturating_sub(3); // " ╭" (2 chars) + "╮" (1 char)

        // Row 1: bone ... v0.1.0
        let r1_left = "bone";
        let r1_right = format!("v{version}");
        let r1_pad = inner.saturating_sub(r1_left.len() + r1_right.len());

        // Row 2: provider · model ... dir
        let r2_left = format!("{provider} · {model}");
        let r2_right = dir_display;
        let r2_pad = inner.saturating_sub(r2_left.len() + r2_right.len());

        let banner_lines: Vec<Line<'static>> = vec![
            // Top border
            Line::from(vec![
                Span::styled(" ╭", dim),
                Span::styled("─".repeat(inner), dim),
                Span::styled("╮", dim),
            ]),
            // Row 1
            Line::from(vec![
                Span::styled(" │ ", dim),
                Span::styled(r1_left.to_string(), bold_white),
                Span::styled(" ".repeat(r1_pad), Style::default()),
                Span::styled(r1_right, muted),
                Span::styled(" │", dim),
            ]),
            // Row 2
            Line::from(vec![
                Span::styled(" │ ", dim),
                Span::styled(r2_left, accent),
                Span::styled(" ".repeat(r2_pad), Style::default()),
                Span::styled(r2_right, dim),
                Span::styled(" │", dim),
            ]),
            // Bottom border
            Line::from(vec![
                Span::styled(" ╰", dim),
                Span::styled("─".repeat(inner), dim),
                Span::styled("╯", dim),
            ]),
            // Blank line after banner
            Line::raw(""),
        ];

        let line_count = banner_lines.len() as u16;
        term.insert_before(line_count, |buf| {
            for (row, line) in banner_lines.iter().enumerate() {
                let area = Rect {
                    x: 0,
                    y: row as u16,
                    width: buf.area.width,
                    height: 1,
                };
                Paragraph::new(line.clone()).render(area, buf);
            }
        })?;

        Ok(())
    }

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
        let rendered: Vec<Line<'static>> = new_msgs
            .iter()
            .enumerate()
            .flat_map(|(i, message)| {
                let prev_role = if i == 0 && start > 0 {
                    Some(messages[start - 1].role)
                } else if i > 0 {
                    Some(new_msgs[i - 1].role)
                } else {
                    None
                };
                msg_to_lines(message, &self.theme, prev_role)
            })
            .collect();
        let line_count = rendered.len() as u16;

        term.insert_before(line_count, |buf| {
            for (row, line) in rendered.iter().enumerate() {
                let msg_area = Rect { x: 0, y: row as u16, width: buf.area.width, height: 1 };
                Paragraph::new(line.clone()).render(msg_area, buf);
            }
        })?;

        self.scrollback_cursor = messages.len();
        Ok(())
    }

    /// During streaming: flush only complete lines of the assistant message.
    pub fn redraw_streaming_message(
        &mut self,
        content: &str,
        term: &mut BoneTerminal,
        input: &InputState,
        status_info: &StatusInfo,
    ) -> io::Result<()> {
        let all_lines: Vec<&str> = content.lines().collect();

        let complete = if content.ends_with('\n') {
            all_lines.len()
        } else {
            all_lines.len().saturating_sub(1)
        };

        if complete > self.streaming_lines_flushed {
            let new_lines = &all_lines[self.streaming_lines_flushed..complete];
            let line_count = new_lines.len() as u16;
            term.insert_before(line_count, |buf| {
                for (row, &raw_line) in new_lines.iter().enumerate() {
                    let line = Line::raw(raw_line.to_string());
                    let msg_area = Rect {
                        x: 0,
                        y: row as u16,
                        width: buf.area.width,
                        height: 1,
                    };
                    Paragraph::new(line).render(msg_area, buf);
                }
            })?;
            self.streaming_lines_flushed = complete;
        }

        term.draw(|frame| self.draw_bottom_pane(frame, input, status_info))?;
        Ok(())
    }

    /// Flush all remaining lines from the streaming message (including the
    /// final partial line that `redraw_streaming_message` skips).
    pub fn finalize_streaming_message(
        &mut self,
        content: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let all_lines: Vec<&str> = content.lines().collect();

        if all_lines.len() > self.streaming_lines_flushed {
            let remaining = &all_lines[self.streaming_lines_flushed..];
            let line_count = remaining.len() as u16;
            term.insert_before(line_count, |buf| {
                for (row, &raw_line) in remaining.iter().enumerate() {
                    let line = Line::raw(raw_line.to_string());
                    let msg_area = Rect {
                        x: 0,
                        y: row as u16,
                        width: buf.area.width,
                        height: 1,
                    };
                    Paragraph::new(line).render(msg_area, buf);
                }
            })?;
            self.streaming_lines_flushed = all_lines.len();
        }

        Ok(())
    }

    /// Advance the spinner and redraw bottom pane.
    pub fn tick_spinner(&mut self, term: &mut BoneTerminal, input: &InputState, status_info: &StatusInfo) -> io::Result<()> {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        term.draw(|frame| self.draw_bottom_pane(frame, input, status_info))?;
        Ok(())
    }

    /// Draw the bottom pane: separator + input + separator + status.
    pub fn draw_bottom_pane(&self, frame: &mut Frame, input: &InputState, status_info: &StatusInfo) {
        self.draw_bottom_pane_with_tick(frame, input, status_info, self.spinner_tick);
    }

    /// Draw the bottom pane with an explicit spinner tick (used during stream wait).
    pub fn draw_bottom_pane_with_tick(
        &self,
        frame: &mut Frame,
        input: &InputState,
        status_info: &StatusInfo,
        tick: usize,
    ) {
        let area = frame.area();
        let line = "─".repeat(area.width as usize);

        for y in [area.y, area.y + 2] {
            frame.render_widget(
                Paragraph::new(line.clone()).style(Style::default().fg(self.theme.input_border)),
                Rect { y, height: 1, ..area },
            );
        }

        // Input line with visible cursor at the correct character position.
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
            Rect { y: area.y + 1, height: 1, ..area },
        );

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
        let status = format!(
            " {} │ {} │ msgs: {}{}{}",
            status_info.provider,
            status_info.model,
            status_info.msg_count,
            thinking,
            queued
        );
        frame.render_widget(
            Paragraph::new(status).style(Style::default().fg(self.theme.status_text)),
            Rect { y: area.y + 3, height: 1, ..area },
        );
    }
}

/// Status bar info passed from App to Renderer for each draw.
pub struct StatusInfo {
    pub provider: String,
    pub model: String,
    pub msg_count: usize,
    pub streaming: bool,
    pub queue_len: usize,
}

/// Convert a Message into terminal lines for native scrollback rendering.
///
/// `prev_role` is the role of the message that precedes this one (if any).
/// An extra blank line is inserted when the role changes between user and
/// non-user (assistant/system), giving consistent visual spacing.
fn msg_to_lines(msg: &Message, theme: &Theme, prev_role: Option<ChatRole>) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Add an extra blank line when transitioning between user ↔ assistant/system.
    if let Some(prev) = prev_role {
        let changed = matches!(
            (prev, msg.role),
            (ChatRole::User, ChatRole::Assistant) |
            (ChatRole::User, ChatRole::System) |
            (ChatRole::Assistant, ChatRole::User) |
            (ChatRole::System, ChatRole::User)
        );
        if changed {
            lines.push(Line::raw(""));
        }
    }

    match msg.role {
        ChatRole::User => {
            for (idx, raw_line) in msg.content.lines().enumerate() {
                if idx == 0 {
                    lines.push(Line::from(vec![
                        Span::styled("> ", Style::default().fg(theme.user_msg)),
                        Span::styled(raw_line.to_string(), Style::default().fg(theme.user_msg)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("  ", Style::default().fg(theme.user_msg)),
                        Span::styled(raw_line.to_string(), Style::default().fg(theme.user_msg)),
                    ]));
                }
            }
        }
        ChatRole::Assistant => {
            for raw_line in msg.content.lines() {
                lines.push(Line::raw(raw_line.to_string()));
            }
        }
        ChatRole::System => {
            for raw_line in msg.content.lines() {
                lines.push(Line::from(Span::styled(raw_line.to_string(), Style::default().fg(theme.system_msg))));
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::raw(""));
    }

    lines.push(Line::raw(""));
    lines
}

/// Shorten a directory path to `first/.../last` for the banner display.
fn format_short_dir(path: &Path) -> String {
    let components: Vec<&std::ffi::OsStr> = path.iter().collect();
    if components.len() > 2 {
        let first = components[0].to_string_lossy();
        let last = components.last().unwrap().to_string_lossy();
        // Avoid double-slash when first component is "/" (Linux root).
        let sep = if first.ends_with('/') || first.ends_with('\\') {
            ""
        } else {
            "/"
        };
        format!("{first}{sep}.../{last}")
    } else {
        path.display().to_string()
    }
}
