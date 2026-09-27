//! Fullscreen live-output viewer for host-managed background processes.

use std::io;
use std::time::{Duration, Instant};

use bone_render::screens::process::{ProcessAction, ProcessScreen};
use crossterm::event::{self, Event, KeyEventKind};

use crate::ui::fullscreen::{self, FullscreenTerminal};

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
    process: bone_protocol::ProcessSnapshot,
    command_tx: &tokio::sync::mpsc::UnboundedSender<bone_protocol::RuntimeCommand>,
    mut events_rx: tokio::sync::broadcast::Receiver<bone_protocol::RuntimeEvent>,
    theme: &crate::ui::theme::Theme,
) -> io::Result<()> {
    let mut screen = ProcessScreen::new(process);
    term.draw(|frame| screen.draw(frame, theme))?;
    let mut last_redraw = Instant::now();
    let mut dirty = false;

    loop {
        loop {
            match events_rx.try_recv() {
                Ok(bone_protocol::RuntimeEvent::ProcessesSnapshot { processes, .. }) => {
                    if !screen.update(&processes) {
                        return Ok(());
                    }
                    dirty = true;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    let _ = command_tx.send(bone_protocol::RuntimeCommand::GetProcesses);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return Ok(()),
            }
        }
        if dirty || (screen.running() && last_redraw.elapsed() >= Duration::from_secs(1)) {
            term.draw(|frame| screen.draw(frame, theme))?;
            last_redraw = Instant::now();
            dirty = false;
        }

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match screen.handle_key(crate::ui::screen_key(key)) {
                    ProcessAction::Close => break,
                    ProcessAction::Cancel(id) => {
                        let _ =
                            command_tx.send(bone_protocol::RuntimeCommand::CancelProcess { id });
                    }
                    ProcessAction::None => {}
                }
                dirty = true;
            }
            Event::Resize(_, _) => dirty = true,
            _ => {}
        }
    }
    Ok(())
}
