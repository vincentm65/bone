//! A frame's input, as the window (or a test) hands it over: keys, text,
//! the pointer, the wheel, the clipboard and dropped files, in the node
//! view's own types.

use std::path::PathBuf;

use super::geom::{Pos, Rect, Vec2};

/// The modifier keys held. `command` is the platform's command key (ctrl,
/// or cmd on a Mac); `logo` is the super / Windows / cmd key itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
        logo: false,
    };
    pub const CTRL: Modifiers = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    pub const ALT: Modifiers = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    pub const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };

    /// The platform's command modifier: cmd on a Mac, ctrl elsewhere.
    pub fn command(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.logo
        } else {
            self.ctrl
        }
    }

    pub fn any(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.logo
    }
}

/// A key on the keyboard, by what it is, not what it types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    Escape,
    Tab,
    Backspace,
    Enter,
    Space,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    /// F1 to F35.
    F(u8),
    /// A key that types `char` without shift: letters (lowercase), digits
    /// and punctuation.
    Char(char),
}

#[allow(non_upper_case_globals)]
impl Key {
    pub const A: Key = Key::Char('a');
    pub const C: Key = Key::Char('c');
    pub const G: Key = Key::Char('g');
    pub const K: Key = Key::Char('k');
    pub const O: Key = Key::Char('o');
    pub const V: Key = Key::Char('v');
    pub const W: Key = Key::Char('w');
    pub const X: Key = Key::Char('x');
    pub const Num5: Key = Key::Char('5');
    pub const CloseBracket: Key = Key::Char(']');
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Primary,
    Secondary,
    Middle,
    Back,
    Forward,
}

impl Button {
    /// The `mouse` event's name for it.
    pub fn name(self) -> &'static str {
        match self {
            Button::Primary => "left",
            Button::Secondary => "right",
            Button::Middle => "middle",
            Button::Back => "back",
            Button::Forward => "forward",
        }
    }
}

/// How a wheel's travel is counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelUnit {
    /// Pixels (a trackpad).
    Point,
    /// Lines (a mouse wheel's notches).
    Line,
    Page,
}

/// A wheel's `delta` is positive up and left: toward the start of what
/// scrolls.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Key {
        key: Key,
        pressed: bool,
        repeat: bool,
        mods: Modifiers,
    },
    /// Typed text (after the keyboard layout and input method).
    Text(String),
    /// The input method's text being composed, not typed yet (empty when
    /// it is done or given up).
    Preedit(String),
    /// Text from the clipboard (ctrl+v, the paste key, a middle click).
    Paste(String),
    PointerMoved(Pos),
    PointerButton {
        pos: Pos,
        button: Button,
        pressed: bool,
        mods: Modifiers,
    },
    /// The pointer left the window.
    PointerGone,
    Wheel {
        delta: Vec2,
        unit: WheelUnit,
        mods: Modifiers,
    },
    DroppedFile(PathBuf),
}

/// One frame's input.
#[derive(Clone, Debug)]
pub struct FrameInput {
    pub events: Vec<Event>,
    /// The window's area, in logical pixels.
    pub screen: Rect,
    /// Physical pixels per logical one.
    pub scale: f32,
    /// Whether the window has the keyboard focus.
    pub focused: bool,
    pub fullscreen: bool,
    pub maximized: bool,
    /// The modifiers held at the end of the frame's events.
    pub mods: Modifiers,
    pub time: std::time::Instant,
}

impl FrameInput {
    pub fn new(screen: Rect) -> Self {
        FrameInput {
            events: Vec::new(),
            screen,
            scale: 1.0,
            focused: true,
            fullscreen: false,
            maximized: false,
            mods: Modifiers::NONE,
            time: std::time::Instant::now(),
        }
    }
}

/// bone's key for `key` with `mods`.
pub fn bone_key(key: Key, m: Modifiers) -> bone_ui::keys::Key {
    use bone_ui::keys::{KeyCode, KeyModifiers};
    let code = match key {
        Key::ArrowDown => KeyCode::Down,
        Key::ArrowLeft => KeyCode::Left,
        Key::ArrowRight => KeyCode::Right,
        Key::ArrowUp => KeyCode::Up,
        Key::Escape => KeyCode::Esc,
        Key::Tab => KeyCode::Tab,
        Key::Backspace => KeyCode::Backspace,
        Key::Enter => KeyCode::Enter,
        Key::Space => KeyCode::Char(' '),
        Key::Insert => KeyCode::Insert,
        Key::Delete => KeyCode::Delete,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::F(n) => KeyCode::F(n),
        Key::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
    };
    let mut mods = KeyModifiers::NONE;
    for (on, flag) in [
        (m.ctrl, KeyModifiers::CONTROL),
        (m.alt, KeyModifiers::ALT),
        (m.shift, KeyModifiers::SHIFT),
        (m.logo, KeyModifiers::SUPER),
    ] {
        if on {
            mods |= flag;
        }
    }
    bone_ui::keys::Key::new(code, mods)
}
