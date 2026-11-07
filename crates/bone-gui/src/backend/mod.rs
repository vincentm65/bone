//! What the GUI needs of a window, kept apart from any renderer: a frame
//! takes [`input::FrameInput`] and gives a [`FrameOutput`] (a
//! [`draw::DrawList`] and what the window should do), laying text out
//! with the window's [`text::Shaper`]. The window itself is `native.rs`;
//! [`crate::Headless`] runs frames with no window.

pub mod draw;
pub mod geom;
pub mod gpui;
pub mod input;
pub mod interact;
pub mod text;

use std::time::Duration;

pub use draw::{DrawList, Picture, Shape};
pub use geom::{Pos, Rect, Vec2, pos, vec2};
pub use input::{Button, Event, FrameInput, Key, Modifiers};
pub use text::{Family, FontSet, FontsApplied, RunStyle, Shaped, Shaper};

/// The pointer's look over what it is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cursor {
    #[default]
    Default,
    None,
    PointingHand,
    Text,
    VerticalText,
    Grab,
    Grabbing,
    Move,
    AllScroll,
    NotAllowed,
    NoDrop,
    Wait,
    Progress,
    Crosshair,
    Cell,
    Help,
    Copy,
    Alias,
    ZoomIn,
    ZoomOut,
    ResizeHorizontal,
    ResizeVertical,
    ResizeNeSw,
    ResizeNwSe,
    ResizeColumn,
    ResizeRow,
}

/// A change to the window Lua asked for (`bone.gui.window`).
#[derive(Clone, Debug, PartialEq)]
pub enum WindowCmd {
    Size(Vec2),
    Fullscreen(bool),
    Maximized(bool),
    Minimized(bool),
}

/// What a frame leaves the window to do.
#[derive(Debug, Default)]
pub struct FrameOutput {
    pub draw: DrawList,
    pub cursor: Cursor,
    /// Draw another frame after this long, with no input (`None`: only
    /// on input or a message).
    pub repaint_after: Option<Duration>,
    /// Text to put on the clipboard.
    pub copy: Option<String>,
    /// Where text is being typed, for the input method's window; `None`
    /// when nothing takes text.
    pub ime: Option<Rect>,
    pub title: Option<String>,
    pub close: bool,
    pub window: Vec<WindowCmd>,
}

impl FrameOutput {
    /// Draw again within `after`.
    pub fn repaint_after(&mut self, after: Duration) {
        self.repaint_after = Some(self.repaint_after.map_or(after, |d| d.min(after)));
    }

    /// Draw again as soon as possible.
    pub fn repaint(&mut self) {
        self.repaint_after(Duration::ZERO);
    }
}

/// The renderer's side of a frame.
pub trait Backend {
    fn shaper(&self) -> &dyn Shaper;
    /// Use these fonts from now on (only called when they change).
    fn set_fonts(&mut self, fonts: &FontSet) -> FontsApplied;
}
