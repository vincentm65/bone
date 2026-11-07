//! The GUI's keyboard. Keys reach the node with the keyboard focus first
//! (its `on_key`, tab between nodes), then the shared key engine
//! (`bone_ui::lua::input`: windows', panels' and Lua's keys, keymaps and
//! sequences) when it has something to do with them; typing and editing
//! are left for the text field with the keyboard (the prompt's, or a text
//! input's). The pointer is the nodes' (`nodeview`).

use bone_proto::methods::{QueueMode, SessionRef, TurnCancel};
use bone_ui::editor::TextBuffer;
use bone_ui::keymap::{Action, Builtin, Context};
use bone_ui::keys::{Key, KeyCode, KeyModifiers};
use bone_ui::lua::{Host, input};

use crate::Gui;
use crate::backend::input::{self as ev, Event, bone_key};
use crate::nodeview::{Fx, MouseTaken};

/// Actions the text field does itself while the prompt has the keyboard.
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

/// Where ctrl+c or ctrl+x went.
enum Clip {
    /// Used: a panel's key, or text copied.
    Done,
    /// The text field's, to copy or cut its selection.
    Field,
    /// A key like any other.
    Key,
}

/// A key that also arrives as text (a character, with shift at most).
fn typed(k: Key) -> bool {
    matches!(k.code, KeyCode::Char(_)) && (k.mods - KeyModifiers::SHIFT).is_empty()
}

impl Gui {
    /// Where key `k` goes: the engine, nowhere more (Lua already took an
    /// editing action), or the text box.
    fn route(&mut self, k: Key) -> Route {
        // A modal's native editor uses editing bindings even if the shared
        // engine still sees the terminal's context underneath it.
        let trapped = self.view.trap.is_some();
        let ctx = self.context();
        // Shared popup handlers own navigation, even when their node card
        // also traps focus. Only node-only modals use prompt bindings.
        let ctx = if trapped && ctx != Context::Popup {
            Context::Main
        } else {
            ctx
        };
        let native = ctx != Context::Popup && (trapped || self.focused_panel().is_none());
        // A text input of the view has the keyboard: editing keys are its
        // own, not the prompt's.
        if self.view.input_focus.is_some()
            && native
            && matches!(
                self.lua.keymaps.lookup(&ctx, &[k]).action,
                Some(Action::Builtin(b)) if NATIVE.contains(&b) || b == Builtin::Submit
            )
        {
            return Route::Native;
        }
        let native = native && self.lua.input.pending_keys.is_empty();
        match self.lua.keymaps.lookup(&ctx, &[k]).action {
            Some(Action::Builtin(b)) if native && NATIVE.contains(&b) => {
                if input::native_key(self, k) || input::lua_action(self, b) {
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
            // A key that types arrives again as text, and is seen there.
            _ if native && !typed(k) && input::native_key(self, k) => Route::Done,
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
            // Ctrl+C interrupts the chat, even when a desktop text input has
            // the keyboard; do not let its key callback consume the shortcut.
            if k == Key::ctrl_c() || k.text().is_some() {
                return false;
            }
            let cb = self.view.input_keys.get(&input).copied();
            let taken = cb.is_some_and(|cb| self.call_key(cb, &bone_ui::keys::format(&k)));
            if taken {
                self.view.invalidate();
            }
            return taken;
        }
        let trapped = self.view.trap.is_some();
        // Node-only modals sit above the shared windows and panels without
        // changing their focus. Their guard and keys still take precedence.
        if !trapped && (self.context() == Context::Popup || self.focused_panel().is_some()) {
            return false;
        }
        let vs = &mut self.view;
        if let Some(until) = vs.guard_until {
            if std::time::Instant::now() < until {
                return true;
            }
            vs.guard_until = None;
        }
        // A focused window handles keys in its own `on_key`. The node card
        // traps the pointer (`block_input`); focus movement here would eat
        // typing, arrows and esc before that handler sees them.
        if trapped && self.context() == Context::Popup {
            if let Some(node) = self.view.focused_node().cloned()
                && let Some(cb) = node.on_key
                && self.call_key(cb, &bone_ui::keys::format(&k))
            {
                self.view.invalidate();
                return true;
            }
            return false;
        }
        // Use the prompt's editing bindings, not the panel underneath:
        // terminal contexts need not bind left, delete, enter, etc.
        let ctx = if trapped {
            Context::Main
        } else {
            self.context()
        };
        let editing = matches!(
            self.lua.keymaps.lookup(&ctx, &[k]).action,
            Some(Action::Builtin(b)) if NATIVE.contains(&b) || b == Builtin::Submit
                || (trapped && b == Builtin::PasteClipboard)
        );
        // Plain typing and navigation stay in the modal even if unbound.
        // Modified shortcuts and function keys may still reach the window.
        let modal_key = trapped
            && !matches!(k.code, KeyCode::F(_) | KeyCode::Unknown)
            && !k
                .mods
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        let Some(node) = self.view.focused_node().cloned() else {
            // Even an empty modal must not send input to what's underneath.
            return trapped && (modal_key || editing);
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
            _ => return modal_key || editing,
        }
        self.view.invalidate();
        true
    }

    /// Whether the key engine should take `k` rather than the text box.
    fn engine_wants(&mut self, k: Key) -> bool {
        // The shared key engine owns ctrl+c (when it copies nothing) so the
        // configured interrupt action can stop the current chat.
        if k == Key::ctrl_c() {
            return true;
        }
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

    /// Take the frame's key events the engine wants; leave the rest
    /// (typing, editing) in `fx.typed` for the text field with the keyboard.
    pub(crate) fn route_keys(&mut self, fx: &mut Fx) {
        input::expire_key_sequence(self);
        if let Some(at) = input::key_sequence_deadline(self) {
            fx.out
                .repaint_after(at.saturating_duration_since(std::time::Instant::now()));
        }
        let mut keep = Vec::new();
        let mut alt_text: Option<char> = None;
        for e in fx.input.events.clone() {
            match &e {
                Event::Key {
                    key,
                    pressed: true,
                    mods,
                    ..
                } => {
                    let k = bone_key(*key, *mods);
                    // ctrl+c and ctrl+x: a panel with the keyboard hears them
                    // first; then they copy or cut a selection; else they are
                    // keys (ctrl+c stops the turn).
                    if mods.command()
                        && !mods.alt
                        && !mods.shift
                        && matches!(key, ev::Key::Char('c' | 'x'))
                    {
                        match self.clipboard_key(fx, *key == ev::Key::Char('x'), k) {
                            Clip::Done => continue,
                            Clip::Field => {
                                keep.push(e);
                                continue;
                            }
                            Clip::Key => {}
                        }
                    }
                    // Plain characters arrive as text too; while a node has
                    // the keyboard, its text is all it gets.
                    if k.text().is_some() {
                        alt_text = None;
                        if self.view.focused_node().is_none() {
                            keep.push(e);
                        }
                        continue;
                    }
                    alt_text = None;
                    let taken = if self.node_key(k) {
                        true
                    } else {
                        match self.route(k) {
                            Route::Engine => {
                                input::handle_key(self, k);
                                true
                            }
                            Route::Done => true,
                            Route::Native => false,
                        }
                    };
                    // The window also sends Text for Alt+letter. Once its key
                    // was handled, that text is not another key (notably
                    // Alt+W must not resolve Alt+W W). Leave AltGr and
                    // composed/nonmatching text alone.
                    if taken
                        && mods.alt
                        && !mods.ctrl
                        && !mods.logo
                        && let KeyCode::Char(c) = k.code
                    {
                        alt_text = Some(c);
                    }
                    if !taken {
                        keep.push(e);
                    }
                }
                // A modal without a text field has nowhere to paste. Do not
                // fire the global paste hook of a terminal underneath it.
                Event::Paste(_) if self.view.trap.is_some() && self.view.input_focus.is_none() => {}
                // To the prompt, or to Lua's paste event (a focused
                // panel's, such as a terminal's).
                Event::Paste(t)
                    if self.view.input_focus.is_none()
                        && (self.prompt_has_keys() || self.focused_panel().is_some()) =>
                {
                    let t = t.clone();
                    self.paste(&t);
                }
                Event::Text(t) => {
                    if let Some(c) = alt_text.take() {
                        let mut chars = t.chars();
                        if chars
                            .next()
                            .is_some_and(|text| text.eq_ignore_ascii_case(&c))
                            && chars.next().is_none()
                        {
                            continue;
                        }
                    }
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
                    } else if self.view.input_focus.is_none() && self.view.trap.is_none() {
                        // Typed into the prompt: the key event and raw
                        // interceptors see each character first.
                        let rest: String = (t.chars().zip(keys))
                            .filter(|(_, k)| !input::native_key(self, *k))
                            .map(|(c, _)| c)
                            .collect();
                        if !rest.is_empty() {
                            keep.push(Event::Text(rest));
                        }
                    } else {
                        keep.push(e);
                    }
                }
                Event::Paste(_) | Event::Preedit(_) => keep.push(e),
                _ => {}
            }
        }
        fx.typed = keep;
    }

    /// ctrl+c (or ctrl+x with `cut`), key `k`: a focused panel's key, the
    /// text field's to copy its selection, a text node's selection copied,
    /// or else a key.
    fn clipboard_key(&mut self, fx: &mut Fx, cut: bool, k: Key) -> Clip {
        if self.view.trap.is_none()
            && self.view.input_focus.is_none()
            && self.focused_panel().is_some()
        {
            let id = self.focused_panel().unwrap_or_default();
            if input::panel_key(self, &id, k) {
                self.view.invalidate();
                return Clip::Done;
            }
        }
        let field_selects = match &self.view.input_focus {
            Some(id) => (self.view.inputs.get(id)).is_some_and(|b| b.anchor != b.cursor),
            None => {
                self.view.focused_node().is_none()
                    && self.prompt.anchor != self.prompt.cursor
                    && self.prompt_has_keys()
            }
        };
        if field_selects {
            return Clip::Field;
        }
        if !cut && let Some(t) = self.selected_text() {
            fx.out.copy = Some(t);
            return Clip::Done;
        }
        Clip::Key
    }

    /// Lua's `mouse` handlers see every press, drag and release first; one
    /// that returns true takes the rest of the gesture from the nodes. This
    /// is the frame's input before the nodes read it.
    pub(crate) fn route_pointer(&mut self, events: &mut Vec<Event>) {
        if self.view.mouse_held.is_none() {
            self.view.mouse_taken = MouseTaken::No;
        }
        for mut e in std::mem::take(events) {
            match &mut e {
                Event::PointerButton {
                    pos,
                    button,
                    pressed: true,
                    ..
                } => {
                    if self.pointer_event("down", button.name(), *pos) {
                        self.view.mouse_taken = MouseTaken::Whole;
                        continue;
                    }
                    self.view.mouse_seen = Some(*pos);
                }
                // The nodes end a press they saw where they last saw the
                // pointer.
                Event::PointerButton {
                    pos,
                    button,
                    pressed: false,
                    ..
                } => {
                    let taken = self.pointer_event("up", button.name(), *pos);
                    match self.view.mouse_taken {
                        MouseTaken::Whole => continue,
                        MouseTaken::No if !taken => self.view.mouse_seen = Some(*pos),
                        _ => {
                            self.view.mouse_taken = MouseTaken::Rest;
                            *pos = self.view.mouse_seen.unwrap_or(*pos);
                        }
                    }
                }
                Event::PointerMoved(pos) => {
                    if self.pointer_event(
                        if self.view.mouse_held.is_some() {
                            "drag"
                        } else {
                            "move"
                        },
                        self.view.mouse_held.map(|(b, _)| b).unwrap_or(""),
                        *pos,
                    ) && self.view.mouse_taken == MouseTaken::No
                    {
                        self.view.mouse_taken = MouseTaken::Rest;
                    }
                    if self.view.mouse_taken == MouseTaken::Rest {
                        continue;
                    }
                    self.view.mouse_seen = Some(*pos);
                }
                _ => {}
            }
            events.push(e);
        }
    }

    /// A pointer press, drag or release at `pos` for Lua's `mouse` event, in
    /// character cells as the TUI has it (and `px`, `py` in pixels). A drag
    /// is sent when it reaches another cell. Whether a handler took it.
    fn pointer_event(&mut self, action: &str, button: &'static str, pos: crate::backend::Pos) -> bool {
        let (Some((cw, ch)), Some(area)) = (self.view.cell, self.view.area) else {
            return false;
        };
        let cell = (
            ((pos.x - area.min.x) / cw).max(0.0) as u16,
            ((pos.y - area.min.y) / ch).max(0.0) as u16,
        );
        match action {
            "down" => self.view.mouse_held = Some((button, cell)),
            "up" | "move" => self.view.mouse_held = None,
            _ if self.view.mouse_held == Some((button, cell)) => return false,
            _ => self.view.mouse_held = Some((button, cell)),
        }
        let m = input::mouse_at(self, action, button, cell, Some((pos.x, pos.y)));
        if m.handled {
            self.view.invalidate();
        }
        m.handled
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

    /// What a built-in action does in the GUI. Editing the prompt is the
    /// text field's (see `NATIVE`); the chat follows the end.
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
            // the TUI (pageup, shift+up, ctrl+home; the wheel is the nodes').
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
