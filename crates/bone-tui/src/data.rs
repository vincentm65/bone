//! Read-only chat data for Lua (`bone.chat`) over the chats this TUI has
//! open; the data itself is built in `bone-ui`.

use serde_json::Value as Json;

use crate::app::App;
use crate::chat::ChatBuffer;
pub use bone_ui::data::{Dep, ItemFilter};

pub type Indexed<'a> = bone_ui::data::Indexed<'a, ratatui::text::Line<'static>>;

impl App {
    /// The chat a query is about: by session id, else the one on screen.
    /// `None` for a session no open chat has.
    pub fn data_chat(&self, session: Option<&str>) -> Option<&ChatBuffer> {
        match session {
            Some(id) => self.chat_by_session(id).and_then(|b| self.chat(b)),
            None => self.chat(self.rendering.unwrap_or(self.current)),
        }
    }

    /// Items as data, with their `turn`, filtered.
    pub fn chat_query(&self, f: &ItemFilter) -> Vec<Json> {
        self.indexed(f.session.as_deref())
            .map_or_else(Vec::new, |c| c.query(f))
    }

    /// The chat a query is about, indexed for [`Indexed::signature`].
    pub fn indexed(&self, session: Option<&str>) -> Option<Indexed<'_>> {
        self.data_chat(session).map(Indexed::new)
    }

    /// See [`Indexed::signature`]; 0 for a chat that isn't open.
    pub fn chat_signature(&self, f: &ItemFilter) -> u64 {
        self.indexed(f.session.as_deref())
            .map_or(0, |c| c.signature(f))
    }

    /// Changes whenever any open chat's data does.
    pub fn data_generation(&self) -> u64 {
        bone_ui::data::generation(&self.chats)
    }

    /// The turns of a chat: each user message and what followed it.
    pub fn chat_turns(&self, session: Option<&str>) -> Vec<Json> {
        self.data_chat(session)
            .map_or_else(Vec::new, bone_ui::data::turns)
    }

    /// One chat's session: the core's info plus what the TUI knows.
    pub fn chat_session(&self, session: Option<&str>) -> Json {
        let Some(chat) = self.data_chat(session) else {
            return Json::Null;
        };
        let buf = (self.chats.iter()).position(|c| std::ptr::eq(c, chat));
        bone_ui::data::session(chat, buf, buf == Some(self.current))
    }

    /// Every chat this TUI has open, the one on screen marked `current`.
    pub fn chat_sessions(&self) -> Vec<Json> {
        bone_ui::data::sessions(&self.chats, self.current)
    }
}
