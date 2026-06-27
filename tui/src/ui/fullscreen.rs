//! Shared scaffolding for fullscreen TUI takeovers (`/stats`, the onboarding
//! wizard). Both enter the alternate screen, run an event loop against a
//! [`BoneBackend`] terminal, and restore the terminal on exit — this owns that
//! setup/teardown so each screen only writes its own draw + key handling.

use std::io;

use crossterm::style::{Attribute, SetAttribute};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;

use crate::ui::render::backend::BoneBackend;

/// Terminal type the fullscreen screens draw into.
pub type FullscreenTerminal = Terminal<BoneBackend<io::Stdout>>;

/// RAII guard that enables raw mode and disables it on drop (only if this guard
/// was the one that enabled it).
struct RawModeGuard {
    was_enabled: bool,
}

impl RawModeGuard {
    fn enable() -> io::Result<Self> {
        let was_enabled = crossterm::terminal::is_raw_mode_enabled()?;
        if !was_enabled {
            crossterm::terminal::enable_raw_mode()?;
        }
        Ok(Self { was_enabled })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if !self.was_enabled
            && let Err(e) = crossterm::terminal::disable_raw_mode()
        {
            eprintln!("bone: warning: failed to disable raw mode: {e}");
        }
    }
}

/// Run `body` as a fullscreen takeover: enable raw mode, enter the alternate
/// screen, build the terminal, run `body`, then always restore the terminal
/// (leave alt-screen, reset attributes) regardless of how `body` returned. The
/// body's error is surfaced before any teardown error.
pub fn run<T>(body: impl FnOnce(&mut FullscreenTerminal) -> io::Result<T>) -> io::Result<T> {
    let _raw_guard = RawModeGuard::enable()?;

    // Inner closure so teardown always runs, even if terminal setup or `body`
    // fails partway through.
    let result = (|| -> io::Result<T> {
        crossterm::execute!(
            io::stdout(),
            SetAttribute(Attribute::Reset),
            EnterAlternateScreen
        )?;
        let backend = BoneBackend::new(io::stdout());
        let mut term = Terminal::new(backend)?;
        body(&mut term)
    })();

    let leave = crossterm::execute!(
        io::stdout(),
        SetAttribute(Attribute::Reset),
        LeaveAlternateScreen,
        SetAttribute(Attribute::Reset)
    );

    let value = result?;
    leave?;
    Ok(value)
}
