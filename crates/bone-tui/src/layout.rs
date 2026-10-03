//! Per-area view state: scroll position for the chat and the prompt, and
//! where each area was drawn last frame. The screen layout itself is in
//! `render.rs` (chat, regions from Lua, prompt, statusline).

use ratatui::layout::Rect;

pub type WindowId = usize;
pub type BufferId = usize;

#[derive(Debug, Clone)]
pub struct Window {
    /// First visible row.
    pub top: usize,
    /// Keep the view pinned to the end as content grows.
    pub follow: bool,
}

/// Where an area was drawn, from the last frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub area: Rect,
}
