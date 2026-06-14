//! Lua-configurable keymap: match a key combo against the user's bindings and
//! execute the resolved action name.

use std::io;

use crossterm::event::{KeyCode, KeyModifiers};

use super::{App, BoneTerminal};

impl App {
    /// Look up a keymap binding for the given key combo.
    /// Returns the action name if found in the current mode.
    pub(super) fn lookup_keymap(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<String> {
        let mode = if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT {
            "n"
        } else {
            "i"
        };

        let bindings = match mode {
            "n" => &self.lua_keymap.normal,
            "i" => &self.lua_keymap.insert,
            _ => return None,
        };

        for binding in bindings {
            if key_matches(&binding.key, code, modifiers) {
                return Some(binding.action.clone());
            }
        }
        None
    }

    /// Execute a keymap action.
    pub(super) async fn handle_keymap_action(
        &mut self,
        action: String,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        match action.as_str() {
            "toggle_panes" => {
                self.panes_visible = !self.panes_visible;
                self.redraw(term)
            }
            "cycle_approval_mode" => {
                self.approval_mode = self.approval_mode.cycle();
                self.user_config.approval_mode = self.approval_mode;
                self.persist_runtime_config();
                self.redraw(term)
            }
            "cursor_to_start" => {
                self.input.cursor_to_start();
                self.redraw(term)
            }
            "cursor_to_end" => {
                self.input.cursor_to_end();
                self.redraw(term)
            }
            other => {
                eprintln!("bone-lua warn: unknown keymap action '{other}'; ignoring");
                self.redraw(term)
            }
        }
    }
}

/// Match a Lua key string (e.g. "<C-p>", "<S-Tab>") against a KeyCode + modifiers.
fn key_matches(key_str: &str, code: KeyCode, modifiers: KeyModifiers) -> bool {
    let key_str = key_str.trim();
    let mut expected_mods = KeyModifiers::NONE;
    let mut key_part = key_str;

    if key_str.starts_with('<') && key_str.ends_with('>') {
        key_part = &key_str[1..key_str.len() - 1];
        let parts: Vec<&str> = key_part.split('-').collect();
        for part in &parts {
            match *part {
                "C" | "Ctrl" => expected_mods |= KeyModifiers::CONTROL,
                "S" | "Shift" => expected_mods |= KeyModifiers::SHIFT,
                "A" | "Alt" => expected_mods |= KeyModifiers::ALT,
                _ => {}
            }
        }
        key_part = parts.last().copied().unwrap_or(&key_part);
    }

    if modifiers != expected_mods {
        return false;
    }

    match key_part {
        "Tab" => code == KeyCode::Tab,
        "BackTab" | "Backtab" => code == KeyCode::BackTab,
        "Enter" => code == KeyCode::Enter,
        "Esc" | "Escape" => code == KeyCode::Esc,
        "Space" => code == KeyCode::Char(' '),
        "Backspace" => code == KeyCode::Backspace,
        "Delete" => code == KeyCode::Delete,
        "Insert" => code == KeyCode::Insert,
        "Home" => code == KeyCode::Home,
        "End" => code == KeyCode::End,
        "PageUp" => code == KeyCode::PageUp,
        "PageDown" => code == KeyCode::PageDown,
        "Up" => code == KeyCode::Up,
        "Down" => code == KeyCode::Down,
        "Left" => code == KeyCode::Left,
        "Right" => code == KeyCode::Right,
        "F1" | "F2" | "F3" | "F4" | "F5" | "F6" | "F7" | "F8" | "F9" | "F10" | "F11" | "F12" => {
            key_part[1..]
                .parse::<u8>()
                .is_ok_and(|n| code == KeyCode::F(n))
        }
        _ if key_part.len() == 1 => key_part
            .chars()
            .next()
            .is_some_and(|ch| code == KeyCode::Char(ch)),
        _ => false,
    }
}
