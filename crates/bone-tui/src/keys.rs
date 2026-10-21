//! Keys and their names (from `bone-ui`), and crossterm's key events as
//! keys.

use crossterm::event::{self as ct, KeyEvent};

pub use bone_ui::keys::*;

pub fn from_event(e: KeyEvent) -> Key {
    Key::new(code(e.code), mods(e.modifiers))
}

pub fn code(c: ct::KeyCode) -> KeyCode {
    match c {
        ct::KeyCode::Char(c) => KeyCode::Char(c),
        ct::KeyCode::F(n) => KeyCode::F(n),
        ct::KeyCode::Enter => KeyCode::Enter,
        ct::KeyCode::Esc => KeyCode::Esc,
        ct::KeyCode::Tab => KeyCode::Tab,
        ct::KeyCode::BackTab => KeyCode::BackTab,
        ct::KeyCode::Backspace => KeyCode::Backspace,
        ct::KeyCode::Delete => KeyCode::Delete,
        ct::KeyCode::Insert => KeyCode::Insert,
        ct::KeyCode::Up => KeyCode::Up,
        ct::KeyCode::Down => KeyCode::Down,
        ct::KeyCode::Left => KeyCode::Left,
        ct::KeyCode::Right => KeyCode::Right,
        ct::KeyCode::Home => KeyCode::Home,
        ct::KeyCode::End => KeyCode::End,
        ct::KeyCode::PageUp => KeyCode::PageUp,
        ct::KeyCode::PageDown => KeyCode::PageDown,
        _ => KeyCode::Unknown,
    }
}

pub fn mods(m: ct::KeyModifiers) -> KeyModifiers {
    let mut out = KeyModifiers::NONE;
    for (from, to) in [
        (ct::KeyModifiers::SHIFT, KeyModifiers::SHIFT),
        (ct::KeyModifiers::CONTROL, KeyModifiers::CONTROL),
        (ct::KeyModifiers::ALT, KeyModifiers::ALT),
        (ct::KeyModifiers::SUPER, KeyModifiers::SUPER),
    ] {
        if m.contains(from) {
            out |= to;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_normalize() {
        let shifted = KeyEvent::new(ct::KeyCode::Char('G'), ct::KeyModifiers::SHIFT);
        assert_eq!(from_event(shifted), Key::char('G'));
        let ctrl = KeyEvent::new(
            ct::KeyCode::Char('W'),
            ct::KeyModifiers::CONTROL | ct::KeyModifiers::SHIFT,
        );
        assert_eq!(from_event(ctrl), parse("ctrl+w").unwrap());
        assert_eq!(Key::char('x').text(), Some('x'));
        assert_eq!(parse("ctrl+x").unwrap().text(), None);
    }
}
