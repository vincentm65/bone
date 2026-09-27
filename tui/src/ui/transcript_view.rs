//! Fullscreen transcript viewer for conversation and sub-agent transcripts.

use std::io;

use bone_render::screens::transcript::{MOUSE_WHEEL_LINES, TranscriptScreen};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
};

use crate::chat::Message;
use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::theme::Theme;

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
            bone_core::ext::ctx::runtime_warn(format!(
                "bone: warning: failed to disable mouse capture: {e}"
            ));
        }
    }
}

pub fn run(messages: &[Message], theme: &Theme) -> io::Result<()> {
    run_with_shell_outputs(messages, theme, true)
}

pub fn run_collapsed(messages: &[Message], theme: &Theme) -> io::Result<()> {
    run_with_shell_outputs(messages, theme, false)
}

fn run_with_shell_outputs(
    messages: &[Message],
    theme: &Theme,
    expanded_shell_outputs: bool,
) -> io::Result<()> {
    fullscreen::run(|term| {
        let _mouse_guard = MouseCaptureGuard::enable()?;
        run_loop(term, messages, theme, expanded_shell_outputs)
    })
}

fn run_loop(
    term: &mut FullscreenTerminal,
    messages: &[Message],
    theme: &Theme,
    expanded_shell_outputs: bool,
) -> io::Result<()> {
    let mut screen = TranscriptScreen::new(messages.to_vec(), expanded_shell_outputs);
    term.draw(|frame| screen.draw(frame, theme))?;
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if screen.handle_key(crate::ui::screen_key(key)) {
                    break;
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => screen.scroll_by(-(MOUSE_WHEEL_LINES as i64)),
                MouseEventKind::ScrollDown => screen.scroll_by(MOUSE_WHEEL_LINES as i64),
                _ => continue,
            },
            Event::Resize(..) => {}
            _ => continue,
        }
        term.draw(|frame| screen.draw(frame, theme))?;
    }
    Ok(())
}
