//! Panels: persistent areas that Lua owns, docked beside, above or below the
//! chat (`bone.ui.panel`). Unlike windows they take space from the chat,
//! keep their scroll position, can be hidden and shown again, and can take
//! the keyboard. This is their state, sizing and docking; the ops are in
//! `bone_ui::lua`, and frontends paint them.

use serde_json::{Value as Json, json};

use crate::keymap::Context;
use crate::keys::Key;
use crate::layout::Rect;

/// The chat keeps at least this much room; a panel that does not fit is not
/// drawn this frame.
pub const MIN_CHAT_COLS: u16 = 20;
pub const MIN_CHAT_ROWS: u16 = 3;
/// Default size of a side panel, in columns.
pub const SIDE_SIZE: u16 = 30;
/// Default cap of an `auto` panel: columns for sides, rows for top/bottom.
pub const SIDE_MAX: u16 = 40;
pub const ROW_MAX: u16 = 10;
/// Rows the wheel scrolls.
pub const WHEEL_ROWS: i64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dock {
    Left,
    Right,
    Top,
    Bottom,
}

impl Dock {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "left" => Dock::Left,
            "right" => Dock::Right,
            "top" => Dock::Top,
            "bottom" => Dock::Bottom,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Dock::Left => "left",
            Dock::Right => "right",
            Dock::Top => "top",
            Dock::Bottom => "bottom",
        }
    }

    /// Sized in columns (beside the chat) rather than rows.
    pub fn sideways(self) -> bool {
        matches!(self, Dock::Left | Dock::Right)
    }

    /// Placement order: side panels take full-height columns first, then
    /// top and bottom panels split what is left of the chat column.
    pub fn rank(self) -> u8 {
        match self {
            Dock::Left => 0,
            Dock::Right => 1,
            Dock::Top => 2,
            Dock::Bottom => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    /// Columns or rows.
    Cells(u16),
    /// A share of the room on that side, `0 < f < 1`.
    Fraction(f64),
    /// Fit the content, up to `max`.
    Auto,
}

/// Final frame geometry shared by panel painting and hit testing.
pub struct PanelPlacement {
    pub id: String,
    pub area: Rect,
    pub separator: Option<u16>,
}

#[derive(Clone)]
pub struct Panel {
    pub id: String,
    pub dock: Dock,
    /// Side panels may span the screen instead of just the chat viewport.
    pub full_height: bool,
    /// Side panels draw a `WinSeparator` bar in the column next to the chat;
    /// without it that column stays blank.
    pub separator: bool,
    /// `None`: 30 columns beside the chat, `auto` above or below it.
    pub size: Option<Size>,
    pub max: Option<u16>,
    /// Not drawn when it would get less than this.
    pub min: u16,
    /// Lower is placed first (nearer the screen edge); ties go to the older.
    pub order: i32,
    pub seq: u64,
    /// A fixed first row that does not scroll.
    pub title: Option<String>,
    /// Callback id of the content: a function(ctx) or a fixed list of lines.
    pub content: u64,
    pub keys: Vec<(Key, u64)>,
    pub on_key: Option<u64>,
    pub on_close: Option<u64>,
    pub focusable: bool,
    /// Keymap context while focused; `None` is `panel`.
    pub context: Option<Context>,
    pub hidden: bool,
    /// Keep the end in view as content grows (until scrolled away from it).
    pub follow: bool,
    /// First visible content row.
    pub top: usize,
    /// Pinned to the end: `follow` and not scrolled up.
    pub at_end: bool,
    /// From the last frame: content rows, visible body rows, and where it was
    /// drawn (`None` when it did not fit or is hidden).
    pub rows: usize,
    pub view: u16,
    pub area: Option<Rect>,
}

impl Panel {
    pub fn new(id: String, seq: u64) -> Self {
        Panel {
            id,
            dock: Dock::Right,
            full_height: false,
            separator: true,
            size: None,
            max: None,
            min: 1,
            order: 0,
            seq,
            title: None,
            content: 0,
            keys: Vec::new(),
            on_key: None,
            on_close: None,
            focusable: true,
            context: None,
            hidden: false,
            follow: false,
            top: 0,
            at_end: false,
            rows: 0,
            view: 0,
            area: None,
        }
    }

    pub fn size(&self) -> Size {
        self.size.unwrap_or(if self.dock.sideways() {
            Size::Cells(SIDE_SIZE)
        } else {
            Size::Auto
        })
    }

    pub fn max(&self) -> u16 {
        self.max.unwrap_or(if self.dock.sideways() {
            SIDE_MAX
        } else {
            ROW_MAX
        })
    }

    /// The panel as Lua sees it (`info`, and the `panel/*` events).
    pub fn info(&self, focused: bool) -> Json {
        let size = match self.size() {
            Size::Cells(n) => json!(n),
            Size::Fraction(f) => json!(f),
            Size::Auto => json!("auto"),
        };
        let mut v = json!({
            "id": self.id,
            "kind": "panel",
            "dock": self.dock.name(),
            "size": size,
            "order": self.order,
            "title": self.title,
            "hidden": self.hidden,
            "focusable": self.focusable,
            "focus": focused,
            "follow": self.follow,
            "top": self.top,
            "rows": self.rows,
            "visible": self.area.is_some(),
        });
        if let Some(a) = self.area {
            v["width"] = json!(a.width);
            v["height"] = json!(a.height);
            v["row"] = json!(a.y);
            v["col"] = json!(a.x);
        }
        v
    }

    /// Scroll the content; clamped on the next draw.
    pub fn scroll_by(&mut self, by: i64) {
        let max_top = self.rows.saturating_sub(self.view as usize);
        self.top = (self.top as i64).saturating_add(by).max(0) as usize;
        self.at_end = self.follow && self.top >= max_top;
    }

    pub fn scroll_to(&mut self, end: bool) {
        if end {
            self.top = self.rows.saturating_sub(self.view as usize);
            self.at_end = self.follow;
        } else {
            self.top = 0;
            self.at_end = self.follow && self.rows <= self.view as usize;
        }
    }

    pub fn page(&self) -> i64 {
        (self.view as i64 - 1).max(1)
    }
}

/// Split `room` for a panel docked at `dock` that wants `want` cells, leaving
/// at least `keep` for the rest. Side panels also take a one-column
/// separator. Returns the panel's rect and what is left, or `None` when
/// fewer than `min` cells fit.
pub fn split(room: Rect, dock: Dock, want: u16, min: u16, keep: u16) -> Option<(Rect, Rect)> {
    let sep = u16::from(dock.sideways());
    let total = if dock.sideways() {
        room.width
    } else {
        room.height
    };
    let n = want.min(total.saturating_sub(keep + sep));
    if n == 0 || n < min {
        return None;
    }
    Some(match dock {
        Dock::Left => (
            Rect { width: n, ..room },
            Rect {
                x: room.x + n + sep,
                width: room.width - n - sep,
                ..room
            },
        ),
        Dock::Right => (
            Rect {
                x: room.right() - n,
                width: n,
                ..room
            },
            Rect {
                width: room.width - n - sep,
                ..room
            },
        ),
        Dock::Top => (
            Rect { height: n, ..room },
            Rect {
                y: room.y + n,
                height: room.height - n,
                ..room
            },
        ),
        Dock::Bottom => (
            Rect {
                y: room.bottom() - n,
                height: n,
                ..room
            },
            Rect {
                height: room.height - n,
                ..room
            },
        ),
    })
}

/// How many cells a panel wants, given the room on its side and (for
/// `auto`) its content.
pub fn wanted(size: Size, max: u16, room: u16, content: impl FnOnce() -> u16) -> u16 {
    match size {
        Size::Cells(n) => n,
        Size::Fraction(f) => (room as f64 * f).round() as u16,
        Size::Auto => content().min(max),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn split_docks_and_keeps_room_for_the_chat() {
        let room = r(0, 0, 100, 20);
        assert_eq!(
            split(room, Dock::Left, 30, 1, 20),
            Some((r(0, 0, 30, 20), r(31, 0, 69, 20)))
        );
        assert_eq!(
            split(room, Dock::Right, 30, 1, 20),
            Some((r(70, 0, 30, 20), r(0, 0, 69, 20)))
        );
        assert_eq!(
            split(room, Dock::Top, 5, 1, 3),
            Some((r(0, 0, 100, 5), r(0, 5, 100, 15)))
        );
        assert_eq!(
            split(room, Dock::Bottom, 5, 1, 3),
            Some((r(0, 15, 100, 5), r(0, 0, 100, 15)))
        );
        // Shrinks to leave the chat its minimum, and gives up below `min`.
        assert_eq!(
            split(room, Dock::Right, 90, 1, 20).map(|s| s.0.width),
            Some(79)
        );
        assert_eq!(split(r(0, 0, 25, 20), Dock::Left, 30, 10, 20), None);
        assert_eq!(split(room, Dock::Top, 0, 1, 3), None);
    }

    #[test]
    fn sizes() {
        assert_eq!(wanted(Size::Cells(7), 10, 50, || 0), 7);
        assert_eq!(wanted(Size::Fraction(0.25), 10, 80, || 0), 20);
        assert_eq!(wanted(Size::Auto, 10, 80, || 4), 4);
        assert_eq!(wanted(Size::Auto, 10, 80, || 40), 10);
    }

    #[test]
    fn scrolling_clamps_and_follows() {
        let mut p = Panel::new("p".into(), 1);
        p.rows = 10;
        p.view = 4;
        p.scroll_by(-3);
        assert_eq!(p.top, 0);
        p.scroll_by(100);
        assert_eq!(
            p.top, 100,
            "upper bound is clamped against fresh content on draw"
        );
        assert!(!p.at_end, "only following panels stick to the end");
        p.follow = true;
        p.scroll_to(true);
        assert!(p.at_end);
        p.scroll_by(-1);
        assert!(!p.at_end);
        assert_eq!(p.top, 5);
    }
}
