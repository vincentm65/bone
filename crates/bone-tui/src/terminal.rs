//! Terminal setup and restore. The guard restores on drop, and a panic hook
//! restores before the panic message prints.

use std::io::{self, BufWriter, Stdout};

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement,
};
use crossterm::cursor::Hide;
use crossterm::{execute, queue};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// Frames are written whole: std's stdout flushes every 1 KB, which would
/// split a full redraw into dozens of writes.
type Backend = CrosstermBackend<BufWriter<Stdout>>;

pub struct Guard {
    terminal: Terminal<Backend>,
    enhanced: bool,
}

impl std::ops::Deref for Guard {
    type Target = Terminal<Backend>;
    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl std::ops::DerefMut for Guard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

pub fn enter() -> io::Result<Guard> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore(true);
        hook(info);
    }));
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
    // Lets terminals that support it report Shift+Enter and friends.
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let out = BufWriter::with_capacity(256 * 1024, out);
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    terminal.clear()?;
    Ok(Guard { terminal, enhanced })
}

impl Guard {
    /// Draw a frame as one synchronized update, which terminals that support
    /// it show only once it is complete. The cursor is hidden while cells
    /// are written (tmux/SSH drop the update, and it would flicker).
    pub fn draw_frame(&mut self, render: impl FnOnce(&mut ratatui::Frame)) -> io::Result<()> {
        queue!(self.terminal.backend_mut(), BeginSynchronizedUpdate, Hide)?;
        self.terminal.draw(render)?;
        execute!(self.terminal.backend_mut(), EndSynchronizedUpdate)
    }
}

/// Report mouse buttons, drags and the wheel (SGR encoding), or stop so the
/// terminal's own selection works.
pub fn set_mouse(on: bool) -> io::Result<()> {
    let seq = if on {
        "\x1b[?1000h\x1b[?1002h\x1b[?1006h"
    } else {
        "\x1b[?1006l\x1b[?1002l\x1b[?1000l"
    };
    execute!(io::stdout(), crossterm::style::Print(seq))
}

fn restore(enhanced: bool) {
    let mut out = io::stdout();
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        crossterm::style::Print("\x1b[?1006l\x1b[?1002l\x1b[?1000l"),
        DisableBracketedPaste,
        crossterm::cursor::SetCursorStyle::DefaultUserShape,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = disable_raw_mode();
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore(self.enhanced);
    }
}
