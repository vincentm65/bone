//! Live-output viewer for a host-managed background process.

use bone_protocol::{ProcessSnapshot, ProcessState};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{Key, KeyCode};
use crate::theme::Theme;
use crate::wrap::wrap_text;

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn process_state_label(state: ProcessState) -> &'static str {
    match state {
        ProcessState::Running => "running",
        ProcessState::Exited => "exited",
        ProcessState::TimedOut => "timed out",
        ProcessState::Cancelled => "cancelled",
    }
}

fn format_elapsed_ms(elapsed_ms: u64) -> String {
    if elapsed_ms < 60_000 {
        format!("{}s", elapsed_ms / 1000)
    } else {
        format!("{}m{}s", elapsed_ms / 60_000, (elapsed_ms / 1000) % 60)
    }
}

fn elapsed_ms(process: &ProcessSnapshot) -> u64 {
    if process.started_at == 0 {
        return 0;
    }
    process
        .finished_at
        .unwrap_or_else(now_millis)
        .saturating_sub(process.started_at)
}

fn format_elapsed(process: &ProcessSnapshot) -> String {
    format_elapsed_ms(elapsed_ms(process))
}

/// What the frontend must do after a key.
#[derive(Debug, PartialEq, Eq)]
pub enum ProcessAction {
    None,
    Close,
    /// Ask the daemon to cancel this process.
    Cancel(String),
}

pub struct ProcessScreen {
    process: ProcessSnapshot,
    scroll: usize,
    follow: bool,
    height: usize,
    max_scroll: usize,
}

impl ProcessScreen {
    pub fn new(process: ProcessSnapshot) -> Self {
        Self {
            process,
            scroll: 0,
            follow: true,
            height: 1,
            max_scroll: 0,
        }
    }

    pub fn id(&self) -> &str {
        &self.process.id
    }

    pub fn command(&self) -> &str {
        &self.process.command
    }

    pub fn running(&self) -> bool {
        self.process.running
    }

    /// Apply a fresh process list. Returns false when the process is gone and
    /// the viewer should close.
    pub fn update(&mut self, processes: &[ProcessSnapshot]) -> bool {
        match processes
            .iter()
            .find(|candidate| candidate.id == self.process.id)
        {
            Some(next) => {
                self.process = next.clone();
                true
            }
            None => false,
        }
    }

    pub fn handle_key(&mut self, key: Key) -> ProcessAction {
        match key.code {
            KeyCode::Char('c') if key.ctrl && self.process.running => {
                return ProcessAction::Cancel(self.process.id.clone());
            }
            KeyCode::Char('q') | KeyCode::Esc => return ProcessAction::Close,
            KeyCode::Char('o') if key.ctrl => return ProcessAction::Close,
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll = self.scroll.saturating_add(1).min(self.max_scroll);
                self.follow = self.scroll == self.max_scroll;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
                self.follow = false;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(self.height).min(self.max_scroll);
                self.follow = self.scroll == self.max_scroll;
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(self.height);
                self.follow = false;
            }
            KeyCode::Home => {
                self.scroll = 0;
                self.follow = false;
            }
            KeyCode::End => {
                self.scroll = self.max_scroll;
                self.follow = true;
            }
            _ => {}
        }
        ProcessAction::None
    }

    pub fn draw(&mut self, frame: &mut ratatui::Frame, theme: &Theme) {
        let size = frame.area();
        let lines = process_lines(&self.process, size.width as usize, theme);
        self.height = size.height.saturating_sub(1) as usize;
        self.max_scroll = lines.len().saturating_sub(self.height);
        self.scroll = if self.follow {
            self.max_scroll
        } else {
            self.scroll.min(self.max_scroll)
        };
        let mut surface = Style::default().fg(theme.palette.fg);
        if let Some(bg) = theme.palette.bg {
            surface = surface.bg(bg);
        }
        frame.render_widget(ratatui::widgets::Block::default().style(surface), size);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(size);
        let visible = lines
            .iter()
            .skip(self.scroll)
            .take(chunks[0].height as usize)
            .cloned()
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible), chunks[0]);
        let cancel = matches!(self.process.state, ProcessState::Running)
            .then_some(" · Ctrl+C cancel")
            .unwrap_or("");
        let state = process_state_label(self.process.state);
        let elapsed = format_elapsed(&self.process);
        let follow = if self.follow { " · following" } else { "" };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(
                    "{state} · {elapsed}{follow}{cancel} · ↑/↓ PgUp/PgDn Home/End scroll · q/Esc/Ctrl+O close"
                ),
                Style::default().fg(theme.palette.muted),
            ))),
            chunks[1],
        );
    }
}

pub fn process_lines(process: &ProcessSnapshot, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("$ ", Style::default().fg(theme.shell_separator)),
        Span::styled(
            process.command.clone(),
            Style::default()
                .fg(theme.shell_program)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    append_output(&mut lines, &process.stdout, width, theme.palette.fg);
    append_output(&mut lines, &process.stderr, width, theme.tool_error);
    if let Some(error) = &process.error {
        append_output(&mut lines, error, width, theme.tool_error);
    }
    if !process.running {
        if let Some(code) = process.exit_code {
            append_output(
                &mut lines,
                &format!("exit code: {code}"),
                width,
                theme.palette.muted,
            );
        }
        if let Some(signal) = process.signal {
            append_output(
                &mut lines,
                &format!("signal: {signal}"),
                width,
                theme.palette.muted,
            );
        }
        append_output(
            &mut lines,
            &format!("state: {}", process_state_label(process.state)),
            width,
            theme.palette.muted,
        );
        append_output(
            &mut lines,
            &format!("elapsed: {}", format_elapsed(process)),
            width,
            theme.palette.muted,
        );
        if process.exit_code.is_none() && process.signal.is_none() && process.error.is_none() {
            append_output(&mut lines, "finished", width, theme.palette.muted);
        }
    }
    lines
}

fn append_output(
    lines: &mut Vec<Line<'static>>,
    output: &str,
    width: usize,
    color: ratatui::style::Color,
) {
    for logical in output.lines() {
        for visual in wrap_text(logical, width) {
            lines.push(Line::from(Span::styled(visual, Style::default().fg(color))));
        }
    }
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
