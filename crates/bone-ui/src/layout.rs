//! Per-area view state: scroll position for the chat and the prompt.

pub type WindowId = usize;
pub type BufferId = usize;

#[derive(Debug, Clone)]
pub struct Window {
    /// First visible row.
    pub top: usize,
    /// Keep the view pinned to the end as content grows.
    pub follow: bool,
}

impl Default for Window {
    fn default() -> Self {
        Window {
            top: 0,
            follow: true,
        }
    }
}
