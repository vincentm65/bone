//! Frontend-only restart layout: which conversations are open and in what
//! order, the selected tab, the daemon address field, and each tab's unsent
//! composer draft. Conversation content itself stays daemon-owned; this file is
//! purely a client preference (see NATIVE_APP_PLAN.md).
//!
//! Length-prefixed UTF-8 records keep multiline drafts intact. The workspace
//! record is a JSON split tree of windows, panes and tab groups; layout files
//! written before it (v1–v3) carried a single split-view flag that is migrated
//! into the tree once on restore.

use std::fmt;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const FORMAT_HEADER: &[u8] = b"bone-desktop-layout v5\n";
const V4_FORMAT_HEADER: &[u8] = b"bone-desktop-layout v4\n";
const V3_FORMAT_HEADER: &[u8] = b"bone-desktop-layout v3\n";
const V2_FORMAT_HEADER: &[u8] = b"bone-desktop-layout v2\n";
const V1_FORMAT_HEADER: &[u8] = b"bone-desktop-layout v1\n";
const STATE_FILE_NAME: &str = "layout.txt";

/// Valid ranges for persisted display values; out-of-range values found in a
/// saved file are clamped on load.
pub const ZOOM_MIN: u16 = 75;
pub const ZOOM_MAX: u16 = 200;
pub const SIDEBAR_WIDTH_MIN: u16 = 220;
pub const SIDEBAR_WIDTH_MAX: u16 = 500;
/// Fresh layouts use this fraction of the available window width. A manually
/// resized sidebar is stored as an absolute width instead.
pub const SIDEBAR_DEFAULT_FRACTION: f32 = 0.20;
/// Leave a readable transcript and room for composer actions before showing navigation.
pub const CENTRAL_WIDTH_MIN: u16 = 560;

/// How much tool detail the desktop shows without opening an individual call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolVerbosity {
    #[default]
    Concise,
    Verbose,
}

/// Persisted display settings: zoom level and sidebar width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preferences {
    /// Document zoom as a percentage (75..=200 after clamping).
    pub zoom_percent: u16,
    pub tool_verbosity: ToolVerbosity,
    /// Left sidebar width in pixels (220..=500 after clamping).
    pub sidebar_width: u16,
    /// Whether `sidebar_width` is a user override. Fresh layouts use the
    /// responsive 20% default and do not let an old absolute width win.
    pub sidebar_width_manual: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            zoom_percent: 100,
            tool_verbosity: ToolVerbosity::Concise,
            sidebar_width: 230,
            sidebar_width_manual: false,
        }
    }
}

/// Responsive default width for a sidebar that has not been manually resized.
pub fn default_sidebar_width(window_width: f32) -> f32 {
    (window_width * SIDEBAR_DEFAULT_FRACTION)
        .clamp(SIDEBAR_WIDTH_MIN as f32, SIDEBAR_WIDTH_MAX as f32)
}

/// Per-frame responsive decision for the horizontal panes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResponsivePlan {
    /// Whether the left sidebar renders at all this frame.
    pub show_sidebar: bool,
    /// Maximum sidebar width this frame (`>= SIDEBAR_WIDTH_MIN` when shown).
    pub sidebar_cap: f32,
}

/// Decide whether the sidebar renders and how wide it may be for a window of
/// `window_width`. The conversation area is guaranteed at least
/// `CENTRAL_WIDTH_MIN` pixels:
///
/// - The sidebar hides when the window cannot host it at its minimum width
///   alongside the guaranteed conversation area.
/// - The sidebar is capped so `window_width - sidebar_cap` always leaves room
///   for the conversation area.
pub fn responsive_plan(window_width: f32) -> ResponsivePlan {
    if !window_width.is_finite() || window_width <= 0.0 {
        return ResponsivePlan {
            show_sidebar: false,
            sidebar_cap: 0.0,
        };
    }
    let sidebar_min = SIDEBAR_WIDTH_MIN as f32;
    let central_min = CENTRAL_WIDTH_MIN as f32;
    let show_sidebar = window_width >= sidebar_min + central_min;
    let sidebar_cap = if show_sidebar {
        (window_width - central_min).max(sidebar_min)
    } else {
        0.0
    };
    ResponsivePlan {
        show_sidebar,
        sidebar_cap,
    }
}

/// One open tab's durable frontend state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabState {
    /// `Some(id)` once the tab is pinned to a daemon conversation; `None` for
    /// a not-yet-attached "new" tab.
    pub conversation_id: Option<i64>,
    /// Unsent composer draft.
    pub draft: String,
}

/// The single split-view flag carried by pre-workspace (v1–v3) layout files.
///
/// `tab` is a one-based index into `tabs` (0 when the flag names no tab), the
/// same convention `Workspace` uses. Read from a saved file only; migration
/// input for [`crate::workspace::Workspace::from_legacy`], never re-encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LegacySplit {
    /// Whether the old file requested two panes.
    pub active: bool,
    /// The tab shown in the second pane.
    pub tab: usize,
}

/// Snapshot of the whole open-tab layout.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// Daemon address shown in the toolbar.
    pub address: String,
    /// Index of the selected tab (clamped on restore).
    pub selected: usize,
    /// Open tabs in order.
    pub tabs: Vec<TabState>,
    /// Persisted display settings.
    pub preferences: Preferences,
    /// Tab references are one-based indices into `tabs`, not runtime IDs.
    pub workspace: Option<crate::workspace::Workspace>,
    /// Persisted client-local placement, visibility, and ordering of daemon panels.
    pub panel_layout: crate::workspace::PanelLayout,
    /// Split flag from a pre-workspace file; migration input only. Never
    /// written back, so it is excluded from equality (see `PartialEq`).
    pub legacy_split: LegacySplit,
}

impl PartialEq for Layout {
    /// Compares the fields `encode` writes. `legacy_split` is a one-way
    /// migration input, so two layouts with the same durable state compare
    /// equal even when one was decoded from an old file.
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
            && self.selected == other.selected
            && self.tabs == other.tabs
            && self.preferences == other.preferences
            && self.workspace == other.workspace
            && self.panel_layout == other.panel_layout
    }
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
    let prefs = &layout.preferences;
    out.push_str("display ");
    out.push_str(&prefs.zoom_percent.to_string());
    out.push(' ');
    out.push_str(&prefs.sidebar_width.to_string());
    out.push(' ');
    out.push_str(if prefs.sidebar_width_manual { "1" } else { "0" });
    out.push_str(if prefs.tool_verbosity == ToolVerbosity::Verbose {
        " 1"
    } else {
        " 0"
    });
    out.push('\n');
    if let Some(workspace) = &layout.workspace {
        let json = serde_json::to_string(workspace).expect("workspace is serializable");
        push_field(&mut out, b"workspace", json.as_bytes());
    }
    let panels = serde_json::to_string(&layout.panel_layout).expect("panel layout is serializable");
    push_field(&mut out, b"panels", panels.as_bytes());
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
enum Format {
    /// Oldest format: `display` carries the split flag but no manual-sidebar
    /// marker, so its sidebar width always migrates to the responsive default.
    V1,
    /// Like v1 but with a manual-sidebar marker.
    V2,
    /// Like v2 (current file is v4; v3 only differs by header).
    V3,
    /// Current format: `display` is just zoom, sidebar width and manual flag.
    V4,
    /// Adds the tool verbosity preference to the display record.
    V5,
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

/// Clamp an arbitrary parsed number into a valid `u16` range (negative or
/// oversized values snap to the nearest bound).
fn clamp_u16(value: i128, min: u16, max: u16) -> u16 {
    value.clamp(min as i128, max as i128) as u16
}

fn decode(input: &[u8]) -> Result<Layout, FormatError> {
    let mut cursor = Cursor {
        bytes: input,
        pos: 0,
    };
    // The workspace tree (v4) supersedes the single split flag carried by
    // v1–v3 files; those are decoded into `legacy_split` for a one-time
    // migration on restore.
    let (format, header_len) = if input.starts_with(FORMAT_HEADER) {
        (Format::V5, FORMAT_HEADER.len())
    } else if input.starts_with(V4_FORMAT_HEADER) {
        (Format::V4, V4_FORMAT_HEADER.len())
    } else if input.starts_with(V3_FORMAT_HEADER) {
        (Format::V3, V3_FORMAT_HEADER.len())
    } else if input.starts_with(V2_FORMAT_HEADER) {
        (Format::V2, V2_FORMAT_HEADER.len())
    } else if input.starts_with(V1_FORMAT_HEADER) {
        (Format::V1, V1_FORMAT_HEADER.len())
    } else {
        return Err(FormatError::Header);
    };
    cursor.pos = header_len;
    let mut layout = Layout::default();
    let mut current_tab = None;
    let mut seen_panel_layout = false;
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
            b"workspace" => {
                let payload = cursor.payload()?;
                if layout.workspace.is_some() {
                    return Err(FormatError::Unexpected("duplicate workspace"));
                }
                layout.workspace = Some(
                    serde_json::from_slice(&payload)
                        .map_err(|_| FormatError::Unexpected("invalid workspace tree"))?,
                );
            }
            b"panels" => {
                let payload = cursor.payload()?;
                if seen_panel_layout {
                    return Err(FormatError::Unexpected("duplicate panel layout"));
                }
                seen_panel_layout = true;
                layout.panel_layout = serde_json::from_slice(&payload)
                    .map_err(|_| FormatError::Unexpected("invalid panel layout"))?;
            }
            b"display" => {
                cursor.byte(b' ')?;
                let zoom_percent = clamp_u16(cursor.number()?, ZOOM_MIN, ZOOM_MAX);
                cursor.byte(b' ')?;
                if matches!(format, Format::V4 | Format::V5) {
                    let sidebar_width =
                        clamp_u16(cursor.number()?, SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX);
                    cursor.byte(b' ')?;
                    let sidebar_width_manual = match cursor.number()? {
                        0 => false,
                        1 => true,
                        _ => return Err(FormatError::Number),
                    };
                    let tool_verbosity = if format == Format::V5 {
                        cursor.byte(b' ')?;
                        match cursor.number()? {
                            0 => ToolVerbosity::Concise,
                            1 => ToolVerbosity::Verbose,
                            _ => return Err(FormatError::Number),
                        }
                    } else {
                        ToolVerbosity::Concise
                    };
                    cursor.newline()?;
                    layout.preferences = Preferences {
                        tool_verbosity,
                        zoom_percent,
                        sidebar_width,
                        sidebar_width_manual,
                    };
                } else {
                    let split = match cursor.number()? {
                        0 => false,
                        1 => true,
                        _ => return Err(FormatError::Number),
                    };
                    cursor.byte(b' ')?;
                    let split_tab = cursor.number()?;
                    let split_tab = usize::try_from(if split_tab < 0 {
                        0
                    } else {
                        split_tab.min(usize::MAX as i128)
                    })
                    .map_err(|_| FormatError::Number)?;
                    cursor.byte(b' ')?;
                    let sidebar_width =
                        clamp_u16(cursor.number()?, SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX);
                    cursor.byte(b' ')?;
                    let first = cursor.number()?;
                    let sidebar_width_manual = if cursor.bytes.get(cursor.pos) == Some(&b'\n') {
                        // Five-field record: `first` is the old split width.
                        false
                    } else {
                        // The manual marker only exists from v2 onward; v1 files
                        // (and some written by the buggy egui restore path)
                        // always migrate back to the responsive default.
                        let manual = match first {
                            0 => false,
                            1 => format != Format::V1,
                            _ => return Err(FormatError::Number),
                        };
                        cursor.byte(b' ')?;
                        // The trailing split width is discarded; the workspace
                        // tree supersedes it.
                        cursor.number()?;
                        manual
                    };
                    cursor.newline()?;
                    layout.preferences = Preferences {
                        tool_verbosity: ToolVerbosity::Concise,
                        zoom_percent,
                        sidebar_width,
                        sidebar_width_manual,
                    };
                    layout.legacy_split = LegacySplit {
                        active: split,
                        tab: split_tab,
                    };
                }
            }
            _ => return Err(FormatError::Unexpected("unknown record")),
        }
    }
    Ok(layout)
}

/// Write `layout` atomically (temp file + rename) under `path`.
pub fn save(path: &Path, layout: &Layout) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    // Include both the process id and a monotonic suffix so concurrent saves
    // from one process cannot overwrite each other's temporary file. The
    // destination replacement is the commit point for each writer.
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(encode(layout).as_bytes())?;
        file.sync_all()?;
        drop(file);
        replace_file(&tmp, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(not(windows))]
fn replace_file(tmp: &Path, path: &Path) -> io::Result<()> {
    std::fs::rename(tmp, path)
}

#[cfg(windows)]
fn replace_file(tmp: &Path, path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    // `std::fs::rename` does not replace an existing destination on Windows.
    // MoveFileEx does, while keeping the replacement a single filesystem
    // operation and requesting write-through durability.
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let from: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        // `Path::parent` returns an empty path for a relative filename. There
        // is no directory handle to sync in that case; the file itself was
        // already flushed before the rename.
        if !parent.as_os_str().is_empty() {
            std::fs::File::open(parent)?.sync_all()?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
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
            preferences: Preferences {
                tool_verbosity: ToolVerbosity::Concise,
                zoom_percent: 150,
                sidebar_width: 300,
                sidebar_width_manual: true,
            },
            workspace: None,
            panel_layout: crate::workspace::PanelLayout::default(),
            legacy_split: LegacySplit::default(),
        }
    }

    #[test]
    fn tool_verbosity_round_trips_and_older_layouts_default_to_concise() {
        let mut layout = Layout::default();
        layout.preferences.tool_verbosity = ToolVerbosity::Verbose;
        layout.tabs.push(TabState {
            conversation_id: Some(42),
            draft: "keep my draft".into(),
        });
        let restored = decode(encode(&layout).as_bytes()).unwrap();
        assert_eq!(restored.preferences.tool_verbosity, ToolVerbosity::Verbose);
        assert_eq!(restored.tabs[0].draft, "keep my draft");
        let old = b"bone-desktop-layout v4\naddress 0 \nselected 0\ndisplay 125 300 1\n";
        let restored = decode(old).unwrap();
        assert_eq!(restored.preferences.tool_verbosity, ToolVerbosity::Concise);
        assert_eq!(restored.preferences.zoom_percent, 125);
        assert_eq!(restored.preferences.sidebar_width, 300);
        assert!(restored.preferences.sidebar_width_manual);
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
    fn panel_layout_record_round_trips_nonempty_state() {
        let mut layout = Layout::default();
        layout
            .panel_layout
            .panels
            .push(crate::workspace::PanelEntry {
                id: "activity".into(),
                slot: bone_protocol::PanelSlot::Right,
                order: 3,
                hidden: true,
                size: Some(280.5),
            });
        let restored = decode(encode(&layout).as_bytes()).unwrap();
        assert_eq!(restored.panel_layout, layout.panel_layout);
    }

    #[test]
    fn legacy_layouts_decode_without_panel_record() {
        let records = [
            ("v1", "display 100 0 0 230 380\n"),
            ("v2", "display 100 0 0 230 0 380\n"),
            ("v3", "display 100 0 0 230 0 380\n"),
            ("v4", "display 100 230 0\n"),
        ];
        for (version, display) in records {
            let input = format!("bone-desktop-layout {version}\n{display}");
            let layout = decode(input.as_bytes()).unwrap();
            assert!(layout.panel_layout.panels.is_empty(), "{version}");
        }
    }

    #[test]
    fn duplicate_panel_layout_records_are_rejected_even_when_empty() {
        let empty = serde_json::to_string(&crate::workspace::PanelLayout::default()).unwrap();
        let input = format!("bone-desktop-layout v5\npanels {} {}\n", empty.len(), empty)
            + &format!("panels {} {}\n", empty.len(), empty);
        assert!(matches!(
            decode(input.as_bytes()),
            Err(FormatError::Unexpected("duplicate panel layout"))
        ));
    }

    #[test]
    fn rejects_wrong_header_and_garbage() {
        // v4 is current; a future version is (correctly) rejected.
        assert!(matches!(
            decode(b"bone-desktop-layout v99\n"),
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

    #[test]
    fn legacy_file_without_display_record_keeps_address_and_drafts_with_default_prefs() {
        let bytes =
            b"bone-desktop-layout v1\naddress 11 127.0.0.1:1\nselected 0\ntab load 5\ndraft 2 hi\n";
        let layout = decode(bytes).unwrap();
        assert_eq!(layout.address, "127.0.0.1:1");
        assert_eq!(layout.selected, 0);
        assert_eq!(layout.tabs.len(), 1);
        assert_eq!(layout.tabs[0].conversation_id, Some(5));
        assert_eq!(layout.tabs[0].draft, "hi");
        assert_eq!(layout.preferences, Preferences::default());
        assert_eq!(Preferences::default().zoom_percent, 100);
        assert_eq!(Preferences::default().sidebar_width, 230);
        assert!(!Preferences::default().sidebar_width_manual);
        assert_eq!(layout.legacy_split, LegacySplit::default());
    }

    #[test]
    fn legacy_manual_sidebar_marker_is_migrated_to_responsive_default() {
        let bytes = b"bone-desktop-layout v1\ndisplay 100 0 0 480 1 380\n";
        let layout = decode(bytes).unwrap();
        assert_eq!(layout.preferences.sidebar_width, 480);
        assert!(!layout.preferences.sidebar_width_manual);
        assert_eq!(default_sidebar_width(840.0), SIDEBAR_WIDTH_MIN as f32);

        let current = b"bone-desktop-layout v2\ndisplay 100 0 0 480 1 380\n";
        assert!(decode(current).unwrap().preferences.sidebar_width_manual);
    }

    #[test]
    fn legacy_split_record_feeds_legacy_split_for_migration() {
        // Mirrors the real on-disk v2 file: 6-field display with split on and
        // tab index 7 in the right pane, sidebar dragged to 349.
        let bytes = b"bone-desktop-layout v2\ndisplay 100 1 7 349 1 676\n";
        let layout = decode(bytes).unwrap();
        assert_eq!(
            layout.preferences,
            Preferences {
                tool_verbosity: ToolVerbosity::Concise,
                zoom_percent: 100,
                sidebar_width: 349,
                sidebar_width_manual: true,
            }
        );
        assert_eq!(
            layout.legacy_split,
            LegacySplit {
                active: true,
                tab: 7,
            }
        );
    }

    #[test]
    fn current_file_never_records_a_legacy_split() {
        // v4 is encode's only output, so a round-tripped layout re-decodes with
        // no migration input.
        let layout = Layout {
            legacy_split: LegacySplit {
                active: true,
                tab: 3,
            },
            ..Default::default()
        };
        let decoded = decode(encode(&layout).as_bytes()).unwrap();
        assert_eq!(decoded.legacy_split, LegacySplit::default());
        // `legacy_split` is excluded from equality, so the durable state still
        // compares equal.
        assert_eq!(decoded, layout);
    }

    #[test]
    fn display_preferences_round_trip_current_values() {
        let layout = Layout {
            preferences: Preferences {
                tool_verbosity: ToolVerbosity::Concise,
                zoom_percent: 75,
                sidebar_width: 500,
                sidebar_width_manual: true,
            },
            ..Default::default()
        };
        let decoded = decode(encode(&layout).as_bytes()).unwrap();
        assert_eq!(decoded.preferences, layout.preferences);
        assert_eq!(decoded, layout);
    }

    #[test]
    fn out_of_range_display_values_are_clamped_on_load() {
        // Current (v4) record: zoom and sidebar width clamp, flag preserved.
        let bytes = b"bone-desktop-layout v4\ndisplay -10 5 0\n";
        let layout = decode(bytes).unwrap();
        assert_eq!(
            layout.preferences,
            Preferences {
                tool_verbosity: ToolVerbosity::Concise,
                zoom_percent: ZOOM_MIN,
                sidebar_width: SIDEBAR_WIDTH_MIN,
                sidebar_width_manual: false,
            }
        );
        let bytes = b"bone-desktop-layout v4\ndisplay 9999 99999 1\n";
        let layout = decode(bytes).unwrap();
        assert_eq!(layout.preferences.zoom_percent, ZOOM_MAX);
        assert_eq!(layout.preferences.sidebar_width, SIDEBAR_WIDTH_MAX);
        assert!(layout.preferences.sidebar_width_manual);

        // Legacy (v1–v3) record: same clamping, and the split flag feeds
        // `legacy_split` for migration.
        let bytes = b"bone-desktop-layout v1\ndisplay -10 1 -3 5 99999\n";
        let layout = decode(bytes).unwrap();
        assert_eq!(layout.preferences.zoom_percent, ZOOM_MIN);
        assert_eq!(layout.preferences.sidebar_width, SIDEBAR_WIDTH_MIN);
        assert!(!layout.preferences.sidebar_width_manual);
        assert_eq!(
            layout.legacy_split,
            LegacySplit {
                active: true,
                tab: 0,
            }
        );
    }

    #[test]
    fn malformed_display_record_is_an_error() {
        // Legacy split flag must be 0 or 1.
        assert!(matches!(
            decode(b"bone-desktop-layout v1\ndisplay 100 2 0 230 380\n"),
            Err(FormatError::Number)
        ));
        // Non-numeric value.
        assert!(matches!(
            decode(b"bone-desktop-layout v1\ndisplay 100 0 0 230 wide\n"),
            Err(FormatError::Number)
        ));
        // Missing fields.
        assert!(decode(b"bone-desktop-layout v1\ndisplay 100 0\n").is_err());
        // Current flag must be 0 or 1.
        assert!(matches!(
            decode(b"bone-desktop-layout v4\ndisplay 100 230 2\n"),
            Err(FormatError::Number)
        ));
        assert!(decode(b"bone-desktop-layout v4\ndisplay 100 230\n").is_err());
    }

    #[test]
    fn unicode_drafts_survive_with_display_record() {
        let mut layout = Layout::default();
        layout.tabs.push(TabState {
            conversation_id: Some(1),
            draft: "héllo ✓\n第二行 🚀\ttabbed\n".into(),
        });
        layout.preferences = Preferences {
            tool_verbosity: ToolVerbosity::Concise,
            zoom_percent: 125,
            sidebar_width: 250,
            sidebar_width_manual: true,
        };
        let encoded = encode(&layout);
        let decoded = decode(encoded.as_bytes()).unwrap();
        assert_eq!(decoded.tabs[0].draft, layout.tabs[0].draft);
        assert_eq!(decoded, layout);
    }

    #[test]
    fn proportional_sidebar_default_is_bounded_and_legacy_width_is_not_manual() {
        assert_eq!(default_sidebar_width(840.0), SIDEBAR_WIDTH_MIN as f32);
        assert_eq!(default_sidebar_width(400.0), SIDEBAR_WIDTH_MIN as f32);
        assert_eq!(default_sidebar_width(4000.0), SIDEBAR_WIDTH_MAX as f32);
        let legacy = decode(b"bone-desktop-layout v1\ndisplay 100 0 0 500 380\n").unwrap();
        assert_eq!(legacy.preferences.sidebar_width, 500);
        assert!(!legacy.preferences.sidebar_width_manual);
    }

    #[test]
    fn manual_sidebar_width_round_trips_without_clobbering_other_preferences() {
        let mut layout = Layout::default();
        layout.preferences.sidebar_width = 410;
        layout.preferences.sidebar_width_manual = true;
        let restored = decode(encode(&layout).as_bytes()).unwrap();
        assert_eq!(restored.preferences, layout.preferences);
    }
    #[test]
    fn responsive_plan_caps_sidebar_but_keeps_central_minimum() {
        // Wide window: the sidebar shows and is capped so the conversation area
        // always keeps its minimum.
        let plan = responsive_plan(1200.0);
        assert!(plan.show_sidebar);
        assert_eq!(plan.sidebar_cap, 1200.0 - CENTRAL_WIDTH_MIN as f32);
        assert!(1200.0 - plan.sidebar_cap >= CENTRAL_WIDTH_MIN as f32);
    }

    #[test]
    fn responsive_plan_hides_sidebar_when_window_is_narrow() {
        // The sidebar yields before the transcript becomes a cramped column.
        let threshold = (SIDEBAR_WIDTH_MIN + CENTRAL_WIDTH_MIN) as f32;
        assert!(responsive_plan(threshold).show_sidebar);
        assert!(!responsive_plan(threshold - 1.0).show_sidebar);
        assert_eq!(responsive_plan(threshold - 1.0).sidebar_cap, 0.0);
        assert!(!responsive_plan(700.0).show_sidebar);
    }

    #[test]
    fn responsive_plan_sweep_keeps_central_minimum_at_every_width() {
        let central_min = CENTRAL_WIDTH_MIN as f32;
        for width in (400..=2400).map(|w| w as f32) {
            let plan = responsive_plan(width);
            if plan.show_sidebar {
                assert!(plan.sidebar_cap >= SIDEBAR_WIDTH_MIN as f32);
                assert!(
                    width - plan.sidebar_cap >= central_min - f32::EPSILON,
                    "central area squeezed by the sidebar cap at width {width}: {plan:?}"
                );
            } else {
                assert_eq!(plan.sidebar_cap, 0.0);
            }
        }
    }

    #[test]
    fn responsive_plan_rejects_invalid_widths() {
        for width in [0.0, -100.0, f32::NAN, f32::INFINITY] {
            let plan = responsive_plan(width);
            assert!(!plan.show_sidebar, "width {width}: {plan:?}");
            assert_eq!(plan.sidebar_cap, 0.0);
        }
    }
}
