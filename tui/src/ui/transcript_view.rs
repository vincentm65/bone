//! Fullscreen transcript viewer with shell outputs expanded.

use std::io;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::chat::Message;
use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::render::messages::msg_to_lines;
use crate::ui::theme::Theme;

const MOUSE_WHEEL_LINES: usize = 3;

struct MouseCaptureGuard;

impl MouseCaptureGuard {
    fn enable() -> io::Result<Self> {
        crossterm::execute!(io::stdout(), EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for MouseCaptureGuard {
    fn drop(&mut self) {
        if let Err(e) = crossterm::execute!(io::stdout(), DisableMouseCapture) {
            eprintln!("bone: warning: failed to disable mouse capture: {e}");
        }
    }
}

pub fn run(messages: &[Message], theme: &Theme) -> io::Result<()> {
    fullscreen::run(|term| {
        let _mouse_guard = MouseCaptureGuard::enable()?;
        run_loop(term, messages, theme)
    })
}

fn run_loop(term: &mut FullscreenTerminal, messages: &[Message], theme: &Theme) -> io::Result<()> {
    let mut width = term.size()?.width.max(1);
    let mut lines = msg_to_lines(messages, theme, None, width, true);
    let mut scroll = initial_scroll(&lines, term.size()?.height);

    draw(term, &lines, scroll)?;
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let height = view_height(term.size()?.height);
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll = scroll.saturating_add(height),
                    KeyCode::PageUp => scroll = scroll.saturating_sub(height),
                    KeyCode::Home => scroll = 0,
                    KeyCode::End => scroll = max_scroll(&lines, height),
                    _ => continue,
                }
                scroll = scroll.min(max_scroll(&lines, height));
            }
            Event::Mouse(mouse) => {
                let height = view_height(term.size()?.height);
                match mouse.kind {
                    MouseEventKind::ScrollUp => scroll = scroll.saturating_sub(MOUSE_WHEEL_LINES),
                    MouseEventKind::ScrollDown => scroll = scroll.saturating_add(MOUSE_WHEEL_LINES),
                    _ => continue,
                }
                scroll = scroll.min(max_scroll(&lines, height));
            }
            Event::Resize(new_width, _) => {
                width = new_width.max(1);
                lines = msg_to_lines(messages, theme, None, width, true);
                scroll = scroll.min(max_scroll(&lines, view_height(term.size()?.height)));
            }
            _ => continue,
        }
        draw(term, &lines, scroll)?;
    }
    Ok(())
}

fn draw(term: &mut FullscreenTerminal, lines: &[Line<'static>], scroll: usize) -> io::Result<()> {
    term.draw(|frame| {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(frame.area());
        let height = chunks[0].height as usize;
        let visible = lines
            .iter()
            .skip(scroll)
            .take(height)
            .cloned()
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible), chunks[0]);
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "↑/↓ PgUp/PgDn Home/End scroll · q/Esc/Ctrl+O close",
                Style::default().fg(Color::DarkGray),
            )])),
            chunks[1],
        );
    })?;
    Ok(())
}

fn initial_scroll(lines: &[Line<'static>], terminal_height: u16) -> usize {
    max_scroll(lines, view_height(terminal_height))
}

fn view_height(terminal_height: u16) -> usize {
    terminal_height.saturating_sub(1) as usize
}

fn max_scroll(lines: &[Line<'static>], height: usize) -> usize {
    lines.len().saturating_sub(height)
}
