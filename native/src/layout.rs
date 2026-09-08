//! Frontend-only restart layout: which conversations are open and in what
//! order, the selected tab, the daemon address field, and each tab's unsent
//! composer draft. Conversation content itself stays daemon-owned; this file is
//! purely a client preference (see NATIVE_APP_PLAN.md).
//!
//! The file uses a small hand-rolled, length-prefixed text format so the
//! desktop crate needs no serde dependency. String payloads are byte-length
//! prefixed, so drafts may contain any bytes including newlines.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

pub const FORMAT_HEADER: &[u8] = b"bone-desktop-layout v1\n";
const STATE_FILE_NAME: &str = "layout.txt";

/// One open tab's durable frontend state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabState {
    /// `Some(id)` once the tab is pinned to a daemon conversation; `None` for
    /// a not-yet-attached "new" tab.
    pub conversation_id: Option<i64>,
    /// Unsent composer draft.
    pub draft: String,
}

/// Snapshot of the whole open-tab layout.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    /// Daemon address shown in the toolbar.
    pub address: String,
    /// Index of the selected tab (clamped on restore).
    pub selected: usize,
    /// Open tabs in order.
    pub tabs: Vec<TabState>,
}

/// Default on-disk location: `BONE_DESKTOP_STATE` when set, otherwise
/// `$XDG_STATE_HOME|$HOME/.local/state`/bone-desktop/layout.txt. `None` when
/// no home directory is known (persistence then stays disabled).
pub fn state_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("BONE_DESKTOP_STATE") {
        return Some(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(base.join("bone-desktop").join(STATE_FILE_NAME))
}

fn encode(layout: &Layout) -> String {
    let mut out = String::with_capacity(128 + layout.tabs.len() * 64);
    out.push_str(std::str::from_utf8(FORMAT_HEADER).expect("static header is utf-8"));
    push_field(&mut out, b"address", layout.address.as_bytes());
    out.push_str("selected ");
    out.push_str(&layout.selected.to_string());
    out.push('\n');
    for tab in &layout.tabs {
        match tab.conversation_id {
            Some(id) => {
                out.push_str("tab load ");
                out.push_str(&id.to_string());
                out.push('\n');
            }
            None => out.push_str("tab new\n"),
        }
        push_field(&mut out, b"draft", tab.draft.as_bytes());
    }
    out
}

/// `keyword <byte-len> <raw bytes>\n`
fn push_field(out: &mut String, keyword: &[u8], bytes: &[u8]) {
    out.push_str(std::str::from_utf8(keyword).expect("keyword is utf-8"));
    out.push(' ');
    out.push_str(&bytes.len().to_string());
    out.push(' ');
    out.push_str(std::str::from_utf8(bytes).expect("payload must be utf-8"));
    out.push('\n');
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatError {
    Header,
    Unexpected(&'static str),
    Utf8,
    Number,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::Header => write!(f, "not a bone-desktop layout file"),
            FormatError::Unexpected(what) => write!(f, "unexpected input: {what}"),
            FormatError::Utf8 => write!(f, "payload is not valid UTF-8"),
            FormatError::Number => write!(f, "invalid number"),
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn token(&mut self) -> Result<&'a [u8], FormatError> {
        let start = self.pos;
        while self.pos < self.bytes.len()
            && self.bytes[self.pos] != b' '
            && self.bytes[self.pos] != b'\n'
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(FormatError::Unexpected("empty token"));
        }
        Ok(&self.bytes[start..self.pos])
    }

    fn byte(&mut self, expected: u8) -> Result<(), FormatError> {
        if self.bytes.get(self.pos) == Some(&expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(FormatError::Unexpected("missing field separator"))
        }
    }

    fn newline(&mut self) -> Result<(), FormatError> {
        self.byte(b'\n')
    }

    /// `123 …` → the decimal value, stopping at the next space or newline
    /// (not consumed).
    fn number(&mut self) -> Result<i128, FormatError> {
        let text = std::str::from_utf8(self.token()?).map_err(|_| FormatError::Number)?;
        text.parse::<i128>().map_err(|_| FormatError::Number)
    }

    /// A byte-length-prefixed payload: `<len> <bytes>\n`. Consumes through the
    /// terminator newline.
    fn payload(&mut self) -> Result<Vec<u8>, FormatError> {
        self.byte(b' ')?;
        let len = self.number()?;
        let len = usize::try_from(len).map_err(|_| FormatError::Number)?;
        self.byte(b' ')?;
        let end = self
            .pos
            .checked_add(len)
            .ok_or(FormatError::Unexpected("payload length overflow"))?;
        if end > self.bytes.len() {
            return Err(FormatError::Unexpected("payload exceeds file length"));
        }
        let value = self.bytes[self.pos..end].to_vec();
        self.pos = end;
        self.newline()?;
        Ok(value)
    }
}

fn decode(input: &[u8]) -> Result<Layout, FormatError> {
    let mut cursor = Cursor {
        bytes: input,
        pos: 0,
    };
    if !input.starts_with(FORMAT_HEADER) {
        return Err(FormatError::Header);
    }
    cursor.pos = FORMAT_HEADER.len();
    let mut layout = Layout::default();
    let mut current_tab = None;
    loop {
        if cursor.pos >= cursor.bytes.len() {
            break;
        }
        let keyword = cursor.token()?;
        match keyword {
            b"address" => {
                let payload = cursor.payload()?;
                layout.address = String::from_utf8(payload).map_err(|_| FormatError::Utf8)?;
            }
            b"selected" => {
                cursor.byte(b' ')?;
                let value = cursor.number()?;
                cursor.newline()?;
                layout.selected = usize::try_from(value).map_err(|_| FormatError::Number)?;
            }
            b"tab" => {
                cursor.byte(b' ')?;
                let kind = cursor.token()?;
                let conversation_id = match kind {
                    b"load" => {
                        cursor.byte(b' ')?;
                        let id = cursor.number()?;
                        cursor.newline()?;
                        Some(i64::try_from(id).map_err(|_| FormatError::Number)?)
                    }
                    b"new" => {
                        cursor.newline()?;
                        None
                    }
                    _ => return Err(FormatError::Unexpected("unknown tab kind")),
                };
                layout.tabs.push(TabState {
                    conversation_id,
                    draft: String::new(),
                });
                current_tab = Some(layout.tabs.len() - 1);
            }
            b"draft" => {
                let payload = cursor.payload()?;
                let index =
                    current_tab.ok_or(FormatError::Unexpected("draft before any tab record"))?;
                layout.tabs[index].draft =
                    String::from_utf8(payload).map_err(|_| FormatError::Utf8)?;
            }
            _ => return Err(FormatError::Unexpected("unknown record")),
        }
    }
    Ok(layout)
}

/// Write `layout` atomically (temp file + rename) under `path`.
pub fn save(path: &Path, layout: &Layout) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, encode(layout))?;
    std::fs::rename(&tmp, path)
}

/// Read a layout file. `Ok(None)` when the file does not exist; `Ok(Some(_))`
/// on success; `Err(message)` when the file exists but is unreadable or not a
/// valid layout (callers may surface this and fall back to a default layout).
pub fn load(path: &Path) -> Result<Option<Layout>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {path:?}: {error}")),
    };
    match decode(&bytes) {
        Ok(layout) => Ok(Some(layout)),
        Err(error) => Err(format!("invalid layout {path:?}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Layout {
        Layout {
            address: "127.0.0.1:17878".into(),
            selected: 2,
            tabs: vec![
                TabState {
                    conversation_id: Some(7),
                    draft: "hello".into(),
                },
                TabState {
                    conversation_id: None,
                    draft: "draft line one\n\nline three ✓ and trailing newline\n".into(),
                },
                TabState {
                    conversation_id: Some(9001),
                    draft: String::new(),
                },
            ],
        }
    }

    #[test]
    fn round_trips_drafts_order_and_selection() {
        let layout = sample();
        assert_eq!(decode(encode(&layout).as_bytes()).unwrap(), layout);
    }

    #[test]
    fn round_trips_empty_layout() {
        let layout = Layout::default();
        assert_eq!(decode(encode(&layout).as_bytes()).unwrap(), layout);
    }

    #[test]
    fn rejects_wrong_header_and_garbage() {
        assert!(matches!(
            decode(b"bone-desktop-layout v2\n"),
            Err(FormatError::Header)
        ));
        assert!(matches!(decode(b"not a layout"), Err(FormatError::Header)));
        // Truncated payload.
        assert!(decode(b"bone-desktop-layout v1\naddress 10 127.0.0.1\n").is_err());
        // Trailing junk after a complete record.
        let mut good = encode(&sample()).into_bytes();
        good.extend_from_slice(b"junk\n");
        assert!(decode(&good).is_err());
    }

    #[test]
    fn file_round_trip_and_missing_file() {
        let dir =
            std::env::temp_dir().join(format!("bone-desktop-layout-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.txt");
        save(&path, &sample()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(sample()));
        // Atomic write leaves no temp sibling behind.
        assert!(!path.with_extension("tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(load(&path).unwrap(), None);
    }

    #[test]
    fn corrupted_file_reports_error() {
        let dir = std::env::temp_dir().join(format!(
            "bone-desktop-layout-test-{}-corrupt",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.txt");
        std::fs::write(&path, b"bone-desktop-layout v1\nselected not-a-number\n").unwrap();
        assert!(load(&path).unwrap_err().contains("invalid layout"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
