//! The prompt for Lua, as the TUI has it: the cursor and selection (kept in
//! step with egui's text box), editing it from Lua (`bone.prompt.insert`,
//! `set_range`, `select`, …), and recalling sent messages with up and down.

use bone_proto::types::ImageAttachment;
use bone_ui::editor::{Pos, TextBuffer};
use bone_ui::lua::{args, err, ret};
use eframe::egui;
use mlua::{Lua, MultiValue, Table, Value};

use crate::Gui;

/// The id of the prompt's text box, whose cursor egui keeps.
pub(crate) fn edit_id() -> egui::Id {
    egui::Id::new("bone prompt")
}

/// What is kept of the prompt besides its text: the cursor and selection,
/// and the history of sent messages.
#[derive(Default)]
pub(crate) struct PromptState {
    /// Where the selection starts and the cursor is, in chars from the
    /// start (equal: no selection).
    pub anchor: usize,
    pub cursor: usize,
    /// Lua moved them: egui's text box takes them on the next frame.
    pub moved: bool,
    /// Long pastes, as (placeholder in the text, what was pasted).
    pub pastes: Vec<(String, String)>,
    history: Vec<(String, Vec<ImageAttachment>)>,
    /// The entry recalled, and the draft that was there before.
    history_pos: Option<usize>,
    saved: Option<(String, Vec<ImageAttachment>)>,
}

/// These ops; `None` for an op that is not one of them.
pub(crate) fn dispatch(
    gui: &mut Gui,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Option<mlua::Result<MultiValue>> {
    Some(match op {
        "prompt_edit" => args::<(String, Value, Value, Option<String>)>(lua, a)
            .and_then(|(op, x, y, text)| edit(gui, &op, x, y, text).and_then(|()| ret(lua, ()))),
        "prompt_get_range" => args::<(Value, Value)>(lua, a).and_then(|(from, to)| {
            let t = gui.buffer();
            ret(lua, t.range_text(pos_arg(&t, from)?, pos_arg(&t, to)?))
        }),
        "prompt_offset" => args::<Value>(lua, a).and_then(|pos| {
            let t = gui.buffer();
            ret(lua, t.offset(pos_arg(&t, pos)?))
        }),
        "prompt_position" => args::<Value>(lua, a).and_then(|pos| {
            let t = gui.buffer();
            ret(lua, pos_out(lua, pos_arg(&t, pos)?)?)
        }),
        _ => return None,
    })
}

/// Every prompt change from Lua; then the event.
fn edit(gui: &mut Gui, op: &str, x: Value, y: Value, text: Option<String>) -> mlua::Result<()> {
    let mut t = gui.buffer();
    match op {
        "cursor" => {
            let pos = pos_arg(&t, x)?;
            t.clear_selection();
            t.set_cursor(pos);
        }
        "insert" => {
            let text = text.ok_or_else(|| err("insert needs text"))?;
            t.delete_selection();
            let at = t.cursor();
            t.replace_range(at, at, &text);
        }
        "range" => {
            let (from, to) = (pos_arg(&t, x)?, pos_arg(&t, y)?);
            let text = text.ok_or_else(|| err("set_range needs text"))?;
            t.replace_range(from, to, &text);
        }
        "select" => {
            let from = pos_arg(&t, x)?;
            let to = match y {
                Value::Nil => t.cursor(),
                y => pos_arg(&t, y)?,
            };
            t.select(from, to);
        }
        "unselect" => t.clear_selection(),
        other => return Err(err(format!("unknown prompt edit {other}"))),
    }
    gui.set_buffer(&t);
    gui.prompt_changed();
    Ok(())
}

/// A position from Lua: `{ row, col }` or an offset in chars.
fn pos_arg(t: &TextBuffer, v: Value) -> mlua::Result<Pos> {
    let n = |v: Value, what: &str| -> mlua::Result<usize> {
        match v {
            Value::Integer(n) if n >= 0 => Ok(n as usize),
            Value::Number(n) if n >= 0.0 && n.fract() == 0.0 => Ok(n as usize),
            _ => Err(err(format!("a prompt {what} is a non-negative integer"))),
        }
    };
    match v {
        Value::Table(p) => Ok(t.clamp((n(p.get("row")?, "row")?, n(p.get("col")?, "col")?))),
        v @ (Value::Integer(_) | Value::Number(_)) => Ok(t.position(n(v, "offset")?)),
        other => Err(err(format!(
            "a prompt position is {{ row, col }} or an offset, not {}",
            other.type_name()
        ))),
    }
}

pub(crate) fn pos_out(lua: &Lua, (row, col): Pos) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("row", row)?;
    t.set("col", col)?;
    Ok(t)
}

impl Gui {
    /// The prompt as an editor buffer: its text, cursor and selection.
    pub(crate) fn buffer(&self) -> TextBuffer {
        let mut t = TextBuffer::default();
        t.set_text(&self.draft);
        let p = &self.prompt;
        let (anchor, cursor) = (t.position(p.anchor), t.position(p.cursor));
        if anchor == cursor {
            t.set_cursor(cursor);
        } else {
            t.select(anchor, cursor);
        }
        t
    }

    /// Take the text, cursor and selection of `t`.
    pub(crate) fn set_buffer(&mut self, t: &TextBuffer) {
        self.draft = t.text();
        let cursor = t.offset(t.cursor());
        let anchor = match t.selection() {
            Some((a, b)) if t.offset(a) == cursor => t.offset(b),
            Some((a, _)) => t.offset(a),
            None => cursor,
        };
        self.prompt.anchor = anchor;
        self.prompt.cursor = cursor;
        self.prompt.moved = true;
    }

    /// Put the cursor at the end of the text (after Lua replaced it).
    pub(crate) fn cursor_to_end(&mut self) {
        let end = self.draft.chars().count();
        (self.prompt.anchor, self.prompt.cursor, self.prompt.moved) = (end, end, true);
    }

    /// Before the text box draws: the cursor Lua set. After: the one the
    /// user moved.
    pub(crate) fn sync_cursor(&mut self, ctx: &egui::Context, before: bool) {
        let id = edit_id();
        let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) else {
            return;
        };
        if before {
            if !self.prompt.moved {
                return;
            }
            self.prompt.moved = false;
            let range = egui::text::CCursorRange::two(
                egui::text::CCursor::new(self.prompt.anchor),
                egui::text::CCursor::new(self.prompt.cursor),
            );
            state.cursor.set_char_range(Some(range));
            state.store(ctx, id);
        } else if let Some(r) = state.cursor.char_range() {
            self.prompt.anchor = r.secondary.index.into();
            self.prompt.cursor = r.primary.index.into();
        }
    }

    /// The cursor and selection as `prompt/changed` and `bone.prompt.info`
    /// give them: `{ row, col }` positions.
    pub(crate) fn cursor_json(&self) -> (serde_json::Value, serde_json::Value) {
        let t = self.buffer();
        let pos = |(row, col): Pos| serde_json::json!({ "row": row, "col": col });
        let selection = (t.selection())
            .map(|(a, b)| serde_json::json!({ "start": pos(a), "end": pos(b) }))
            .unwrap_or(serde_json::Value::Null);
        (pos(t.cursor()), selection)
    }

    /// Remember a sent message for up and down.
    pub(crate) fn remember(&mut self, text: &str, images: &[ImageAttachment]) {
        let p = &mut self.prompt;
        if (p.history.last()).is_none_or(|(t, i)| t != text || i.as_slice() != images) {
            p.history.push((text.to_owned(), images.to_vec()));
        }
        p.history_pos = None;
        p.saved = None;
    }

    /// Up or down on the prompt: the line above or below, else the message
    /// sent before or after. Returns whether it did anything.
    pub(crate) fn vertical(&mut self, up: bool) -> bool {
        let mut t = self.buffer();
        let moved = if up { t.up() } else { t.down() };
        if moved {
            self.set_buffer(&t);
            return true;
        }
        let p = &mut self.prompt;
        let len = p.history.len();
        if len == 0 {
            return false;
        }
        let pos = match (p.history_pos, up) {
            (None, true) => Some(len - 1),
            (None, false) => return false,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i + 1 < len => Some(i + 1),
            (Some(_), false) => None,
        };
        if p.history_pos.is_none() && pos.is_some() {
            p.saved = Some((self.draft.clone(), self.images.clone()));
        }
        p.history_pos = pos;
        let (text, images) = match pos {
            Some(i) => p.history[i].clone(),
            None => p.saved.take().unwrap_or_default(),
        };
        (self.draft, self.images) = (text, images);
        self.draft_epoch += 1;
        self.cursor_to_end();
        self.prompt_changed();
        true
    }

    /// Text pasted into the window: into the prompt, a long one as a
    /// placeholder expanded when it is sent, as in the TUI.
    pub(crate) fn paste(&mut self, text: &str) {
        bone_ui::lua::input::flush_pending(self);
        let context = bone_ui::lua::Host::context(self).name().to_owned();
        let ev = serde_json::json!({ "text": text, "context": context });
        bone_ui::lua::fire(self, "paste", ev);
        if !self.prompt_has_keys() {
            return;
        }
        self.prompt.history_pos = None;
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let insert = if text.chars().count() > bone_ui::composer::PASTE_PLACEHOLDER_THRESHOLD {
            let token = bone_ui::composer::paste_token(self.prompt.pastes.len() + 1, &text);
            self.prompt.pastes.push((token.clone(), text));
            token
        } else {
            text
        };
        let mut t = self.buffer();
        t.delete_selection();
        let at = t.cursor();
        t.replace_range(at, at, &insert);
        self.set_buffer(&t);
        self.prompt_changed();
    }

    /// The prompt on screen has the keyboard.
    pub(crate) fn prompt_has_keys(&self) -> bool {
        bone_ui::lua::Host::prompt_focused(self, self.current)
    }

    /// The prompt's text with long pastes in full.
    pub(crate) fn expanded(&self) -> String {
        let mut text = self.draft.clone();
        for (token, content) in &self.prompt.pastes {
            text = text.replace(token, content);
        }
        text
    }

    /// Backspace; a paste's placeholder goes in one.
    pub(crate) fn backspace(&mut self) {
        let mut t = self.buffer();
        let (row, col) = t.cursor();
        let before: String = t.lines()[row].chars().take(col).collect();
        let token = self.prompt.pastes.iter().find(|p| before.ends_with(&p.0));
        for _ in 0..token.map_or(1, |p| p.0.chars().count()) {
            t.backspace();
        }
        self.set_buffer(&t);
        self.prompt_changed();
    }

    /// Whether up or down is recalling history now.
    pub(crate) fn browsing_history(&self) -> bool {
        self.prompt.history_pos.is_some()
    }
}
