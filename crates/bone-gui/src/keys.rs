//! The GUI's keyboard. Keys reach the shared key engine (`bone_ui::lua::
//! input`: windows', panels' and Lua's keys, keymaps and sequences) when it
//! has something to do with them; typing and editing the prompt stay with
//! egui's text box, so input methods, selection and native editing work.

use bone_proto::methods::{SessionRef, TurnCancel};
use bone_ui::editor::TextBuffer;
use bone_ui::keymap::{Action, Builtin, Context};
use bone_ui::keys::{Key, KeyCode, KeyModifiers};
use bone_ui::lua::{Host, input};
use eframe::egui;

use crate::Gui;

/// Actions egui's text box does itself while the prompt has the keyboard.
const NATIVE: [Builtin; 14] = [
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
];

enum Route {
    Engine,
    Done,
    Native,
}

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
    /// Where key `k` goes: the engine, nowhere more (Lua already took an
    /// editing action), or the text box.
    fn route(&mut self, k: Key) -> Route {
        let ctx = self.context();
        let native = ctx != Context::Popup
            && self.focused_panel().is_none()
            && self.lua.input.pending_keys.is_empty();
        match self.lua.keymaps.lookup(&ctx, &[k]).action {
            Some(Action::Builtin(b)) if native && NATIVE.contains(&b) => {
                if input::lua_action(self, b) {
                    Route::Done
                } else if matches!(b, Builtin::Up | Builtin::Down) {
                    // The line above or below, else the history, as the
                    // TUI does: the text box never sees them.
                    self.vertical(b == Builtin::Up);
                    Route::Done
                } else {
                    Route::Native
                }
            }
            _ if self.engine_wants(k) => Route::Engine,
            _ => Route::Native,
        }
    }

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
        let hover = ctx.input(|i| i.pointer.hover_pos());
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
                    Some(k) => match self.route(k) {
                        Route::Engine => input::handle_key(self, k),
                        Route::Done => {}
                        Route::Native => keep.push(ev),
                    },
                    None => keep.push(ev),
                },
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    ..
                } => {
                    let Some(at) = self.cell_at(*pos) else {
                        keep.push(ev);
                        continue;
                    };
                    let name = match button {
                        egui::PointerButton::Primary => "left",
                        egui::PointerButton::Secondary => "right",
                        egui::PointerButton::Middle => "middle",
                        _ => "other",
                    };
                    self.lua.dragging = *pressed && name == "left";
                    let m = input::mouse(self, if *pressed { "down" } else { "up" }, name, at);
                    if m.handled || m.popup {
                        continue;
                    }
                    if *pressed && name == "left" {
                        self.click(at);
                    }
                    keep.push(ev);
                }
                egui::Event::PointerMoved(pos) if self.lua.dragging => {
                    if let Some(at) = self.cell_at(*pos)
                        && input::mouse(self, "drag", "left", at).handled
                    {
                        continue;
                    }
                    keep.push(ev);
                }
                egui::Event::MouseWheel { delta, .. } => {
                    let panel = hover
                        .and_then(|p| self.cell_at(p))
                        .and_then(|(x, y)| bone_ui::lua::panels::at(&self.lua.panels, x, y))
                        .map(|p| p.id.clone());
                    match panel {
                        Some(id) if delta.y != 0.0 => {
                            let rows = bone_ui::panel::WHEEL_ROWS;
                            let by = if delta.y > 0.0 { -rows } else { rows };
                            if let Some(p) = self.lua.panels.iter_mut().find(|p| p.id == id) {
                                p.scroll_by(by);
                            }
                            self.changed();
                        }
                        _ => keep.push(ev),
                    }
                }
                egui::Event::Paste(t) if self.prompt_has_keys() => {
                    let t = t.clone();
                    self.paste(&t);
                }
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

    /// The character cell at a pointer position.
    fn cell_at(&self, pos: egui::Pos2) -> Option<(u16, u16)> {
        let (ox, oy, col, row) = self.lua.cells?;
        let (x, y) = ((pos.x - ox) / col, (pos.y - oy) / row);
        (x >= 0.0 && y >= 0.0).then_some((x as u16, y as u16))
    }

    /// A left press nothing took: it focuses the panel under it, or the
    /// prompt.
    fn click(&mut self, (x, y): (u16, u16)) {
        let under =
            bone_ui::lua::panels::at(&self.lua.panels, x, y).map(|p| (p.id.clone(), p.focusable));
        match under {
            Some((id, true)) => {
                let _ = bone_ui::lua::panels::focus_panel(self, Some(&id));
            }
            Some(_) => {}
            None => {
                let in_prompt = (self.lua.geometry.leaves.iter())
                    .any(|(n, r)| n == "prompt" && r.contains(x, y));
                if in_prompt {
                    let _ = bone_ui::lua::panels::focus_panel(self, None);
                }
            }
        }
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

    /// `prompt/changed`, when the prompt's text is not what was last
    /// reported (the cursor is at its end: the text box keeps its own).
    pub(crate) fn prompt_changed(&mut self) {
        let (cursor, selection) = self.cursor_json();
        let state = (self.draft.clone(), self.images.clone(), cursor, selection);
        if state == self.lua.last_prompt {
            return;
        }
        self.lua.last_prompt = state.clone();
        let data = serde_json::json!({
            "text": state.0,
            "images": state.1,
            "cursor": state.2,
            "selection": state.3,
        });
        self.fire_event("prompt/changed", data);
    }

    /// Call `bone.ui[name]()`, a Lua part of the UI.
    fn call_ui(&mut self, name: &str) {
        let r = self.with_api(|lua| {
            let ui: mlua::Table = lua.globals().get::<mlua::Table>("bone")?.get("ui")?;
            match ui.get::<Option<mlua::Function>>(name)? {
                Some(f) => f.call::<()>(()).map(|_| true),
                None => Ok(false),
            }
        });
        match r {
            Ok(true) => {}
            Ok(false) => self.error(format!("bone.ui.{name} is not defined")),
            Err(e) => Host::lua_error(self, &format!("bone.ui.{name}"), &e),
        }
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
            Newline => self.edit_prompt(TextBuffer::newline),
            Up | Down => {
                self.vertical(b == Up);
            }
            Left => self.edit_prompt(TextBuffer::left),
            Right => self.edit_prompt(TextBuffer::right),
            WordLeft => self.edit_prompt(TextBuffer::word_back),
            WordRight => self.edit_prompt(TextBuffer::word_forward),
            LineStart => self.edit_prompt(TextBuffer::line_start),
            LineEnd => self.edit_prompt(TextBuffer::line_end),
            Backspace => self.backspace(),
            Delete => self.edit_prompt(TextBuffer::delete),
            DeleteWord => self.edit_prompt(TextBuffer::delete_word_back),
            DeleteToStart => self.edit_prompt(TextBuffer::delete_to_line_start),
            DeleteToEnd => self.edit_prompt(TextBuffer::delete_to_line_end),
            Dismiss => {
                self.clear_draft();
                self.prompt_changed();
                self.status = None;
            }
            PasteClipboard => self.paste_clipboard(),
            Interrupt => self.interrupt(),
            Quit => self.closed = true,
            QuitIfEmpty if self.draft.is_empty() => self.closed = true,
            NewSession => self.new_chat(),
            Sessions => self.call_ui("sessions"),
            FocusNext => input::cycle_focus(self, true),
            FocusPrev => input::cycle_focus(self, false),
            FocusPrompt => {
                let _ = bone_ui::lua::panels::focus_panel(self, None);
            }
            // The text box has the arrows; these scroll the chat, as in
            // the TUI (pageup, shift+up, ctrl+home, the wheel stays egui's).
            ScrollUp => self.nudge_chat(-3),
            ScrollDown => self.nudge_chat(3),
            PageUp => self.nudge_chat(-(self.chat_page() - 2).max(1)),
            PageDown => self.nudge_chat((self.chat_page() - 2).max(1)),
            ScrollTop => self.chat_scroll = Some(crate::ChatScroll::Top),
            ScrollBottom => self.chat_scroll = Some(crate::ChatScroll::Bottom),
            _ => {}
        }
    }

    /// An editing action run by Lua or a key sequence (the text box does
    /// the ones typed straight at it).
    fn edit_prompt(&mut self, f: impl FnOnce(&mut TextBuffer)) {
        let mut t = self.buffer();
        f(&mut t);
        self.set_buffer(&t);
        self.prompt_changed();
    }

    /// Rows of the chat on screen, for a page key.
    fn chat_page(&self) -> i32 {
        self.lua
            .geometry
            .leaves
            .iter()
            .find(|(n, r)| n == "chat" && r.height > 0)
            .map(|(_, r)| i32::from(r.height))
            .unwrap_or(12)
    }

    fn nudge_chat(&mut self, rows: i32) {
        self.chat_scroll = Some(crate::ChatScroll::By(rows));
    }
}
