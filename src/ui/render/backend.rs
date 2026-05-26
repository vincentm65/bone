use std::io::{self, Write};

use crossterm::cursor::MoveTo;
use crossterm::style::{Color as CrosstermColor, ResetColor, SetBackgroundColor};
use crossterm::terminal::{Clear, ClearType as CrosstermClearType};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Modifier};

/// Crossterm backend that paints trailing background fill using EL instead of spaces.
///
/// This lets scrollback rows retain full-width backgrounds without placing
/// printable padding in copied terminal content or advancing through the last
/// terminal column.
pub struct BoneBackend<W: Write> {
    pub inner: CrosstermBackend<W>,
}

impl<W: Write> BoneBackend<W> {
    pub const fn new(writer: W) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
        }
    }

    fn clear_background_suffix(&mut self, x: u16, y: u16, bg: Color) -> io::Result<()> {
        crossterm::queue!(
            self.inner.writer_mut(),
            MoveTo(x, y),
            SetBackgroundColor(to_crossterm_color(bg)),
            Clear(CrosstermClearType::UntilNewLine),
            ResetColor
        )
    }
}

impl<W: Write> Write for BoneBackend<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
}

impl<W: Write> Backend for BoneBackend<W> {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let cells = content.collect::<Vec<_>>();
        let mut pending_start = 0;
        let mut row_start = 0;

        while row_start < cells.len() {
            let y = cells[row_start].1;
            let mut row_end = row_start + 1;
            while row_end < cells.len() && cells[row_end].1 == y {
                row_end += 1;
            }

            if let Some(fill_start) = background_suffix_start(&cells[row_start..row_end]) {
                let fill_start = row_start + fill_start;
                if pending_start < fill_start {
                    self.inner
                        .draw(cells[pending_start..fill_start].iter().copied())?;
                }
                let (x, y, cell) = cells[fill_start];
                self.clear_background_suffix(x, y, cell.bg)?;
                pending_start = row_end;
            }

            row_start = row_end;
        }

        if pending_start < cells.len() {
            self.inner.draw(cells[pending_start..].iter().copied())?;
        }
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }

    fn scroll_region_up(
        &mut self,
        region: std::ops::Range<u16>,
        line_count: u16,
    ) -> io::Result<()> {
        self.inner.scroll_region_up(region, line_count)
    }

    fn scroll_region_down(
        &mut self,
        region: std::ops::Range<u16>,
        line_count: u16,
    ) -> io::Result<()> {
        self.inner.scroll_region_down(region, line_count)
    }
}

fn background_suffix_start(row: &[(u16, u16, &Cell)]) -> Option<usize> {
    let (_, _, last) = row.last()?;
    if !is_background_fill(last) {
        return None;
    }
    let bg = last.bg;
    let mut start = row.len() - 1;
    while start > 0
        && row[start - 1].0 + 1 == row[start].0
        && row[start - 1].1 == row[start].1
        && is_background_fill(row[start - 1].2)
        && row[start - 1].2.bg == bg
    {
        start -= 1;
    }
    Some(start)
}

fn is_background_fill(cell: &Cell) -> bool {
    cell.symbol() == " "
        && cell.fg == Color::Reset
        && cell.bg != Color::Reset
        && cell.modifier == Modifier::empty()
}

fn to_crossterm_color(color: Color) -> CrosstermColor {
    match color {
        Color::Reset => CrosstermColor::Reset,
        Color::Black => CrosstermColor::Black,
        Color::Red => CrosstermColor::DarkRed,
        Color::Green => CrosstermColor::DarkGreen,
        Color::Yellow => CrosstermColor::DarkYellow,
        Color::Blue => CrosstermColor::DarkBlue,
        Color::Magenta => CrosstermColor::DarkMagenta,
        Color::Cyan => CrosstermColor::DarkCyan,
        Color::Gray => CrosstermColor::Grey,
        Color::DarkGray => CrosstermColor::DarkGrey,
        Color::LightRed => CrosstermColor::Red,
        Color::LightGreen => CrosstermColor::Green,
        Color::LightBlue => CrosstermColor::Blue,
        Color::LightYellow => CrosstermColor::Yellow,
        Color::LightMagenta => CrosstermColor::Magenta,
        Color::LightCyan => CrosstermColor::Cyan,
        Color::White => CrosstermColor::White,
        Color::Indexed(index) => CrosstermColor::AnsiValue(index),
        Color::Rgb(r, g, b) => CrosstermColor::Rgb { r, g, b },
    }
}


