//! The GUI's keyboard. Keys reach the shared key engine (`bone_ui::lua::
//! input`: windows', panels' and Lua's keys, keymaps and sequences) when it
//! has something to do with them; typing and editing the prompt stay with
//! egui's text box, so input methods, selection and native editing work.

use bone_proto::methods::{SessionRef, TurnCancel};
use bone_ui::keymap::{Action, Builtin, Context};
use bone_ui::keys::{Key, KeyCode, KeyModifiers};
use bone_ui::lua::{Host, input};
use eframe::egui;

use crate::Gui;

/// Actions egui's text box does itself while the prompt has the keyboard.
const NATIVE: [Builtin; 15] = [
    Builtin::Left,
    Builtin::Right,
    Builtin::Up,
    Builtin::Down,
    Builtin::WordLeft,
    Builtin::WordRight,
    Builtin::LineStart,
    Builtin::LineEnd,
    Builtin::Backspace,
    Builtin::Delete,
    Builtin::DeleteWord,
    Builtin::DeleteToStart,
    Builtin::DeleteToEnd,
    Builtin::Newline,
    Builtin::PasteClipboard,
];

/// egui's key and modifiers as bone's key, or `None` for a key bone has no
/// name for.
pub fn key(k: egui::Key, m: egui::Modifiers) -> Option<Key> {
    use egui::Key as E;
    let code = match k {
        E::ArrowDown => KeyCode::Down,
        E::ArrowLeft => KeyCode::Left,
        E::ArrowRight => KeyCode::Right,
        E::ArrowUp => KeyCode::Up,
        E::Escape => KeyCode::Esc,
        E::Tab => KeyCode::Tab,
        E::Backspace => KeyCode::Backspace,
        E::Enter => KeyCode::Enter,
        E::Insert => KeyCode::Insert,
        E::Delete => KeyCode::Delete,
        E::Home => KeyCode::Home,
        E::End => KeyCode::End,
        E::PageUp => KeyCode::PageUp,
        E::PageDown => KeyCode::PageDown,
        E::Space => KeyCode::Char(' '),
        E::Colon => KeyCode::Char(':'),
        E::Comma => KeyCode::Char(','),
        E::Minus => KeyCode::Char('-'),
        E::Period => KeyCode::Char('.'),
        E::Plus => KeyCode::Char('+'),
        E::Equals => KeyCode::Char('='),
        E::Semicolon => KeyCode::Char(';'),
        E::Backslash => KeyCode::Char('\\'),
        E::Slash => KeyCode::Char('/'),
        E::Pipe => KeyCode::Char('|'),
        E::Questionmark => KeyCode::Char('?'),
        E::Exclamationmark => KeyCode::Char('!'),
        E::OpenBracket => KeyCode::Char('['),
        E::CloseBracket => KeyCode::Char(']'),
        E::OpenCurlyBracket => KeyCode::Char('{'),
        E::CloseCurlyBracket => KeyCode::Char('}'),
        E::Backtick => KeyCode::Char('`'),
        E::Quote => KeyCode::Char('\''),
        k => {
            let name = k.name();
            if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
                KeyCode::F(n)
            } else {
                let mut chars = name.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c.to_ascii_lowercase()),
                    _ => return None,
                }
            }
        }
    };
    let mut mods = KeyModifiers::NONE;
    for (on, flag) in [
        (m.ctrl, KeyModifiers::CONTROL),
        (m.alt, KeyModifiers::ALT),
        (m.shift, KeyModifiers::SHIFT),
        (m.mac_cmd, KeyModifiers::SUPER),
    ] {
        if on {
            mods |= flag;
        }
    }
    Some(Key::new(code, mods))
}

impl Gui {
    /// Whether the key engine should take `k` rather than the text box.
    fn engine_wants(&mut self, k: Key) -> bool {
        let ctx = self.context();
        if ctx == Context::Popup || self.focused_panel().is_some() {
            return true;
        }
        if !self.lua.input.pending_keys.is_empty() {
            return true;
        }
        let lookup = self.lua.keymaps.lookup(&ctx, &[k]);
        match lookup.action {
            Some(Action::Builtin(b)) if NATIVE.contains(&b) => false,
            Some(_) => true,
            None => lookup.prefix,
        }
    }

    /// Take the frame's key events the engine wants, before any widget sees
    /// them; leave the rest (typing, editing) for egui. Events are taken out
    /// first: Lua may wake egui, which must not happen inside its input lock.
    pub(crate) fn route_keys(&mut self, ctx: &egui::Context) {
        input::expire_key_sequence(self);
        if let Some(at) = input::key_sequence_deadline(self) {
            ctx.request_repaint_after(at.saturating_duration_since(std::time::Instant::now()));
        }
        // A question's dialog has the keyboard.
        if !self.asks.is_empty() {
            return;
        }
        let events = ctx.input_mut(|i| std::mem::take(&mut i.events));
        let mut keep = Vec::new();
        for ev in events {
            match &ev {
                egui::Event::Key {
                    key: k,
                    pressed: true,
                    modifiers,
                    ..
                } => match key(*k, *modifiers) {
                    // Plain characters arrive as text.
                    Some(k) if k.text().is_some() => keep.push(ev),
                    Some(k) if self.engine_wants(k) => input::handle_key(self, k),
                    _ => keep.push(ev),
                },
                egui::Event::Text(t) => {
                    let keys: Vec<Key> = t.chars().map(Key::char).collect();
                    if keys.iter().any(|k| self.engine_wants(*k)) {
                        for k in keys {
                            input::handle_key(self, k);
                        }
                    } else {
                        keep.push(ev);
                    }
                }
                _ => keep.push(ev),
            }
        }
        ctx.input_mut(|i| {
            keep.append(&mut i.events);
            i.events = keep;
        });
    }

    /// ctrl+c with nothing else to do: stop the turn on screen.
    pub(crate) fn interrupt(&mut self) {
        if !self.running() {
            return;
        }
        let Some(session_id) = self.chats[self.current].session_id().map(str::to_owned) else {
            return;
        };
        self.request::<TurnCancel>(SessionRef { session_id }, |gui, r| {
            if let Err(e) = r {
                gui.error(format!("cannot cancel: {e}"));
            }
        });
    }

    /// A key no keymap took goes to the prompt while it has the keyboard.
    pub(crate) fn type_key(&mut self, ctx: &Context, key: Key) {
        let Some(c) = key.text() else { return };
        if matches!(ctx, Context::Main | Context::Named(_)) && self.focused_panel().is_none() {
            self.draft.push(c);
        }
    }

    /// What a built-in action does in the GUI. Editing the prompt is egui's
    /// text box's (see `NATIVE`); the chat follows the end.
    pub(crate) fn builtin_action(&mut self, b: Builtin) {
        use Builtin::*;
        match b {
            Submit | QueueSteer | QueueNext => self.submit(),
            Newline => self.draft.push('\n'),
            Dismiss => {
                self.draft.clear();
                self.status = None;
            }
            Interrupt => self.interrupt(),
            Quit => self.closed = true,
            QuitIfEmpty if self.draft.is_empty() => self.closed = true,
            NewSession => self.new_chat(),
            FocusNext => input::cycle_focus(self, true),
            FocusPrev => input::cycle_focus(self, false),
            FocusPrompt => {
                let _ = bone_ui::lua::panels::focus_panel(self, None);
            }
            _ => {}
        }
    }
}
