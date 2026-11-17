//! Mouse selection: press and drag to highlight text on screen, release to
//! copy it to the system clipboard. The selection runs in reading order like
//! a terminal's own, within the pane where it started.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use unicode_width::UnicodeWidthStr;

use bone_ui::model::LineCopy;

#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// Where the button went down and where the pointer is, as (col, row).
    pub anchor: (u16, u16),
    pub head: (u16, u16),
    /// The pane it started in; nothing outside it is selected.
    pub clip: Rect,
    /// The button was released.
    pub done: bool,
    /// The text has been sent to the clipboard.
    pub copied: bool,
}

impl Selection {
    pub fn new(at: (u16, u16), clip: Rect) -> Self {
        Selection {
            anchor: at,
            head: at,
            clip,
            done: false,
            copied: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Start and end, in reading order.
    fn bounds(&self) -> ((u16, u16), (u16, u16)) {
        let key = |p: (u16, u16)| (p.1, p.0);
        if key(self.anchor) <= key(self.head) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn contains(&self, x: u16, y: u16) -> bool {
        let c = self.clip;
        if x < c.left() || x >= c.right() || y < c.top() || y >= c.bottom() {
            return false;
        }
        let ((sx, sy), (ex, ey)) = self.bounds();
        (sy..=ey).contains(&y) && (y != sy || x >= sx) && (y != ey || x <= ex)
    }

    /// Reverse the colors of the selected cells.
    pub fn highlight(&self, buf: &mut Buffer) {
        let area = buf.area.intersection(self.clip);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if self.contains(x, y) {
                    let cell = &mut buf[(x, y)];
                    cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }

    /// The selected text: rows joined with newlines, trailing blanks dropped.
    /// `copy` is the chat rows on screen as (chat area, row, [`LineCopy`]):
    /// in its chat, a row's decoration is left out (copied as its `lead`
    /// when the selection takes the row from its start) and a wrapped row
    /// rejoins the one above.
    pub fn text(&self, buf: &Buffer, copy: &[(Rect, u16, LineCopy)]) -> String {
        let area = buf.area.intersection(self.clip);
        let ((sx, sy), (_, ey)) = self.bounds();
        let top = sy.max(area.top());
        let mut out = String::new();
        for y in top..=ey.min(area.bottom().saturating_sub(1)) {
            // Not under a panel over the chat: it has rows of its own.
            let info = (copy.iter())
                .find(|(a, row, _)| *row == y && self.clip.intersection(*a) == *a)
                .map(|(a, _, c)| (a.x + c.skip as u16, c));
            let start = info.map_or(area.left(), |(start, _)| start);
            let mut row = String::new();
            if let Some((_, c)) = info
                && (y > sy || sx <= start)
            {
                row.push_str(&c.lead);
            }
            let mut skip = 0;
            for x in area.left()..area.right() {
                let sym = buf[(x, y)].symbol();
                // Cells after a wide character only pad it.
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                skip = sym.width().saturating_sub(1);
                if x >= start && self.contains(x, y) {
                    row.push_str(sym);
                }
            }
            if y > top {
                out.push_str(info.and_then(|(_, c)| c.join.as_deref()).unwrap_or("\n"));
            }
            out.push_str(row.trim_end());
        }
        out
    }
}

/// Put `text` on the clipboard, every way that may work here. Inside tmux,
/// `tmux load-buffer -w` (tmux forwards it to the outer terminal; apps'
/// own clipboard sequences are usually dropped). Otherwise the OSC 52
/// sequence, which this returns for the caller to print. With a local
/// display, also `wl-copy` or `xclip`. Commands run in the background.
pub fn copy(text: &str) -> Option<String> {
    let tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
    let data = text.to_owned();
    std::thread::spawn(move || {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let vars = bone_ui::clipboard::display_env();
        let set = |k: &str| vars.iter().any(|(key, v)| *key == k && v.is_some());
        let mut cmds: Vec<&[&str]> = Vec::new();
        if tmux {
            cmds.push(&["tmux", "load-buffer", "-w", "-"]);
        }
        if !set("SSH_CONNECTION") {
            if set("WAYLAND_DISPLAY") {
                cmds.push(&["wl-copy"]);
            } else if set("DISPLAY") {
                cmds.push(&["xclip", "-selection", "clipboard"]);
            }
        }
        for cmd in cmds {
            let mut command = Command::new(cmd[0]);
            for (key, value) in &vars {
                match value {
                    Some(value) => command.env(key, value),
                    None => command.env_remove(key),
                };
            }
            let child = command
                .args(&cmd[1..])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(mut child) = child {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(data.as_bytes());
                }
                let _ = child.wait();
            }
        }
    });
    (!tmux).then(|| osc52(text))
}

/// The escape sequence that puts `text` on the clipboard.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_order_text_and_base64() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        buf.set_string(0, 0, "abcdef", ratatui::style::Style::default());
        buf.set_string(0, 1, "日本 x", ratatui::style::Style::default());
        buf.set_string(0, 2, "uvw", ratatui::style::Style::default());
        // Dragged upward: still from (2,0) to (1,2).
        let mut s = Selection::new((1, 2), buf.area);
        s.head = (2, 0);
        assert_eq!(s.text(&buf, &[]), "cdef\n日本 x\nuv");
        assert!(s.contains(5, 0) && !s.contains(1, 0) && !s.contains(2, 2));
        assert_eq!(base64(b"hi!"), "aGkh");
        assert_eq!(base64(b"hi"), "aGk=");
        assert_eq!(base64(b"h"), "aA==");
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn decoration_is_left_out_and_wrapped_rows_rejoin() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 14, 5));
        // A list item wrapped onto two rows, then a code line in a gutter.
        for (y, text) in (0..).zip(["  • one two", "    three", "  ▎ ls -l", "plain"]) {
            buf.set_string(0, y, text, ratatui::style::Style::default());
        }
        let row = |y, lead: &str, join: Option<&str>| {
            let copy = LineCopy {
                skip: 4,
                lead: lead.into(),
                join: join.map(Into::into),
            };
            (buf.area, y, copy)
        };
        let rows = [row(0, "- ", None), row(1, "", Some(" ")), row(2, "", None)];
        let mut s = Selection::new((0, 0), buf.area);
        s.head = (13, 3);
        assert_eq!(s.text(&buf, &rows), "- one two three\nls -l\nplain");
        // Starting after the bullet leaves the lead out too.
        s.anchor = (8, 0);
        assert_eq!(s.text(&buf, &rows), "two three\nls -l\nplain");
        // A selection in a pane over part of the chat reads the screen.
        s.clip = Rect::new(0, 0, 12, 5);
        s.anchor = (0, 0);
        s.head = (11, 0);
        assert_eq!(s.text(&buf, &rows), "  • one two");
    }
}
