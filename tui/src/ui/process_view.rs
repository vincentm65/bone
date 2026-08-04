//! Fullscreen live-output viewer for host-managed background processes.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::render::wrap::wrap_text;

pub fn run(
    process: bone_protocol::ProcessSnapshot,
    command_tx: tokio::sync::mpsc::UnboundedSender<bone_protocol::RuntimeCommand>,
    events_rx: tokio::sync::broadcast::Receiver<bone_protocol::RuntimeEvent>,
    theme: &crate::ui::theme::Theme,
) -> io::Result<()> {
    let _ = command_tx.send(bone_protocol::RuntimeCommand::GetProcesses);
    fullscreen::run(|term| run_loop(term, process, &command_tx, events_rx, theme))
}

fn run_loop(
    term: &mut FullscreenTerminal,
    mut process: bone_protocol::ProcessSnapshot,
    command_tx: &tokio::sync::mpsc::UnboundedSender<bone_protocol::RuntimeCommand>,
    mut events_rx: tokio::sync::broadcast::Receiver<bone_protocol::RuntimeEvent>,
    theme: &crate::ui::theme::Theme,
) -> io::Result<()> {
    let mut scroll = 0;
    let mut follow = true;
    let (mut height, mut max_scroll) = redraw(term, &process, &mut scroll, follow, theme)?;
    let mut dirty = false;

    loop {
        loop {
            match events_rx.try_recv() {
                Ok(bone_protocol::RuntimeEvent::ProcessesSnapshot { processes, .. }) => {
                    let Some(next) = processes
                        .into_iter()
                        .find(|candidate| candidate.id == process.id)
                    else {
                        return Ok(());
                    };
                    if next != process {
                        process = next;
                        dirty = true;
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    let _ = command_tx.send(bone_protocol::RuntimeCommand::GetProcesses);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return Ok(()),
            }
        }
        if dirty {
            (height, max_scroll) = redraw(term, &process, &mut scroll, follow, theme)?;
            dirty = false;
        }

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let _ = command_tx.send(bone_protocol::RuntimeCommand::CancelProcess {
                        id: process.id.clone(),
                    });
                }
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = scroll.saturating_add(1).min(max_scroll);
                    follow = scroll == max_scroll;
                    dirty = true;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    follow = false;
                    dirty = true;
                }
                KeyCode::PageDown => {
                    scroll = scroll.saturating_add(height).min(max_scroll);
                    follow = scroll == max_scroll;
                    dirty = true;
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(height);
                    follow = false;
                    dirty = true;
                }
                KeyCode::Home => {
                    scroll = 0;
                    follow = false;
                    dirty = true;
                }
                KeyCode::End => {
                    scroll = max_scroll;
                    follow = true;
                    dirty = true;
                }
                _ => {}
            },
            Event::Resize(_, _) => dirty = true,
            _ => {}
        }
    }
    Ok(())
}

fn redraw(
    term: &mut FullscreenTerminal,
    process: &bone_protocol::ProcessSnapshot,
    scroll: &mut usize,
    follow: bool,
    theme: &crate::ui::theme::Theme,
) -> io::Result<(usize, usize)> {
    let size = term.size()?;
    let lines = process_lines(process, size.width as usize, theme);
    let height = size.height.saturating_sub(1) as usize;
    let max_scroll = lines.len().saturating_sub(height);
    if follow {
        *scroll = max_scroll;
    } else {
        *scroll = (*scroll).min(max_scroll);
    }
    draw(term, &lines, *scroll, process.running, follow, theme)?;
    Ok((height, max_scroll))
}

fn process_lines(
    process: &bone_protocol::ProcessSnapshot,
    width: usize,
    theme: &crate::ui::theme::Theme,
) -> Vec<Line<'static>> {
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

fn draw(
    term: &mut FullscreenTerminal,
    lines: &[Line<'static>],
    scroll: usize,
    running: bool,
    follow: bool,
    theme: &crate::ui::theme::Theme,
) -> io::Result<()> {
    term.draw(|frame| {
        let mut surface = Style::default().fg(theme.palette.fg);
        if let Some(bg) = theme.palette.bg {
            surface = surface.bg(bg);
        }
        frame.render_widget(
            ratatui::widgets::Block::default().style(surface),
            frame.area(),
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(frame.area());
        let visible = lines
            .iter()
            .skip(scroll)
            .take(chunks[0].height as usize)
            .cloned()
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible), chunks[0]);
        let state = if running { "running" } else { "finished" };
        let follow = if follow { " · following" } else { "" };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(
                    "{state}{follow} · ↑/↓ PgUp/PgDn Home/End scroll · Ctrl+C cancel · q/Esc/Ctrl+O close"
                ),
                Style::default().fg(theme.palette.muted),
            ))),
            chunks[1],
        );
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "process_view_tests.rs"]
mod tests;
