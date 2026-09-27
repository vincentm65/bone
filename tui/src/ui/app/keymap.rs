//! Lua-configurable keymap: match a key combo against the user's bindings and
//! execute the resolved action name.

use std::io;

use crossterm::event::{KeyCode, KeyModifiers};

use super::{App, BoneTerminal};

impl App {
    /// Look up a keymap binding for the given key combo.
    /// Returns the configured action for the given key combo.
    pub(super) fn lookup_keymap(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<String> {
        for binding in &self.keymaps.bindings {
            if key_matches(&binding.key, code, modifiers) {
                return Some(binding.action.clone());
            }
        }

        // Hard-coded fallback: paste images on Ctrl+V / Alt+V. Ctrl+Shift+V
        // is reserved for the terminal's standard text-paste shortcut.
        if is_image_paste_key(code, modifiers) {
            return Some("paste_image".to_string());
        }
        None
    }

    /// Ask the daemon to resolve and execute a keymap rhs, then apply its
    /// frontend-facing result.
    pub(super) async fn handle_keymap_action(
        &mut self,
        action: String,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let request_id = self.next_request();
        self.command_tx
            .send(crate::runtime::RuntimeCommand::KeymapDispatch {
                request_id: Some(request_id),
                action,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "runtime disconnected"))?;
        let kind = loop {
            match self.events_rx.recv().await {
                Ok(crate::runtime::RuntimeEvent::KeymapDispatched {
                    request_id: response_id,
                    kind,
                }) if response_id == Some(request_id)
                    || (response_id.is_none() && self.synchronization_supported != Some(true)) =>
                {
                    if response_id.is_none() {
                        self.mark_legacy_runtime();
                    }
                    break kind;
                }
                Ok(crate::runtime::RuntimeEvent::Started {
                    request_id,
                    task,
                    display,
                    ..
                }) => {
                    self.adopt_daemon_turn(request_id, task, display, term)
                        .await?;
                }
                Ok(crate::runtime::RuntimeEvent::StreamLagged { .. })
                | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    self.recover_from_event_lag();
                    self.messages.push(crate::chat::Message::system(
                        "keymap result was lost while repairing the event stream; the action may have completed",
                    ));
                    self.redraw(term)?;
                    return Ok(());
                }
                Ok(event) => self.apply_idle_event(event),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "runtime disconnected",
                    ));
                }
            }
        };
        match kind {
            bone_protocol::KeymapDispatchKind::Noop => Ok(()),
            bone_protocol::KeymapDispatchKind::Builtin { action } => {
                self.handle_builtin_keymap_action(&action, term)
            }
            bone_protocol::KeymapDispatchKind::Command { text }
            | bone_protocol::KeymapDispatchKind::Prompt { text } => {
                self.input.buffer = text;
                self.input.cursor_pos = self.input.buffer.chars().count();
                self.submit_message_in_order(term).await
            }
        }
    }

    fn handle_builtin_keymap_action(
        &mut self,
        action: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        match action {
            "toggle_panes" => {
                self.panes_visible = !self.panes_visible;
                self.redraw(term)
            }
            "cycle_approval_mode" => self.cycle_approval_mode(term),
            "cursor_to_start" => {
                self.input.cursor_to_start();
                self.redraw(term)
            }
            "cursor_to_end" => {
                self.input.cursor_to_end();
                self.redraw(term)
            }
            "paste_image" => {
                match clipboard_image() {
                    Ok(image) => self.input.insert_image(image),
                    Err(err) => self.messages.push(crate::chat::Message::system(format!(
                        "image paste failed: {err}"
                    ))),
                }
                self.redraw(term)
            }
            other => {
                bone_core::ext::ctx::runtime_warn_once(format!(
                    "bone-lua warn: unknown keymap action '{other}'; ignoring"
                ));
                self.redraw(term)
            }
        }
    }
}

pub(super) fn is_image_paste_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('v' | 'V'))
        && matches!(modifiers, KeyModifiers::CONTROL | KeyModifiers::ALT)
}

pub(super) use bone_render::clipboard::clipboard_image;

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
        key_part = parts.last().copied().unwrap_or(key_part);
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

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod tests;
