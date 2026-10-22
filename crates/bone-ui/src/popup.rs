//! Lua windows (`bone.ui.win`, `bone.ui.popup`): overlays Lua draws every
//! cell of. This is their state and placement; frontends paint them.

use std::time::{Duration, Instant};

use crate::keys::Key;
use crate::layout::Rect;
use crate::model::Item;

#[derive(Debug, Clone)]
pub struct Popup {
    pub id: u64,
    /// Takes the keyboard while open.
    pub focus: bool,
    /// What `row`/`col` are relative to: "screen", "chat" or "prompt" (the
    /// space above the prompt, so row -1 sits right on it).
    pub anchor: String,
    /// Higher is drawn on top; ties go to the newer one.
    pub z: i32,
    /// Callback id of the lines: a function(ctx) or a fixed list.
    pub lines: u64,
    pub keys: Vec<(Key, u64)>,
    /// Called with the key name for keys `keys` does not name.
    pub on_key: Option<u64>,
    /// Size; `None` fits the lines.
    pub width: Option<u16>,
    pub height: Option<u16>,
    /// Position; `None` centers, negative counts from the bottom/right.
    pub row: Option<i32>,
    pub col: Option<i32>,
    /// Actual last-rendered bounds; absent until drawn or after an update.
    pub rect: Option<Rect>,
    pub opened: Instant,
    /// Keys within this long after opening are ignored.
    pub guard: Duration,
}

impl Popup {
    /// A new window with id `id`, before its spec is applied.
    pub fn new(id: u64) -> Self {
        Popup {
            id,
            focus: false,
            anchor: "screen".into(),
            z: 0,
            lines: 0,
            keys: Vec::new(),
            on_key: None,
            width: None,
            height: None,
            row: None,
            col: None,
            rect: None,
            opened: Instant::now(),
            guard: Duration::ZERO,
        }
    }

    /// What `panel/opened`, `panel/updated` and `panel/closed` carry.
    pub fn event(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "kind": "popup",
            "focus": self.focus,
            "anchor": self.anchor,
            "z": self.z,
            "width": self.width,
            "height": self.height,
            "row": self.row,
            "col": self.col,
        })
    }
}

/// Windows bottom first.
pub fn in_order(popups: &[Popup]) -> Vec<&Popup> {
    let mut v: Vec<&Popup> = popups.iter().collect();
    v.sort_by_key(|p| (p.z, p.id));
    v
}

/// The topmost window that takes the keyboard.
pub fn focused(popups: &[Popup]) -> Option<&Popup> {
    in_order(popups).into_iter().rev().find(|p| p.focus)
}

/// The topmost drawn window over cell `(x, y)`, focused or not.
pub fn hit(popups: &[Popup], x: u16, y: u16) -> Option<&Popup> {
    popups
        .iter()
        .filter(|p| p.rect.is_some_and(|r| r.contains(x, y)))
        .max_by_key(|p| (p.z, p.id))
}

/// The areas windows can be placed in.
#[derive(Debug, Clone, Copy)]
pub struct Anchors {
    pub screen: Rect,
    pub chat: Rect,
    /// Everything above the prompt.
    pub prompt: Rect,
}

/// Where window `p` goes in `anchors`, given lines of its content: `lines`
/// is called with the room it may take (width, height) and gives its lines
/// and their laid-out width. `None` when it gets no room.
pub fn place(
    p: &Popup,
    anchors: &Anchors,
    lines: impl FnOnce(u16, u16) -> (Vec<Vec<Item>>, u16),
) -> Option<(Rect, Vec<Vec<Item>>)> {
    let area = match p.anchor.as_str() {
        "chat" => anchors.chat,
        "prompt" => anchors.prompt,
        _ => anchors.screen,
    };
    if area.width == 0 || area.height == 0 {
        return None;
    }
    // Above the prompt, windows sit right on it unless told otherwise.
    let prompt = p.anchor == "prompt";
    let row = p.row.or(prompt.then_some(-1));
    let avail_w = p.width.unwrap_or(area.width).min(area.width);
    let avail_h = p.height.unwrap_or(area.height).min(area.height);
    let (lines, content_w) = lines(avail_w, avail_h);
    let w = p.width.unwrap_or(content_w);
    let h = p.height.unwrap_or(lines.len() as u16);
    let (w, h) = (w.min(area.width), h.min(area.height));
    if w == 0 || h == 0 {
        return None;
    }
    let at = |at: Option<i32>, size: u16, total: u16| -> u16 {
        let free = total - size;
        match at {
            None => free / 2,
            Some(n) if n < 0 => free.saturating_sub((-n - 1) as u16),
            Some(n) => (n as u16).min(free),
        }
    };
    let rect = Rect {
        x: area.x + at(p.col.or(prompt.then_some(0)), w, area.width),
        y: area.y + at(row, h, area.height),
        width: w,
        height: h,
    };
    Some((rect, lines))
}
