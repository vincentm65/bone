//! Fullscreen live-output viewer for host-managed background processes.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::render::wrap::wrap_text;

pub fn run(
    process: bone_protocol::ProcessSnapshot,
    command_tx: tokio::sync::mpsc::UnboundedSender<bone_protocol::RuntimeCommand>,
    events_rx: tokio::sync::broadcast::Receiver<bone_protocol::RuntimeEvent>,
) -> io::Result<()> {
    let _ = command_tx.send(bone_protocol::RuntimeCommand::GetProcesses);
    fullscreen::run(|term| run_loop(term, process, &command_tx, events_rx))
}

fn run_loop(
    term: &mut FullscreenTerminal,
    mut process: bone_protocol::ProcessSnapshot,
    command_tx: &tokio::sync::mpsc::UnboundedSender<bone_protocol::RuntimeCommand>,
    mut events_rx: tokio::sync::broadcast::Receiver<bone_protocol::RuntimeEvent>,
) -> io::Result<()> {
    let mut scroll = 0;
    let mut follow = true;

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
                    process = next;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    let _ = command_tx.send(bone_protocol::RuntimeCommand::GetProcesses);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return Ok(()),
            }
        }
        let size = term.size()?;
        let lines = process_lines(&process, size.width as usize);
        let height = size.height.saturating_sub(1) as usize;
        let max_scroll = lines.len().saturating_sub(height);
        if follow {
            scroll = max_scroll;
        } else {
            scroll = scroll.min(max_scroll);
        }
        draw(term, &lines, scroll, process.running, follow)?;

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
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    follow = false;
                }
                KeyCode::PageDown => {
                    scroll = scroll.saturating_add(height).min(max_scroll);
                    follow = scroll == max_scroll;
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(height);
                    follow = false;
                }
                KeyCode::Home => {
                    scroll = 0;
                    follow = false;
                }
                KeyCode::End => {
                    scroll = max_scroll;
                    follow = true;
                }
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(())
}

fn process_lines(process: &bone_protocol::ProcessSnapshot, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("$ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            process.command.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    append_output(&mut lines, &process.stdout, width, Color::Gray);
    append_output(&mut lines, &process.stderr, width, Color::Red);
    if let Some(error) = &process.error {
        append_output(&mut lines, error, width, Color::Red);
    }
    if !process.running {
        if let Some(code) = process.exit_code {
            append_output(
                &mut lines,
                &format!("exit code: {code}"),
                width,
                Color::DarkGray,
            );
        }
        if let Some(signal) = process.signal {
            append_output(
                &mut lines,
                &format!("signal: {signal}"),
                width,
                Color::DarkGray,
            );
        }
        if process.exit_code.is_none() && process.signal.is_none() && process.error.is_none() {
            append_output(&mut lines, "finished", width, Color::DarkGray);
        }
    }
    lines
}

fn append_output(lines: &mut Vec<Line<'static>>, output: &str, width: usize, color: Color) {
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
) -> io::Result<()> {
    term.draw(|frame| {
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
                Style::default().fg(Color::DarkGray),
            ))),
            chunks[1],
        );
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_output_keeps_stdout_and_stderr_styles() {
        let process = bone_protocol::ProcessSnapshot {
            id: "process-1".into(),
            command: "build".into(),
            owner: "conversation:1".into(),
            running: true,
            stdout: "out".into(),
            stderr: "err".into(),
            exit_code: None,
            signal: None,
            error: None,
        };
        let lines = process_lines(&process, 80);

        assert_eq!(lines[1].style.fg, None);
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Gray));
        assert_eq!(lines[2].spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn completed_process_renders_exit_and_signal_metadata() {
        let process = bone_protocol::ProcessSnapshot {
            id: "process-1".into(),
            command: "build".into(),
            owner: "conversation:1".into(),
            running: false,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(143),
            signal: Some(15),
            error: None,
        };
        let lines = process_lines(&process, 80);

        assert_eq!(lines[1].to_string(), "exit code: 143");
        assert_eq!(lines[2].to_string(), "signal: 15");
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::DarkGray));
    }
}
