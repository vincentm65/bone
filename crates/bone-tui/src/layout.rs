//! Per-area view state: scroll position for the chat and the prompt (from
//! `bone-ui`), and where each area was drawn last frame. The screen layout
//! itself is in `render.rs` (chat, regions from Lua, prompt, statusline).

use ratatui::layout::Rect;

pub use bone_ui::layout::{BufferId, Window, WindowId};

/// Where an area was drawn, from the last frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub area: Rect,
}
