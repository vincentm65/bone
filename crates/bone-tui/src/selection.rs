//! Mouse selection: press and drag to highlight text on screen, release to
//! copy it to the system clipboard. The selection runs in reading order like
//! a terminal's own.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// Where the button went down and where the pointer is, as (col, row).
    pub anchor: (u16, u16),
    pub head: (u16, u16),
    /// The button was released.
    pub done: bool,
    /// The text has been sent to the clipboard.
    pub copied: bool,
}

impl Selection {
    pub fn new(at: (u16, u16)) -> Self {
        Selection {
            anchor: at,
            head: at,
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
        let ((sx, sy), (ex, ey)) = self.bounds();
        (sy..=ey).contains(&y) && (y != sy || x >= sx) && (y != ey || x <= ex)
    }

    /// Reverse the colors of the selected cells.
    pub fn highlight(&self, buf: &mut Buffer) {
        let area = buf.area;
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
    pub fn text(&self, buf: &Buffer) -> String {
        let area = buf.area;
        let ((_, sy), (_, ey)) = self.bounds();
        let mut rows = Vec::new();
        for y in sy.max(area.top())..=ey.min(area.bottom().saturating_sub(1)) {
            let mut row = String::new();
            let mut skip = 0;
            for x in area.left()..area.right() {
                let sym = buf[(x, y)].symbol();
                // Cells after a wide character only pad it.
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                skip = sym.width().saturating_sub(1);
                if self.contains(x, y) {
                    row.push_str(sym);
                }
            }
            rows.push(row.trim_end().to_owned());
        }
        rows.join("\n")
    }
}

/// Put `text` on the clipboard, every way that may work here. Inside tmux,
/// `tmux load-buffer -w` (tmux forwards it to the outer terminal; apps'
/// own clipboard sequences are usually dropped). Otherwise the OSC 52
/// sequence, which this returns for the caller to print. With a local
/// display, also `wl-copy` or `xclip`. Commands run in the background.
pub fn copy(text: &str) -> Option<String> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let mut cmds: Vec<&[&str]> = Vec::new();
    let tmux = env("TMUX").is_some();
    if tmux {
        cmds.push(&["tmux", "load-buffer", "-w", "-"]);
    }
    if env("SSH_CONNECTION").is_none() {
        if env("WAYLAND_DISPLAY").is_some() {
            cmds.push(&["wl-copy"]);
        } else if env("DISPLAY").is_some() {
            cmds.push(&["xclip", "-selection", "clipboard"]);
        }
    }
    let owned: Vec<Vec<String>> = cmds
        .iter()
        .map(|c| c.iter().map(|s| s.to_string()).collect())
        .collect();
    let data = text.to_owned();
    std::thread::spawn(move || {
        use std::io::Write;
        use std::process::{Command, Stdio};
        for cmd in owned {
            let child = Command::new(&cmd[0])
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
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn reading_order_text_and_base64() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        buf.set_string(0, 0, "abcdef", ratatui::style::Style::default());
        buf.set_string(0, 1, "日本 x", ratatui::style::Style::default());
        buf.set_string(0, 2, "uvw", ratatui::style::Style::default());
        // Dragged upward: still from (2,0) to (1,2).
        let mut s = Selection::new((1, 2));
        s.head = (2, 0);
        assert_eq!(s.text(&buf), "cdef\n日本 x\nuv");
        assert!(s.contains(5, 0) && !s.contains(1, 0) && !s.contains(2, 2));
        assert_eq!(base64(b"hi!"), "aGkh");
        assert_eq!(base64(b"hi"), "aGk=");
        assert_eq!(base64(b"h"), "aA==");
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }
}
