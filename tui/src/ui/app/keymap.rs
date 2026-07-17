//! Lua-configurable keymap: match a key combo against the user's bindings and
//! execute the resolved action name.

use std::io;
use std::process::Command;

use base64::Engine;
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

        // Hard-coded fallback: paste image on Ctrl+V / Alt+V (mimics the
        // default insert binding users expect without explicit configuration).
        if code == KeyCode::Char('v')
            && (modifiers == KeyModifiers::CONTROL
                || modifiers == KeyModifiers::ALT
                || modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT))
        {
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
        self.command_tx
            .send(crate::runtime::RuntimeCommand::KeymapDispatch { action })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "runtime disconnected"))?;
        let kind = loop {
            let event =
                self.events_rx.recv().await.map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime disconnected")
                })?;
            if let crate::runtime::RuntimeEvent::KeymapDispatched { kind } = event {
                break kind;
            }
            self.apply_idle_event(event);
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
                self.send_message(term).await
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

fn clipboard_image() -> Result<crate::llm::ImageData, String> {
    // `arboard` has no Android backend, so there we rely solely on the external
    // clipboard command fallback.
    #[cfg(not(target_os = "android"))]
    match arboard_clipboard_image() {
        Ok(image) => Ok(image),
        Err(arboard_err) => match external_clipboard_image() {
            Ok(image) => Ok(image),
            Err(external_err) => Err(format!("{arboard_err}; fallback failed: {external_err}")),
        },
    }

    #[cfg(target_os = "android")]
    external_clipboard_image()
}

#[cfg(not(target_os = "android"))]
fn arboard_clipboard_image() -> Result<crate::llm::ImageData, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|err| format!("clipboard unavailable: {err}"))?;
    let image = clipboard
        .get_image()
        .map_err(|err| format!("clipboard has no image: {err}"))?;

    let mut png_bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut png_bytes, image.width as u32, image.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .map_err(|err| format!("PNG header failed: {err}"))?
        .write_image_data(image.bytes.as_ref())
        .map_err(|err| format!("PNG encode failed: {err}"))?;

    Ok(png_image_data(png_bytes))
}

#[cfg(not(target_os = "android"))]
fn png_image_data(png_bytes: Vec<u8>) -> crate::llm::ImageData {
    crate::llm::ImageData {
        media_type: "image/png".to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(png_bytes),
    }
}

fn external_clipboard_image() -> Result<crate::llm::ImageData, String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && let Ok(image) = run_clipboard_command("wl-paste", &["--type", "image/png"])
    {
        return Ok(image);
    }

    run_clipboard_command(
        "xclip",
        &["-selection", "clipboard", "-t", "image/png", "-o"],
    )
    .or_else(|_| {
        run_clipboard_command(
            "xclip",
            &["-selection", "clipboard", "-t", "image/jpeg", "-o"],
        )
    })
    .or_else(|_| {
        run_clipboard_command(
            "xclip",
            &["-selection", "clipboard", "-t", "image/webp", "-o"],
        )
    })
}

fn run_clipboard_command(command: &str, args: &[&str]) -> Result<crate::llm::ImageData, String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|err| format!("{command} failed to start: {err}"))?;
    if !output.status.success() || output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("{command} returned no image")
        } else {
            format!("{command}: {stderr}")
        });
    }

    let media_type = match args.last().copied() {
        Some("image/jpeg") => "image/jpeg",
        Some("image/webp") => "image/webp",
        _ => "image/png",
    };

    Ok(crate::llm::ImageData {
        media_type: media_type.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(output.stdout),
    })
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
