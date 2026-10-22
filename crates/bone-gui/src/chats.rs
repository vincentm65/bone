//! Chats and messages for Lua, as the TUI has them: opening a session in a
//! chat (`bone.chat.open`, the sessions sidebar) and showing a chat with its
//! own draft, the chat item under a cell, the message log, and running Lua
//! from the prompt (`/lua`, `bone.api.source`).

use std::path::PathBuf;

use bone_proto::methods::{SessionMessages, SessionRef};
use bone_ui::lua::{Host, args, fire, ret};
use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::{ChatBuffer, Gui};

/// These ops; `None` for an op that is not one of them.
pub(crate) fn dispatch(
    gui: &mut Gui,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Option<mlua::Result<MultiValue>> {
    Some(match op {
        // No session: a new chat (for a window to show).
        "open_session" => args::<(Option<String>, Option<bool>)>(lua, a).and_then(|(id, show)| {
            let buf = match id {
                Some(id) => gui.open_session(id, show.unwrap_or(true)),
                None => {
                    gui.chats.push(ChatBuffer::new(None));
                    gui.chats.len() - 1
                }
            };
            ret(lua, buf)
        }),
        "chat_at" => args::<(u16, u16)>(lua, a).and_then(|(x, y)| match gui.chat_at(x, y) {
            Some((index, line)) => {
                let t = lua.create_table()?;
                t.set("index", index)?;
                t.set("line", line)?;
                ret(lua, t)
            }
            None => ret(lua, Value::Nil),
        }),
        // Shown without adding to the log (for showing the log itself).
        "show_message" => args::<String>(lua, a).and_then(|text| {
            gui.status = (!text.is_empty()).then_some(crate::Status { text, error: false });
            gui.changed();
            ret(lua, ())
        }),
        "log_tail" => args::<usize>(lua, a).and_then(|n| {
            let start = gui.lua.log.len().saturating_sub(n);
            ret(lua, gui.lua.log[start..].to_vec())
        }),
        "exec_lua" => args::<String>(lua, a).and_then(|code| {
            gui.exec_lua(&code);
            ret(lua, ())
        }),
        "source_file" => args::<String>(lua, a).and_then(|path| {
            gui.source(&path);
            ret(lua, ())
        }),
        _ => return None,
    })
}

impl Gui {
    /// Session `session_id` in a chat, loaded the first time; shown when
    /// `show`. Returns the chat.
    pub fn open_session(&mut self, session_id: String, show: bool) -> usize {
        if let Some(buf) = self.chat_of(&session_id) {
            if show {
                self.show_chat(buf);
            }
            return buf;
        }
        // Reuse an untouched new chat rather than piling them up.
        let c = &self.chats[self.current];
        let untouched = c.session.is_none() && c.entries.is_empty() && !c.starting;
        let buf = if show && untouched && self.draft.is_empty() && self.images.is_empty() {
            self.current
        } else {
            self.chats.push(ChatBuffer::new(None));
            self.chats.len() - 1
        };
        if show {
            self.show_chat(buf);
        }
        self.request::<SessionMessages>(SessionRef { session_id }, move |gui, r| {
            let Some(c) = gui.chats.get_mut(buf) else {
                return;
            };
            match r {
                Ok(r) => {
                    let session_id = r.info.session_id.clone();
                    c.load(r.info, &r.messages, r.active_turn);
                    c.set_queue(r.queue, r.queue_paused);
                    if buf == gui.current {
                        let ev = serde_json::json!({ "session_id": session_id, "chat": buf });
                        fire(gui, "chat/shown", ev);
                    }
                }
                Err(e) => c.notice(format!("cannot load session: {e}"), true),
            }
        });
        buf
    }

    /// Put chat `buf` on screen. Each chat keeps its own draft.
    pub fn show_chat(&mut self, buf: usize) {
        if buf != self.current {
            let draft = (
                std::mem::take(&mut self.draft),
                std::mem::take(&mut self.images),
            );
            self.drafts.insert(self.current, draft);
            (self.draft, self.images) = self.drafts.remove(&buf).unwrap_or_default();
            // An image still being read was for the draft left behind.
            self.draft_epoch += 1;
            self.current = buf;
            self.prompt_changed();
        }
        let session_id = self.chats[buf].session_id().map(str::to_owned);
        fire(
            self,
            "chat/shown",
            serde_json::json!({ "session_id": session_id, "chat": buf }),
        );
        self.changed();
    }

    /// Run Lua from the prompt: `=expr` shows its value.
    pub fn exec_lua(&mut self, code: &str) {
        let (code, show) = match code.strip_prefix('=') {
            Some(expr) => (format!("return {expr}"), true),
            None => (code.to_owned(), false),
        };
        let r = self.with_api(|lua| {
            let v: Value = lua.load(code).set_name("=/lua").eval()?;
            if !show {
                return Ok(None);
            }
            let inspect: Function = lua.globals().get::<Table>("bone")?.get("inspect")?;
            Ok(Some(inspect.call::<String>(v)?))
        });
        match r {
            Ok(Some(s)) => Host::info(self, s),
            Ok(None) => {}
            Err(e) => self.lua_error("lua", &e),
        }
    }

    /// Run a Lua file.
    pub fn source(&mut self, path: &str) {
        let path = PathBuf::from(path);
        match self.with_api(|lua| bone_lua::run_file(lua, &path)) {
            Ok(true) => {}
            Ok(false) => self.error(format!("no such file: {}", path.display())),
            Err(e) => self.lua_error(&path.display().to_string(), &e),
        }
    }
}
