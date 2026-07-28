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
    disable_on_restore: bool,
}

impl RawModeGuard {
    fn enable() -> io::Result<Self> {
        let was_enabled = crossterm::terminal::is_raw_mode_enabled()?;
        if !was_enabled {
            crossterm::terminal::enable_raw_mode()?;
        }
        Ok(Self {
            disable_on_restore: !was_enabled,
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.disable_on_restore {
            return Ok(());
        }
        crossterm::terminal::disable_raw_mode()?;
        self.disable_on_restore = false;
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.restore()
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            bone_core::ext::ctx::runtime_warn(format!(
                "bone: warning: failed to disable raw mode: {e}"
            ));
        }
    }
}

/// Owns the alternate screen from successful entry through explicit or
/// best-effort restoration.
struct AlternateScreenGuard {
    entered: bool,
}

impl AlternateScreenGuard {
    fn enter() -> io::Result<Self> {
        crossterm::execute!(
            io::stdout(),
            SetAttribute(Attribute::Reset),
            EnterAlternateScreen
        )?;
        Ok(Self { entered: true })
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.entered {
            return Ok(());
        }
        crossterm::execute!(
            io::stdout(),
            SetAttribute(Attribute::Reset),
            LeaveAlternateScreen,
            SetAttribute(Attribute::Reset)
        )?;
        self.entered = false;
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.restore()
    }
}

impl Drop for AlternateScreenGuard {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            bone_core::ext::ctx::runtime_warn(format!(
                "bone: warning: failed to leave alternate screen: {e}"
            ));
        }
    }
}

/// Run `body` as a fullscreen takeover: enable raw mode, enter the alternate
/// screen, build the terminal, run `body`, then always restore the terminal
/// (leave alt-screen, reset attributes) regardless of how `body` returned. The
/// body's error is surfaced before any teardown error.
pub fn run<T>(body: impl FnOnce(&mut FullscreenTerminal) -> io::Result<T>) -> io::Result<T> {
    let raw_guard = RawModeGuard::enable()?;
    let screen_guard = AlternateScreenGuard::enter()?;

    let result = (|| -> io::Result<T> {
        let backend = BoneBackend::new(io::stdout());
        let mut term = Terminal::new(backend)?;
        body(&mut term)
    })();

    // Run every cleanup step before selecting which error to return.
    let screen_result = screen_guard.finish();
    let raw_result = raw_guard.finish();

    let value = result?;
    screen_result?;
    raw_result?;
    Ok(value)
}
