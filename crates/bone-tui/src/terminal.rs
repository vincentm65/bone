//! Terminal setup and restore. The guard restores on drop, and a panic hook
//! restores before the panic message prints.

use std::io::{self, BufWriter, Stdout};

use crossterm::cursor::Hide;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement,
};
use crossterm::{execute, queue};
use ratatui::Terminal;
use ratatui::backend::{ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// Frames are written whole: std's stdout flushes every 1 KB, which would
/// split a full redraw into dozens of writes.
type Backend = Anchored<CrosstermBackend<BufWriter<Stdout>>>;

/// Moves the cursor to the next cell after any cell whose width terminals
/// may not agree on (emoji, CJK, combining marks, symbols newer than our
/// width table). Crossterm writes a row's cells back to back, so one glyph
/// the terminal draws a column wider or narrower than ratatui thought
/// shifts the rest of the row over the next pane, and the diff renderer
/// never repaints what it overwrote.
pub struct Anchored<B>(B);

/// Text every terminal draws one column wide.
fn plain(symbol: &str) -> bool {
    let mut chars = symbol.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return false;
    };
    matches!(c,
        ' '..='~' | '\u{A0}'..='\u{AC}' | '\u{AE}'..='\u{24F}'
        | '\u{2010}'..='\u{2027}' | '\u{2500}'..='\u{259F}' | '\u{2800}'..='\u{28FF}')
}

impl<B: ratatui::backend::Backend> ratatui::backend::Backend for Anchored<B> {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        // Each draw starts with a cursor move.
        let mut run = Vec::new();
        for cell in content {
            let plain = plain(cell.2.symbol());
            run.push(cell);
            if !plain {
                self.0.draw(run.drain(..))?;
            }
        }
        self.0.draw(run.into_iter())
    }
    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.0.append_lines(n)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.0.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.0.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.0.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.0.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.0.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.0.clear_region(clear_type)
    }
    fn size(&self) -> io::Result<Size> {
        self.0.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.0.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<B: io::Write> io::Write for Anchored<B> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

pub struct Guard {
    terminal: Terminal<Backend>,
    enhanced: bool,
    /// tmux repaints the whole pane after every synchronized update, which
    /// sends the outer terminal 30x or more the bytes of the frame itself.
    sync: bool,
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
    let mut terminal = Terminal::new(Anchored(CrosstermBackend::new(out)))?;
    terminal.clear()?;
    Ok(Guard {
        terminal,
        enhanced,
        sync: std::env::var_os("TMUX").is_none(),
    })
}

impl Guard {
    /// Draw a frame as one synchronized update (outside tmux), which
    /// terminals that support it show only once it is complete. The cursor
    /// is hidden while cells are written (SSH can drop the update, and it
    /// would flicker).
    pub fn draw_frame(&mut self, render: impl FnOnce(&mut ratatui::Frame)) -> io::Result<()> {
        if self.sync {
            queue!(self.terminal.backend_mut(), BeginSynchronizedUpdate)?;
        }
        queue!(self.terminal.backend_mut(), Hide)?;
        self.terminal.draw(|f| {
            render(f);
            widen(f.buffer_mut());
        })?;
        crate::stats::mark("draw");
        if self.sync {
            execute!(self.terminal.backend_mut(), EndSynchronizedUpdate)?;
        }
        Ok(())
    }
}

/// Symbols ratatui's width table (Unicode 15, pinned) has one column wide
/// and newer ones, which tmux and up to date terminals use, two: emoji
/// modifier bases, trigrams, newer emoji.
fn wide_now(c: char) -> bool {
    matches!(c,
        '\u{261D}' | '\u{2630}'..='\u{2637}' | '\u{268A}'..='\u{268F}' | '\u{26F9}'
        | '\u{270C}'..='\u{270D}' | '\u{31E4}'..='\u{31E5}' | '\u{3248}'..='\u{324F}'
        | '\u{1F3CB}'..='\u{1F3CC}' | '\u{1F574}'..='\u{1F575}' | '\u{1F590}' | '\u{1F6D8}'
        | '\u{1FA89}'..='\u{1FA8F}' | '\u{1FABE}' | '\u{1FAC6}'..='\u{1FACD}'
        | '\u{1FADC}'..='\u{1FADF}' | '\u{1FAE9}'..='\u{1FAEF}')
}

/// Draw [`wide_now`] symbols as the terminal will: over a blank after
/// them, which is then skipped as a wide char's second cell; else as a
/// box. Otherwise the terminal's second column covers the next cell, which
/// ratatui does not know to repaint.
fn widen(buf: &mut ratatui::buffer::Buffer) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let mut chars = buf[(x, y)].symbol().chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                x += 1;
                continue;
            };
            if wide_now(c) {
                let room = x + 1 < area.right() && buf[(x + 1, y)].symbol() == " ";
                if room {
                    buf[(x + 1, y)].set_skip(true);
                    x += 1;
                } else {
                    buf[(x, y)].set_char('□');
                }
            }
            x += 1;
        }
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
