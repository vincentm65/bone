//! Keys and their names: `"ctrl+s"`, `"alt+enter"`, `"shift+tab"`,
//! `"pageup"`, `"f5"`, `"wheelup"`, or a single character like `"?"`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Key {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        let mods = mods
            & (KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SHIFT
                | KeyModifiers::SUPER);
        // One canonical form per key: shift is folded into characters, and
        // ctrl+letter is lowercase.
        match code {
            KeyCode::Char(c) => {
                let c = if mods.contains(KeyModifiers::CONTROL) {
                    c.to_ascii_lowercase()
                } else {
                    c
                };
                Key {
                    code: KeyCode::Char(c),
                    mods: mods - KeyModifiers::SHIFT,
                }
            }
            KeyCode::BackTab => Key {
                code: KeyCode::BackTab,
                mods: mods - KeyModifiers::SHIFT,
            },
            _ => Key { code, mods },
        }
    }

    pub fn char(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    pub fn ctrl_c() -> Self {
        Self::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    /// The character this key types, if any.
    pub fn text(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(c) if self.mods.is_empty() => Some(c),
            _ => None,
        }
    }
}

impl From<KeyEvent> for Key {
    fn from(e: KeyEvent) -> Self {
        Key::new(e.code, e.modifiers)
    }
}

/// The mouse wheel, bound like keys. Terminals never send these F-keys.
pub const WHEEL_UP: KeyCode = KeyCode::F(62);
pub const WHEEL_DOWN: KeyCode = KeyCode::F(63);

const NAMES: &[(&str, KeyCode)] = &[
    ("wheelup", WHEEL_UP),
    ("wheeldown", WHEEL_DOWN),
    ("enter", KeyCode::Enter),
    ("esc", KeyCode::Esc),
    ("tab", KeyCode::Tab),
    ("backspace", KeyCode::Backspace),
    ("delete", KeyCode::Delete),
    ("insert", KeyCode::Insert),
    ("space", KeyCode::Char(' ')),
    ("up", KeyCode::Up),
    ("down", KeyCode::Down),
    ("left", KeyCode::Left),
    ("right", KeyCode::Right),
    ("home", KeyCode::Home),
    ("end", KeyCode::End),
    ("pageup", KeyCode::PageUp),
    ("pagedown", KeyCode::PageDown),
];

const ALIASES: &[(&str, &str)] = &[
    ("return", "enter"),
    ("escape", "esc"),
    ("del", "delete"),
    ("bs", "backspace"),
];

/// Parse a key name like `"ctrl+shift+up"`.
pub fn parse(s: &str) -> Result<Key, String> {
    let raw = s.trim();
    let lower = raw.to_ascii_lowercase();
    if lower.is_empty() {
        return Err("empty key".into());
    }
    // `+` alone, or a trailing `+` as the key itself (`ctrl++`).
    let (mods_part, name) = match lower.strip_suffix("++") {
        Some(m) => (m, "+"),
        None if lower == "+" => ("", "+"),
        None => lower.rsplit_once('+').unwrap_or(("", lower.as_str())),
    };
    let mut mods = KeyModifiers::NONE;
    for m in mods_part.split('+').filter(|m| !m.is_empty()) {
        mods |= match m {
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "alt" | "meta" | "option" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            "super" | "cmd" | "command" => KeyModifiers::SUPER,
            other => return Err(format!("unknown modifier {other:?} in {s:?}")),
        };
    }
    let name = ALIASES
        .iter()
        .find(|(a, _)| *a == name)
        .map_or(name, |(_, n)| *n);
    let code = if let Some((_, code)) = NAMES.iter().find(|(n, _)| *n == name) {
        *code
    } else if let Some(n) = name
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=24).contains(n))
    {
        KeyCode::F(n)
    } else if name.chars().count() == 1 {
        // Keep the original case for plain characters (`"G"`, `"?"`).
        KeyCode::Char(raw.chars().last().unwrap())
    } else {
        return Err(format!("unknown key {s:?}"));
    };
    if code == KeyCode::Tab && mods.contains(KeyModifiers::SHIFT) {
        return Ok(Key::new(KeyCode::BackTab, mods));
    }
    Ok(Key::new(code, mods))
}

/// Parse a whitespace-separated key sequence, such as `"g g"` or
/// `"ctrl+x enter"`.
pub fn parse_sequence(s: &str) -> Result<Vec<Key>, String> {
    let sequence: Vec<Key> = s.split_whitespace().map(parse).collect::<Result<_, _>>()?;
    if sequence.is_empty() {
        return Err("empty key sequence".into());
    }
    Ok(sequence)
}

/// The name of a key, as `parse` reads it.
pub fn format(k: &Key) -> String {
    let name = match k.code {
        KeyCode::BackTab => "tab".to_owned(),
        KeyCode::Char(' ') => "space".to_owned(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) if n <= 24 => format!("f{n}"),
        code => NAMES
            .iter()
            .find(|(_, c)| *c == code)
            .map_or("?", |(n, _)| *n)
            .to_owned(),
    };
    let mut out = String::new();
    if k.mods.contains(KeyModifiers::SUPER) {
        out.push_str("super+");
    }
    if k.mods.contains(KeyModifiers::CONTROL) {
        out.push_str("ctrl+");
    }
    if k.mods.contains(KeyModifiers::ALT) {
        out.push_str("alt+");
    }
    if k.mods.contains(KeyModifiers::SHIFT) || k.code == KeyCode::BackTab {
        out.push_str("shift+");
    }
    out + &name
}

#[cfg(test)]
/// Format a key sequence using the canonical names accepted by `parse_sequence`.
pub fn format_sequence(sequence: &[Key]) -> String {
    sequence.iter().map(format).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_round_trip() {
        for s in [
            "ctrl+s",
            "alt+enter",
            "shift+tab",
            "pageup",
            "f5",
            "ctrl+shift+up",
            "space",
            "?",
            "G",
            "ctrl++",
            "wheelup",
            "shift+wheeldown",
        ] {
            assert_eq!(format(&parse(s).unwrap()), s, "{s}");
        }
        assert_eq!(parse("Ctrl+S").unwrap(), parse("ctrl+s").unwrap());
        assert_eq!(parse("return").unwrap(), parse("enter").unwrap());
        assert_eq!(parse("+").unwrap(), Key::char('+'));
        assert!(parse("hyper+x").is_err() && parse("nope").is_err() && parse("").is_err());
    }

    #[test]
    fn parse_sequences_and_format() {
        let sequence = parse_sequence("  ctrl+x enter  space ").unwrap();
        assert_eq!(format_sequence(&sequence), "ctrl+x enter space");
        assert_eq!(
            parse_sequence("ctrl+x enter"),
            Ok(vec![parse("ctrl+x").unwrap(), parse("enter").unwrap()])
        );
        assert!(parse_sequence("").is_err());
        assert!(parse_sequence("   ").is_err());
        assert!(parse_sequence("ctrl+x nope").is_err());
    }

    #[test]
    fn events_normalize() {
        let shifted = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(Key::from(shifted), Key::char('G'));
        let ctrl = KeyEvent::new(
            KeyCode::Char('W'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(Key::from(ctrl), parse("ctrl+w").unwrap());
        assert_eq!(Key::char('x').text(), Some('x'));
        assert_eq!(parse("ctrl+x").unwrap().text(), None);
    }
}
