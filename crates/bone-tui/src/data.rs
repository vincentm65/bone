//! Read-only chat data for Lua (`bone.chat`) over the chats this TUI has
//! open; the data itself is built in `bone-ui`.

use crate::app::App;
use crate::chat::ChatBuffer;
pub use bone_ui::data::Dep;

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

    /// The chat a query is about, indexed for [`Indexed::signature`].
    pub fn indexed(&self, session: Option<&str>) -> Option<Indexed<'_>> {
        self.data_chat(session).map(Indexed::new)
    }

    /// Changes whenever any open chat's data does.
    pub fn data_generation(&self) -> u64 {
        bone_ui::data::generation(&self.chats)
    }
}
