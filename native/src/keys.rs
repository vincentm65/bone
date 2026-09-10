//! Map egui logical keys to the frontend-neutral [`KeyEvent`] codes the
//! daemon's Lua layer expects.
//!
//! Codes mirror `key_event_from_crossterm` in the TUI (`tui/src/ui/app/stream`)
//! so `ctx.ui.key()` bindings behave identically in both frontends. Character
//! keys use crossterm's `Char(c)` convention; named keys use its variant names.

use bone_protocol::KeyEvent;
use eframe::egui;

fn named(code: &str, ctrl: bool, alt: bool, shift: bool) -> KeyEvent {
    KeyEvent {
        code: code.to_string(),
        char: None,
        ctrl,
        alt,
        shift,
    }
}

fn char_event(c: char, ctrl: bool, alt: bool, shift: bool) -> KeyEvent {
    KeyEvent {
        code: "Char".to_string(),
        char: Some(c.to_string()),
        ctrl,
        alt,
        shift,
    }
}

/// Whether an egui key is a bare modifier key. egui-winit emits `Event::Key`
/// for these, so an interactive key capture must ignore them: the modifier is
/// already carried by the following non-modifier key's `Modifiers`.
pub fn is_modifier(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    )
}

/// Translate an egui key + modifiers into the wire `KeyEvent` used by
/// `ctx.ui.key()`.
pub fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> KeyEvent {
    let ctrl = modifiers.ctrl;
    let alt = modifiers.alt;
    let shift = modifiers.shift;

    match key {
        egui::Key::ArrowDown => named("Down", ctrl, alt, shift),
        egui::Key::ArrowLeft => named("Left", ctrl, alt, shift),
        egui::Key::ArrowRight => named("Right", ctrl, alt, shift),
        egui::Key::ArrowUp => named("Up", ctrl, alt, shift),
        egui::Key::Escape => named("Esc", ctrl, alt, shift),
        egui::Key::Tab => named("Tab", ctrl, alt, shift),
        egui::Key::Backspace => named("Backspace", ctrl, alt, shift),
        egui::Key::Enter => named("Enter", ctrl, alt, shift),
        egui::Key::Insert => named("Insert", ctrl, alt, shift),
        egui::Key::Delete => named("Delete", ctrl, alt, shift),
        egui::Key::Home => named("Home", ctrl, alt, shift),
        egui::Key::End => named("End", ctrl, alt, shift),
        egui::Key::PageUp => named("PageUp", ctrl, alt, shift),
        egui::Key::PageDown => named("PageDown", ctrl, alt, shift),
        // Modifier keys collapse to crossterm's `KeyCode::Modifier(_)`.
        egui::Key::ShiftLeft
        | egui::Key::ShiftRight
        | egui::Key::ControlLeft
        | egui::Key::ControlRight
        | egui::Key::AltLeft
        | egui::Key::AltRight
        | egui::Key::SuperLeft
        | egui::Key::SuperRight => named("Modifier", ctrl, alt, shift),
        // Logical clipboard shortcuts map to the ctrl+char crossterm would see.
        egui::Key::Copy => char_event('c', true, alt, shift),
        egui::Key::Cut => char_event('x', true, alt, shift),
        egui::Key::Paste => char_event('v', true, alt, shift),
        // Punctuation and space arrive as `Char(c)`.
        egui::Key::Space => char_event(' ', ctrl, alt, shift),
        egui::Key::Colon => char_event(':', ctrl, alt, shift),
        egui::Key::Comma => char_event(',', ctrl, alt, shift),
        egui::Key::Minus => char_event('-', ctrl, alt, shift),
        egui::Key::Period => char_event('.', ctrl, alt, shift),
        egui::Key::Plus => char_event('+', ctrl, alt, shift),
        egui::Key::Equals => char_event('=', ctrl, alt, shift),
        egui::Key::Semicolon => char_event(';', ctrl, alt, shift),
        egui::Key::Backslash => char_event('\\', ctrl, alt, shift),
        egui::Key::Slash => char_event('/', ctrl, alt, shift),
        egui::Key::Pipe => char_event('|', ctrl, alt, shift),
        egui::Key::Questionmark => char_event('?', ctrl, alt, shift),
        egui::Key::Exclamationmark => char_event('!', ctrl, alt, shift),
        egui::Key::OpenBracket => char_event('[', ctrl, alt, shift),
        egui::Key::CloseBracket => char_event(']', ctrl, alt, shift),
        egui::Key::OpenCurlyBracket => char_event('{', ctrl, alt, shift),
        egui::Key::CloseCurlyBracket => char_event('}', ctrl, alt, shift),
        egui::Key::Backtick => char_event('`', ctrl, alt, shift),
        egui::Key::Quote => char_event('\'', ctrl, alt, shift),
        egui::Key::IntlBackslash => char_event('\\', ctrl, alt, shift),
        // Remaining keys derive their code from egui's stable `name()`:
        // single-character names are letters (A–Z) or digits (0–9), and `Fn`
        // names are the crossterm function-key codes verbatim.
        other => {
            let name = other.name();
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => {
                    let c = if shift { c } else { c.to_ascii_lowercase() };
                    char_event(c, ctrl, alt, shift)
                }
                (Some(c), None) if c.is_ascii_digit() => char_event(c, ctrl, alt, shift),
                (Some('F'), Some(_)) if name[1..].chars().all(|c| c.is_ascii_digit()) => {
                    named(name, ctrl, alt, shift)
                }
                _ => named("Null", ctrl, alt, shift),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, alt: bool, shift: bool) -> egui::Modifiers {
        egui::Modifiers {
            ctrl,
            alt,
            shift,
            ..Default::default()
        }
    }

    #[test]
    fn named_keys_use_crossterm_codes() {
        assert_eq!(
            key_event(egui::Key::ArrowDown, mods(false, false, false)).code,
            "Down"
        );
        assert_eq!(
            key_event(egui::Key::ArrowLeft, mods(false, false, false)).code,
            "Left"
        );
        assert_eq!(
            key_event(egui::Key::ArrowRight, mods(false, false, false)).code,
            "Right"
        );
        assert_eq!(
            key_event(egui::Key::ArrowUp, mods(false, false, false)).code,
            "Up"
        );
        assert_eq!(
            key_event(egui::Key::Escape, mods(false, false, false)).code,
            "Esc"
        );
        assert_eq!(
            key_event(egui::Key::Enter, mods(false, false, false)).code,
            "Enter"
        );
        assert_eq!(
            key_event(egui::Key::Backspace, mods(false, false, false)).code,
            "Backspace"
        );
        assert_eq!(
            key_event(egui::Key::Tab, mods(false, false, false)).code,
            "Tab"
        );
        assert_eq!(
            key_event(egui::Key::PageUp, mods(false, false, false)).code,
            "PageUp"
        );
        assert_eq!(
            key_event(egui::Key::PageDown, mods(false, false, false)).code,
            "PageDown"
        );
        assert_eq!(
            key_event(egui::Key::Insert, mods(false, false, false)).code,
            "Insert"
        );
        assert_eq!(
            key_event(egui::Key::Delete, mods(false, false, false)).code,
            "Delete"
        );
        assert_eq!(
            key_event(egui::Key::Home, mods(false, false, false)).code,
            "Home"
        );
        assert_eq!(
            key_event(egui::Key::End, mods(false, false, false)).code,
            "End"
        );
    }

    #[test]
    fn letters_and_digits_are_char_events_case_aware() {
        let lower = key_event(egui::Key::A, mods(false, false, false));
        assert_eq!(lower.code, "Char");
        assert_eq!(lower.char.as_deref(), Some("a"));
        let upper = key_event(egui::Key::A, mods(false, false, true));
        assert_eq!(upper.char.as_deref(), Some("A"));
        let digit = key_event(egui::Key::Num7, mods(false, false, false));
        assert_eq!(digit.code, "Char");
        assert_eq!(digit.char.as_deref(), Some("7"));
    }

    #[test]
    fn function_keys_use_fn_names() {
        assert_eq!(
            key_event(egui::Key::F1, mods(false, false, false)).code,
            "F1"
        );
        assert_eq!(
            key_event(egui::Key::F5, mods(false, false, false)).code,
            "F5"
        );
        assert_eq!(
            key_event(egui::Key::F35, mods(false, false, false)).code,
            "F35"
        );
    }

    #[test]
    fn modifiers_and_clipboard_shortcuts_map() {
        let ev = key_event(egui::Key::ControlLeft, mods(true, false, false));
        assert_eq!(ev.code, "Modifier");
        assert_eq!(
            ev.code,
            key_event(egui::Key::SuperRight, mods(false, false, false)).code
        );
        assert!(is_modifier(egui::Key::ControlLeft));
        assert!(is_modifier(egui::Key::ShiftRight));
        assert!(!is_modifier(egui::Key::C));
        assert!(!is_modifier(egui::Key::ArrowUp));
        let copy = key_event(egui::Key::Copy, mods(false, false, false));
        assert_eq!(copy.code, "Char");
        assert_eq!(copy.char.as_deref(), Some("c"));
        assert!(copy.ctrl);
        // Unknown/unsupported keys fall back to crossterm's `Null`.
        assert_eq!(
            key_event(egui::Key::BrowserBack, mods(false, false, false)).code,
            "Null"
        );
    }

    #[test]
    fn punctuation_is_a_char_event() {
        let slash = key_event(egui::Key::Slash, mods(false, false, false));
        assert_eq!(slash.code, "Char");
        assert_eq!(slash.char.as_deref(), Some("/"));
        let space = key_event(egui::Key::Space, mods(false, false, false));
        assert_eq!(space.char.as_deref(), Some(" "));
    }
}
