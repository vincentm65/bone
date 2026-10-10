//! Terminal views of managed shell processes: the output as written (from
//! `process/changed` chunks and `process/read`), played into a `vt100`
//! screen of the size Lua asks for, and given back as lines of styled text
//! (`bone.processes.screen`).

use ratatui::style::{Color, Modifier, Style};

use crate::theme::Theme;
use crate::ui::Item;

/// Output kept per process, like the core keeps.
const MAX_TEXT: usize = 1 << 20;
const SCROLLBACK: usize = 5000;

pub struct Term {
    /// Output as written, from byte `start` up to `end` of the stream.
    text: String,
    start: u64,
    end: u64,
    /// Chunks went missing (or none came yet): read it all again.
    pub stale: bool,
    pub reading: bool,
    /// Chunks received while the full output read is in flight. They are
    /// replayed after the read so an older response cannot erase them.
    pending: Vec<(u64, String)>,
    /// The screen at its size (rows, cols).
    screen: Option<(u16, u16, vt100::Parser)>,
}

impl Default for Term {
    /// Empty, and stale until its output is read.
    fn default() -> Self {
        Term {
            text: String::new(),
            start: 0,
            end: 0,
            stale: true,
            reading: false,
            pending: Vec::new(),
            screen: None,
        }
    }
}

impl Term {
    /// Start a full output read.
    pub fn begin_read(&mut self) {
        self.reading = true;
        self.pending.clear();
    }

    /// A `process/changed` chunk.
    pub fn chunk(&mut self, offset: u64, data: &str) {
        if self.reading {
            self.pending.push((offset, data.to_owned()));
            return;
        }
        self.apply_chunk(offset, data);
    }

    fn apply_chunk(&mut self, offset: u64, data: &str) {
        if offset > self.end || (self.end == 0 && offset > 0) {
            self.stale = true;
            return;
        }
        let skip = (self.end - offset) as usize;
        if skip >= data.len() {
            return;
        }
        let mut skip = skip;
        while !data.is_char_boundary(skip) {
            skip += 1;
        }
        self.push(&data[skip..]);
    }

    /// Complete a full output read and then apply chunks that arrived while
    /// it was in flight.
    pub fn finish_read(&mut self, offset: u64, data: &str) {
        let pending = std::mem::take(&mut self.pending);
        self.load(offset, data);
        self.reading = false;
        for (offset, data) in pending {
            self.apply_chunk(offset, &data);
        }
    }

    /// Keep chunks received before a failed read rather than dropping them.
    pub fn fail_read(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        self.reading = false;
        for (offset, data) in pending {
            self.apply_chunk(offset, &data);
        }
        // A failed read is treated as an unavailable optional feature, as it
        // was before terminal output loading was added.
        self.stale = false;
    }

    /// What `process/read` gave: all of it from `offset`.
    pub fn load(&mut self, offset: u64, data: &str) {
        self.pending.clear();
        self.text.clear();
        self.start = offset;
        self.end = offset;
        self.screen = None;
        self.stale = false;
        self.push(data);
    }

    fn push(&mut self, data: &str) {
        self.text.push_str(data);
        self.end += data.len() as u64;
        if let Some((_, _, p)) = &mut self.screen {
            p.process(data.as_bytes());
        }
        if self.text.len() > MAX_TEXT {
            let mut cut = self.text.len() - MAX_TEXT;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
            self.start += cut as u64;
        }
    }

    /// The screen `rows` × `cols`, scrolled back `scroll` rows: its lines,
    /// how far it did scroll, and how far it could. Styles are registered in
    /// `theme` as `@term…` groups.
    pub fn lines(
        &mut self,
        rows: u16,
        cols: u16,
        scroll: usize,
        theme: &mut Theme,
    ) -> (Vec<Vec<Item>>, usize, usize) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if !matches!(&self.screen, Some((r, c, _)) if (*r, *c) == (rows, cols)) {
            // vt100 does not reflow: play everything again at the new size.
            let mut p = vt100::Parser::new(rows, cols, SCROLLBACK);
            p.process(self.text.as_bytes());
            self.screen = Some((rows, cols, p));
        }
        let (_, _, p) = self.screen.as_mut().unwrap();
        p.set_scrollback(usize::MAX);
        let max = p.screen().scrollback();
        let scroll = scroll.min(max);
        p.set_scrollback(scroll);
        let normal = theme.hl("Normal");
        let screen = p.screen();
        let mut out = Vec::with_capacity(rows as usize);
        for row in 0..rows {
            let mut line: Vec<Item> = Vec::new();
            let mut run = String::new();
            let mut run_style: Option<String> = None;
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let group = group_of(cell, normal, theme);
                if run_style.as_deref() != Some(group.as_str()) {
                    if let Some(g) = run_style.take() {
                        line.push(Item::Text(std::mem::take(&mut run), g));
                    }
                    run_style = Some(group);
                }
                let c = cell.contents();
                run.push_str(if c.is_empty() { " " } else { &c });
            }
            if let Some(g) = run_style {
                // Trailing blanks in the plain style are not worth drawing.
                if g == "Normal" {
                    run.truncate(run.trim_end().len());
                }
                if !run.is_empty() {
                    line.push(Item::Text(run, g));
                }
            }
            out.push(line);
        }
        p.set_scrollback(0);
        (out, scroll, max)
    }
}

/// The highlight group for a cell's look: "Normal", or an `@term…` group
/// made on first use.
fn group_of(cell: &vt100::Cell, normal: Style, theme: &mut Theme) -> String {
    let (fg, bg) = (cell.fgcolor(), cell.bgcolor());
    let flags = [
        (cell.bold(), 'b'),
        (cell.italic(), 'i'),
        (cell.underline(), 'u'),
        (cell.inverse(), 'r'),
    ];
    if fg == vt100::Color::Default && bg == vt100::Color::Default && flags.iter().all(|f| !f.0) {
        return "Normal".into();
    }
    let code = |c: vt100::Color| match c {
        vt100::Color::Default => "-".to_owned(),
        vt100::Color::Idx(i) => i.to_string(),
        vt100::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    };
    let flag: String = flags.iter().filter(|f| f.0).map(|f| f.1).collect();
    let name = format!("@term/{}/{}/{flag}", code(fg), code(bg));
    if !theme.has(&name) {
        let color = |c: vt100::Color| match c {
            vt100::Color::Default => None,
            vt100::Color::Idx(i) => Some(Color::Indexed(i)),
            vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
        };
        let mut style = normal;
        if let Some(c) = color(fg) {
            style = style.fg(c);
        }
        if let Some(c) = color(bg) {
            style = style.bg(c);
        }
        for (on, f) in flags {
            if on {
                style = style.add_modifier(match f {
                    'b' => Modifier::BOLD,
                    'i' => Modifier::ITALIC,
                    'u' => Modifier::UNDERLINED,
                    _ => Modifier::REVERSED,
                });
            }
        }
        theme.set(&name, style);
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &[Item]) -> String {
        line.iter()
            .map(|i| match i {
                Item::Text(t, _) => t.as_str(),
                Item::Fill(..) => "",
            })
            .collect()
    }

    #[test]
    fn plays_output_into_a_screen_with_colors() {
        let mut theme = Theme::default();
        let mut t = Term::default();
        t.load(
            0,
            "build\r\n\u{1b}[32mgreen\u{1b}[0m plain\r\n10%\r100%\r\n",
        );
        t.chunk(0, "build\r\n"); // already there
        let (lines, scroll, max) = t.lines(4, 12, 0, &mut theme);
        assert_eq!((scroll, max), (0, 0));
        assert_eq!(text(&lines[0]), "build");
        assert_eq!(text(&lines[1]), "green plain");
        assert_eq!(text(&lines[2]), "100%");
        assert_eq!(text(&lines[3]), "");
        assert!(matches!(&lines[1][0], Item::Text(t, g) if t == "green" && g == "@term/2/-/"));
        assert!(matches!(&lines[1][1], Item::Text(t, g) if t == " plain" && g == "Normal"));
        assert!(theme.has("@term/2/-/"));

        // More output scrolls; earlier rows can be scrolled back to.
        t.chunk(t.end, "a\r\nb\r\n");
        let (lines, _, max) = t.lines(4, 12, 0, &mut theme);
        assert_eq!(max, 2);
        assert_eq!(text(&lines[2]), "b");
        let (lines, scroll, _) = t.lines(4, 12, 9, &mut theme);
        assert_eq!(scroll, 2);
        assert_eq!(text(&lines[0]), "build");
    }
}
