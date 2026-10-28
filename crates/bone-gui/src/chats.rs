//! Chats and messages for Lua, as the TUI has them: opening a session in a
//! chat (`bone.chat.open`, the sessions sidebar) and showing a chat with its
//! own draft, the message log, and running Lua
//! from the prompt (`/lua`, `bone.api.source`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bone_proto::methods::{SessionMessages, SessionMessagesParams, SessionMessagesResult};
use bone_ui::lua::{Host, args, err, fire, ret};
use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::nodeview::ChatMove;
use crate::{ChatBuffer, Gui};

/// These ops. `Err(a)` hands the arguments to the next dispatcher.
pub(crate) fn dispatch(
    gui: &mut Gui,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    Ok(match op {
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
        // Rows down (negative is up), "top" or "bottom": the chat list
        // moves by lines of text.
        "chat_scroll" => args::<Value>(lua, a).and_then(|by| {
            let to = match by {
                Value::Integer(n) => ChatMove::By(n as f32),
                Value::Number(n) => ChatMove::By(n as f32),
                Value::String(s) if s.to_str()?.as_ref() == "top" => ChatMove::Top,
                Value::String(s) if s.to_str()?.as_ref() == "bottom" => ChatMove::Bottom,
                Value::String(s) => {
                    return Err(err(format!(
                        "scroll by rows, \"top\" or \"bottom\", not {:?}",
                        s.to_string_lossy()
                    )));
                }
                other => {
                    return Err(err(format!(
                        "scroll by a number, not {}",
                        other.type_name()
                    )));
                }
            };
            gui.move_chat(to);
            ret(lua, ())
        }),
        "chat_scroll_to" => args::<(usize, Option<String>)>(lua, a).and_then(|(index, at)| {
            ret(
                lua,
                gui.chat_scroll_to(index, at.as_deref().unwrap_or("top")),
            )
        }),
        // Shown without adding to the log (for showing the log itself).
        "show_message" => args::<String>(lua, a).and_then(|text| {
            gui.status = (!text.is_empty()).then_some(crate::Status::new(text, false));
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
        _ => return Err(a),
    })
}

/// A chat opens with about its latest this-many messages; the rest load
/// when it is scrolled near its start.
pub(crate) const TAIL: usize = 60;

/// How long a chat is on screen before its earlier turns may load.
const EARLIER_AFTER: Duration = Duration::from_millis(400);

impl Gui {
    /// Session `session_id` in a chat, loaded the first time; shown when
    /// `show`. Returns the chat.
    pub fn open_session(&mut self, session_id: String, show: bool) -> usize {
        // A chat's number does too, for a chat without a session yet.
        if let Some(buf) = bone_ui::chat::find_chat(&self.chats, &session_id) {
            if show {
                self.show_chat(buf);
            }
            return buf;
        }
        // Reuse an untouched new chat rather than piling them up.
        let c = &self.chats[self.current];
        let untouched =
            c.session.is_none() && c.loading.is_none() && c.entries.is_empty() && !c.starting;
        let buf = if show && untouched && self.draft.is_empty() && self.images.is_empty() {
            self.current
        } else {
            self.chats.push(ChatBuffer::new(None));
            self.chats.len() - 1
        };
        // Not a blank new chat while it comes.
        self.chats[buf].loading = Some(session_id.clone());
        if show {
            self.show_chat(buf);
        }
        let params = SessionMessagesParams {
            session_id,
            tail: Some(TAIL),
        };
        self.request::<SessionMessages>(params, move |gui, r| match r {
            Ok(r) => {
                let session_id = r.info.session_id.clone();
                gui.apply_messages(buf, r);
                if buf == gui.current {
                    bone_ui::lua::processes::request(gui, session_id.clone());
                    let ev = serde_json::json!({ "session_id": session_id, "chat": buf });
                    fire(gui, "chat/shown", ev);
                }
            }
            Err(e) => {
                if let Some(c) = gui.chats.get_mut(buf) {
                    c.loading = None;
                    c.notice(format!("cannot load session: {e}"), true);
                }
            }
        });
        buf
    }

    /// Chat `buf`'s transcript from the core, in place of what it had.
    /// Turns it gains at the start (its earlier history) go above what is
    /// in view.
    pub(crate) fn apply_messages(&mut self, buf: usize, r: SessionMessagesResult) {
        let _load = crate::profile::Span::start("chat-load");
        let Some(c) = self.chats.get_mut(buf) else {
            return;
        };
        let id = r.info.session_id.as_str();
        if c.loading.as_deref().is_some_and(|l| l != id) {
            return; // the chat was given another session since
        }
        let same = c.session_id() == Some(id);
        let (omitted, len) = (c.omitted_turns, c.items().len());
        c.load(r.info, &r.messages, r.active_turn);
        c.set_tool_times(&r.tool_times);
        c.set_queue(r.queue, r.queue_paused);
        c.omitted_turns = r.omitted_turns;
        let added = c.items().len().saturating_sub(len);
        if same && r.omitted_turns < omitted && added > 0 {
            let v = &mut self.view;
            let id = chat_list_id(buf, self.current);
            if let Some(st) = v.lists.get_mut(&id) {
                let scroll = v.scroll.get_mut(&id);
                st.prepend(added, scroll);
            }
        }
        self.view.invalidate();
    }

    /// The chat on screen is scrolled near its start: load the turns it
    /// left out. Not at once on showing it, so arrowing through chats
    /// loads only their tails.
    pub(crate) fn load_earlier(&mut self) {
        let buf = self.current;
        let c = &self.chats[buf];
        let Some(session_id) = c.session_id().map(str::to_owned) else {
            return;
        };
        if c.omitted_turns == 0 || self.earlier.contains(&buf) {
            return;
        }
        let due = self.shown_at + EARLIER_AFTER;
        if Instant::now() < due {
            let wake = &mut self.view.wake;
            *wake = Some(wake.map_or(due, |w| w.min(due)));
            return;
        }
        self.earlier.insert(buf);
        let params = SessionMessagesParams {
            session_id,
            tail: None,
        };
        self.request::<SessionMessages>(params, move |gui, r| {
            gui.earlier.remove(&buf);
            if let Ok(r) = r {
                gui.apply_messages(buf, r);
            }
        });
    }

    /// Put chat `buf` on screen. Each chat keeps its own draft.
    pub fn show_chat(&mut self, buf: usize) {
        if buf != self.current {
            // `chat` is the chat on screen; another chat's measured rows,
            // rendered nodes and scroll offset are kept as `chat:<n>`, the
            // list that shows it elsewhere (`bone.nodes.chat{ chat = n }`).
            let (old, new) = (format!("chat:{}", self.current), format!("chat:{buf}"));
            let v = &mut self.view;
            for map in [&mut v.lists as &mut dyn Swap, &mut v.scroll] {
                map.swap_out("chat", &old);
                map.swap_out(&new, "chat");
            }
            let draft = (
                std::mem::take(&mut self.draft),
                std::mem::take(&mut self.images),
            );
            self.drafts.insert(self.current, draft);
            (self.draft, self.images) = self.drafts.remove(&buf).unwrap_or_default();
            // An image still being read was for the draft left behind.
            self.draft_epoch += 1;
            self.current = buf;
            self.shown_at = Instant::now();
            self.prompt_changed();
        }
        let session_id = self.chats[buf].session_id().map(str::to_owned);
        // The tray's shells are the chat on screen's.
        bone_ui::lua::processes::follow(self, session_id.clone());
        fire(
            self,
            "chat/shown",
            serde_json::json!({ "session_id": session_id, "chat": buf }),
        );
        self.changed();
    }

    /// Scroll the chat so item `index` (from 1) shows at the top, center
    /// or bottom; false when it is not in the list.
    pub fn chat_scroll_to(&mut self, index: usize, at: &str) -> bool {
        let Some((start, len, viewport)) = self.item_span(index) else {
            return false;
        };
        let top = match at {
            "bottom" => start + len - viewport,
            "center" => start + len / 2.0 - viewport / 2.0,
            _ => start,
        };
        self.move_chat(ChatMove::To(top.max(0.0)));
        true
    }

    /// `bone.chat.view()`: where the chat on screen is scrolled (pixels)
    /// and which items show.
    pub fn chat_view_json(&self) -> serde_json::Value {
        self.chat_view()
    }

    /// Load the UI with the library `/tui use` chose; a library that does
    /// not load is not kept as the choice.
    pub(crate) fn switch_library(&mut self) {
        if let Some(prev) = self.library_prev.take()
            && !self.reload_lua()
            && let Some(dir) = self.opts.config_dir.clone()
        {
            let _ = bone_lua::set_tui_library(&dir, &prev);
        }
        // `/gui use`: back to the one before (or to none chosen).
        if let Some(prev) = self.gui_library_prev.take()
            && !self.reload_lua()
            && let Some(dir) = self.opts.config_dir.clone()
        {
            let _ = match prev {
                Some(name) => bone_lua::set_gui_library(&dir, &name),
                None => std::fs::remove_file(dir.join("gui-library")).map_err(|e| e.to_string()),
            };
        }
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

/// The id of the list that shows chat `buf` while `current` is on screen.
pub(crate) fn chat_list_id(buf: usize, current: usize) -> String {
    if buf == current {
        "chat".into()
    } else {
        format!("chat:{buf}")
    }
}

/// Moving one id's entry to another in the view's maps by id.
trait Swap {
    fn swap_out(&mut self, from: &str, to: &str);
}

impl<T> Swap for std::collections::HashMap<String, T> {
    fn swap_out(&mut self, from: &str, to: &str) {
        match self.remove(from) {
            Some(v) => self.insert(to.to_owned(), v),
            None => self.remove(to),
        };
    }
}
