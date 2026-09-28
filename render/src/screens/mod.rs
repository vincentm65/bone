//! Full-screen pages shared by the TUI (alternate screen) and the desktop
//! (page tabs): usage stats, provider setup, the catalog, a live process
//! viewer, and a read-only transcript viewer. Each screen is a state machine:
//! frontends feed it keys, deliver the data it asks for, and draw it into a
//! ratatui frame.

pub mod catalog;
pub mod host;
pub mod picker;
pub mod process;
pub mod setup;
pub mod stats;
pub mod transcript;

use bone_protocol::KeyEvent;

/// The terminal key names screens match on, mirroring crossterm's variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Other,
}

/// A key press with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// A semantic target hit through an existing page row or footer token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TouchAction {
    /// Feed the same key path used by keyboard input.
    Keys(Vec<Key>),
    /// Keep the touch-only API-key editor's IME active.
    ApiKey,
}

impl Key {
    pub fn plain(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
}

/// Build the shortest sequence of existing cursor keys that selects `target`.
/// Picker cursors wrap, so a touch can select an off-screen row without adding a
/// second direct-selection path to the screen state machine.
pub(super) fn cursor_keys(current: usize, target: usize, len: usize) -> Option<Vec<Key>> {
    if len == 0 || target >= len {
        return None;
    }
    let down = (target + len - current.min(len - 1)) % len;
    let up = (current.min(len - 1) + len - target) % len;
    let (code, count) = if down <= up {
        (KeyCode::Down, down)
    } else {
        (KeyCode::Up, up)
    };
    Some((0..count).map(|_| Key::plain(code)).collect())
}

impl From<&KeyEvent> for Key {
    fn from(event: &KeyEvent) -> Self {
        let code = match event.code.as_str() {
            "Char" => event
                .char
                .as_deref()
                .and_then(|c| c.chars().next())
                .map_or(KeyCode::Other, KeyCode::Char),
            "Enter" => KeyCode::Enter,
            "Esc" => KeyCode::Esc,
            "Tab" if event.shift => KeyCode::BackTab,
            "Tab" => KeyCode::Tab,
            "BackTab" => KeyCode::BackTab,
            "Backspace" => KeyCode::Backspace,
            "Delete" => KeyCode::Delete,
            "Up" => KeyCode::Up,
            "Down" => KeyCode::Down,
            "Left" => KeyCode::Left,
            "Right" => KeyCode::Right,
            "PageUp" => KeyCode::PageUp,
            "PageDown" => KeyCode::PageDown,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            _ => KeyCode::Other,
        };
        Self {
            code,
            ctrl: event.ctrl,
            alt: event.alt,
            shift: event.shift,
        }
    }
}
