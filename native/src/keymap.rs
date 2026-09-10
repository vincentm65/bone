//! Native mirror of the daemon's configurable keymap
//! (`core/src/config/settings.rs::KeymapSettings`). Parses the resolved
//! `keymaps.bindings` array from the daemon's `FrontendState` settings payload
//! and matches egui key events, mirroring `tui/src/ui/app/keymap.rs`.
//!
//! Native never interprets action semantics locally: a matched binding is sent
//! to the daemon as `RuntimeCommand::KeymapDispatch`, which classifies it into a
//! `KeymapDispatchKind` (builtin / slash command / prompt / noop) that the
//! desktop UI then applies. When no binding matches, the existing hardcoded
//! shortcuts remain the fallback.

use eframe::egui;

/// One user-configured binding, as stored in `keymaps.bindings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Lua key string, e.g. `<C-p>` or `<S-Tab>`.
    pub key: String,
    /// Action rhs: a builtin name, `/slash-command`, or prompt text.
    pub action: String,
}

/// Resolved keymap for the connected daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keymap {
    pub bindings: Vec<Binding>,
}

impl Keymap {
    /// Parse `keymaps.bindings` out of the resolved frontend settings JSON
    /// (`FrontendState.settings`). `BoneSettings` is flattened into that payload,
    /// so the array lives at the top-level `keymaps` key. Malformed entries are
    /// skipped so one bad binding never disables the rest of the keymap.
    pub fn parse(settings: &serde_json::Value) -> Self {
        let bindings = settings
            .pointer("/keymaps/bindings")
            .and_then(serde_json::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        let key = entry.get("key")?.as_str()?.trim();
                        let action = entry.get("action")?.as_str()?;
                        if key.is_empty() || action.is_empty() {
                            return None;
                        }
                        Some(Binding {
                            key: key.to_owned(),
                            action: action.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { bindings }
    }

    /// First binding whose key string matches `(key, modifiers)`, mirroring the
    /// TUI's first-match `lookup_keymap` ordering. Test-only helper: the
    /// desktop client matches bindings via `input.consume_key` in `main.rs`.
    #[cfg(test)]
    pub fn lookup(&self, key: egui::Key, modifiers: egui::Modifiers) -> Option<&str> {
        self.bindings.iter().find_map(|binding| {
            let (expected_mods, expected_key) = parse_key(&binding.key)?;
            (expected_key == key && modifiers.matches_logically(expected_mods))
                .then_some(binding.action.as_str())
        })
    }
}

/// Parse a Lua key string (e.g. `<C-p>`, `<S-Tab>`, `q`) into an egui
/// `(modifiers, key)` pair. Returns `None` for unsupported key names so a typo'd
/// binding is skipped rather than swallowing an unrelated key.
pub fn parse_key(key_str: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let key_str = key_str.trim();
    let mut modifiers = egui::Modifiers::NONE;
    let mut key_part = key_str;

    if let Some(inner) = key_str
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    {
        let mut parts: Vec<&str> = inner.split('-').collect();
        key_part = parts.pop().unwrap_or("");
        for part in parts {
            match part {
                "C" | "Ctrl" | "CTRL" => modifiers.ctrl = true,
                "S" | "Shift" | "SHIFT" => modifiers.shift = true,
                "A" | "Alt" | "ALT" => modifiers.alt = true,
                _ => {}
            }
        }
    }

    let key = named_key(key_part).or_else(|| single_char_key(key_part))?;
    Some((modifiers, key))
}

/// Named (multi-character) keys, matching the TUI's `key_matches` table.
fn named_key(name: &str) -> Option<egui::Key> {
    Some(match name {
        "Tab" => egui::Key::Tab,
        "Enter" => egui::Key::Enter,
        "Esc" | "Escape" => egui::Key::Escape,
        "Space" => egui::Key::Space,
        "Backspace" => egui::Key::Backspace,
        "Delete" => egui::Key::Delete,
        "Insert" => egui::Key::Insert,
        "Home" => egui::Key::Home,
        "End" => egui::Key::End,
        "PageUp" => egui::Key::PageUp,
        "PageDown" => egui::Key::PageDown,
        "Up" => egui::Key::ArrowUp,
        "Down" => egui::Key::ArrowDown,
        "Left" => egui::Key::ArrowLeft,
        "Right" => egui::Key::ArrowRight,
        name if name.len() > 1 && name.starts_with('F') => {
            let n: u8 = name[1..].parse().ok()?;
            f_key(n)?
        }
        _ => return None,
    })
}

fn f_key(n: u8) -> Option<egui::Key> {
    Some(match n {
        1 => egui::Key::F1,
        2 => egui::Key::F2,
        3 => egui::Key::F3,
        4 => egui::Key::F4,
        5 => egui::Key::F5,
        6 => egui::Key::F6,
        7 => egui::Key::F7,
        8 => egui::Key::F8,
        9 => egui::Key::F9,
        10 => egui::Key::F10,
        11 => egui::Key::F11,
        12 => egui::Key::F12,
        13 => egui::Key::F13,
        14 => egui::Key::F14,
        15 => egui::Key::F15,
        16 => egui::Key::F16,
        17 => egui::Key::F17,
        18 => egui::Key::F18,
        19 => egui::Key::F19,
        20 => egui::Key::F20,
        21 => egui::Key::F21,
        22 => egui::Key::F22,
        23 => egui::Key::F23,
        24 => egui::Key::F24,
        25 => egui::Key::F25,
        26 => egui::Key::F26,
        27 => egui::Key::F27,
        28 => egui::Key::F28,
        29 => egui::Key::F29,
        30 => egui::Key::F30,
        31 => egui::Key::F31,
        32 => egui::Key::F32,
        33 => egui::Key::F33,
        34 => egui::Key::F34,
        35 => egui::Key::F35,
        _ => return None,
    })
}

/// Single-character keys (letters, digits, common punctuation).
fn single_char_key(name: &str) -> Option<egui::Key> {
    let mut chars = name.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(match ch {
        'a'..='z' => letter_key(ch.to_ascii_uppercase())?,
        'A'..='Z' => letter_key(ch)?,
        '0'..='9' => digit_key(ch)?,
        ' ' => egui::Key::Space,
        '-' => egui::Key::Minus,
        '=' => egui::Key::Equals,
        ',' => egui::Key::Comma,
        '.' => egui::Key::Period,
        '/' => egui::Key::Slash,
        '\\' => egui::Key::Backslash,
        ';' => egui::Key::Semicolon,
        '\'' => egui::Key::Quote,
        '`' => egui::Key::Backtick,
        '[' => egui::Key::OpenBracket,
        ']' => egui::Key::CloseBracket,
        _ => return None,
    })
}

fn letter_key(ch: char) -> Option<egui::Key> {
    Some(match ch {
        'A' => egui::Key::A,
        'B' => egui::Key::B,
        'C' => egui::Key::C,
        'D' => egui::Key::D,
        'E' => egui::Key::E,
        'F' => egui::Key::F,
        'G' => egui::Key::G,
        'H' => egui::Key::H,
        'I' => egui::Key::I,
        'J' => egui::Key::J,
        'K' => egui::Key::K,
        'L' => egui::Key::L,
        'M' => egui::Key::M,
        'N' => egui::Key::N,
        'O' => egui::Key::O,
        'P' => egui::Key::P,
        'Q' => egui::Key::Q,
        'R' => egui::Key::R,
        'S' => egui::Key::S,
        'T' => egui::Key::T,
        'U' => egui::Key::U,
        'V' => egui::Key::V,
        'W' => egui::Key::W,
        'X' => egui::Key::X,
        'Y' => egui::Key::Y,
        'Z' => egui::Key::Z,
        _ => return None,
    })
}

fn digit_key(ch: char) -> Option<egui::Key> {
    Some(match ch {
        '0' => egui::Key::Num0,
        '1' => egui::Key::Num1,
        '2' => egui::Key::Num2,
        '3' => egui::Key::Num3,
        '4' => egui::Key::Num4,
        '5' => egui::Key::Num5,
        '6' => egui::Key::Num6,
        '7' => egui::Key::Num7,
        '8' => egui::Key::Num8,
        '9' => egui::Key::Num9,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_reads_flattened_keymaps_bindings() {
        let settings = json!({
            "version": 2,
            "revision": 7,
            "keymaps": {
                "bindings": [
                    {"key": "<C-p>", "action": "toggle_panes"},
                    {"key": "<S-Tab>", "action": "/help"},
                    {"key": "", "action": "ignored"},
                    {"key": "x", "action": ""},
                    {"key": "<C-q>"}
                ]
            }
        });
        let keymap = Keymap::parse(&settings);
        assert_eq!(
            keymap.bindings,
            vec![
                Binding {
                    key: "<C-p>".into(),
                    action: "toggle_panes".into()
                },
                Binding {
                    key: "<S-Tab>".into(),
                    action: "/help".into()
                },
            ]
        );
    }

    #[test]
    fn parse_absent_keymaps_yields_empty() {
        assert!(Keymap::parse(&json!({"version": 2})).bindings.is_empty());
        assert!(Keymap::parse(&json!(null)).bindings.is_empty());
    }

    #[test]
    fn parse_key_handles_modifiers_and_names() {
        let (mods, key) = parse_key("<C-p>").unwrap();
        assert!(mods.ctrl && !mods.shift && !mods.alt);
        assert_eq!(key, egui::Key::P);

        let (mods, key) = parse_key("<S-Tab>").unwrap();
        assert!(mods.shift && !mods.ctrl);
        assert_eq!(key, egui::Key::Tab);

        let (mods, key) = parse_key("<C-A-Up>").unwrap();
        assert!(mods.ctrl && mods.alt);
        assert_eq!(key, egui::Key::ArrowUp);

        let (mods, key) = parse_key("F5").unwrap();
        assert!(mods.is_none());
        assert_eq!(key, egui::Key::F5);

        assert!(parse_key("<C-NotAKey>").is_none());
        assert!(parse_key("").is_none());
    }

    #[test]
    fn lookup_matches_first_binding_and_modifiers() {
        let keymap = Keymap {
            bindings: vec![
                Binding {
                    key: "<C-p>".into(),
                    action: "toggle_panes".into(),
                },
                Binding {
                    key: "<C-p>".into(),
                    action: "second".into(),
                },
                Binding {
                    key: "<S-Tab>".into(),
                    action: "/help".into(),
                },
            ],
        };
        assert_eq!(
            keymap.lookup(egui::Key::P, egui::Modifiers::CTRL),
            Some("toggle_panes")
        );
        assert_eq!(
            keymap.lookup(egui::Key::Tab, egui::Modifiers::SHIFT),
            Some("/help")
        );
        // No binding for a bare `p`; the ctrl pattern must not match it.
        assert_eq!(keymap.lookup(egui::Key::P, egui::Modifiers::NONE), None);
    }
}
