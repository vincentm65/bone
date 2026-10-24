//! The GUI's keyboard. Keys reach the node with the keyboard focus first
//! (its `on_key`, tab between nodes), then the shared key engine
//! (`bone_ui::lua::input`: windows', panels' and Lua's keys, keymaps and
//! sequences) when it has something to do with them; typing and editing
//! the prompt stay with egui's text box, so input methods, selection and
//! native editing work. The pointer is the nodes' (`nodeview`).

use bone_proto::methods::{QueueMode, SessionRef, TurnCancel};
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
        // A text input of the view has the keyboard: editing keys are its
        // own, not the prompt's.
        if self.view.input_focus.is_some()
            && ctx != Context::Popup
            && self.focused_panel().is_none()
            && matches!(
                self.lua.keymaps.lookup(&ctx, &[k]).action,
                Some(Action::Builtin(b)) if NATIVE.contains(&b) || b == Builtin::Submit
            )
        {
            return Route::Native;
        }
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

    /// A key for the node with the keyboard focus: its `on_key`, then tab
    /// and the arrows move the focus, enter and space click it, esc gives
    /// the keyboard back to the prompt. Typing goes back to the prompt too,
    /// except in a modal, which keeps every key it does not pass on.
    /// Whether the key was used.
    fn node_key(&mut self, k: Key) -> bool {
        // A text input with the keyboard edits with it; its `on_key` sees
        // the other keys first (a search field over a list it moves in).
        if let Some(input) = self.view.input_focus.clone() {
            if k.text().is_some() {
                return false;
            }
            let cb = self.view.input_keys.get(&input).copied();
            let taken = cb.is_some_and(|cb| self.call_key(cb, &bone_ui::keys::format(&k)));
            if taken {
                self.view.invalidate();
            }
            return taken;
        }
        if self.context() == Context::Popup || self.focused_panel().is_some() {
            return false;
        }
        let vs = &mut self.view;
        if let Some(until) = vs.guard_until {
            if std::time::Instant::now() < until {
                return true;
            }
            vs.guard_until = None;
        }
        let trapped = self.view.trap.is_some();
        let Some(node) = self.view.focused_node().cloned() else {
            // A modal with nothing to focus still keeps typing from the
            // prompt under it.
            return trapped && k.text().is_some();
        };
        let name = bone_ui::keys::format(&k);
        if let Some(cb) = node.on_key
            && self.call_key(cb, &name)
        {
            self.view.invalidate();
            return true;
        }
        match name.as_str() {
            "tab" | "down" => self.view.move_focus(true),
            "shift+tab" | "up" => self.view.move_focus(false),
            "enter" | "space" => {
                if let Some(cb) = node.click {
                    self.activate(cb);
                }
            }
            "esc" if !trapped => self.view.focused = None,
            _ if k.text().is_some() => {
                if trapped {
                    return true;
                }
                self.view.focused = None;
                return false;
            }
            // Editing keys would edit a prompt that does not have the
            // keyboard; the rest (ctrl+c, ctrl+o, ...) are the window's.
            _ => {
                let ctx = self.context();
                return matches!(
                    self.lua.keymaps.lookup(&ctx, &[k]).action,
                    Some(Action::Builtin(b)) if NATIVE.contains(&b) || b == Builtin::Submit
                );
            }
        }
        self.view.invalidate();
        true
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
        let node_had_keys = self.view.focused_node().is_some();
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
                    // Plain characters arrive as text; while a node has the
                    // keyboard (and egui's focus) its text is all it gets.
                    Some(k) if k.text().is_some() => {
                        if self.view.focused_node().is_none() {
                            keep.push(ev);
                        }
                    }
                    Some(k) if self.node_key(k) => {}
                    Some(k) => match self.route(k) {
                        Route::Engine => input::handle_key(self, k),
                        Route::Done => {}
                        Route::Native => keep.push(ev),
                    },
                    None => keep.push(ev),
                },
                // To the prompt, or to Lua's paste event (a focused
                // panel's, such as a terminal's).
                egui::Event::Paste(t)
                    if self.view.input_focus.is_none()
                        && (self.prompt_has_keys() || self.focused_panel().is_some()) =>
                {
                    let t = t.clone();
                    self.paste(&t);
                }
                egui::Event::Text(t) => {
                    let keys: Vec<Key> = t.chars().map(Key::char).collect();
                    if self.view.focused_node().is_some() || self.view.trap.is_some() {
                        let taken: Vec<bool> = keys.iter().map(|k| self.node_key(*k)).collect();
                        if taken.iter().all(|t| *t) {
                            continue;
                        }
                    }
                    // Typing goes to a text input that has the keyboard, even
                    // inside a window or panel that has the keys.
                    if self.view.input_focus.is_none() && keys.iter().any(|k| self.engine_wants(*k))
                    {
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
        // The keyboard came back to the prompt: it has it this frame, so
        // the key that brought it back is typed there.
        if node_had_keys && self.view.focused_node().is_none() {
            ctx.memory_mut(|m| m.request_focus(crate::prompt::edit_id()));
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

    /// `prompt/changed`, when the prompt is not what was last reported.
    /// The text and cursor are compared first, so an idle frame builds no
    /// JSON and copies nothing.
    pub(crate) fn prompt_changed(&mut self) {
        let anchor = self.prompt.anchor;
        let cursor = self.prompt.cursor;
        let same = self
            .lua
            .last_prompt
            .as_ref()
            .is_some_and(|(text, images, a, c)| {
                text == &self.draft && images == &self.images && *a == anchor && *c == cursor
            });
        if same {
            return;
        }
        let (cursor_json, selection) = self.cursor_json();
        self.lua.last_prompt = Some((self.draft.clone(), self.images.clone(), anchor, cursor));
        // The view may show the prompt's text (the / menu).
        self.view.invalidate();
        let data = serde_json::json!({
            "text": self.draft,
            "images": self.images,
            "cursor": cursor_json,
            "selection": selection,
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
            Submit => self.submit(),
            QueueSteer => self.submit_as(Some(QueueMode::Steer)),
            QueueNext => self.submit_as(Some(QueueMode::Next)),
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
            // Through the nodes the keyboard reaches, with the prompt
            // among them; without any, through the panels.
            FocusNext | FocusPrev if !self.view.focusables.is_empty() => {
                self.view.move_focus(b == FocusNext);
                self.view.invalidate();
            }
            FocusNext => input::cycle_focus(self, true),
            FocusPrev => input::cycle_focus(self, false),
            FocusPrompt => {
                let _ = bone_ui::lua::panels::focus_panel(self, None);
            }
            // The text box has the arrows; these scroll the chat, as in
            // the TUI (pageup, shift+up, ctrl+home, the wheel stays egui's).
            ScrollUp | ScrollDown | PageUp | PageDown | ScrollTop | ScrollBottom => {
                self.scroll_view_chat(b);
            }
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
}
