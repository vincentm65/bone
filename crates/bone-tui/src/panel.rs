//! Panels: persistent areas that Lua owns, docked beside, above or below the
//! chat (`bone.ui.panel`). Unlike `bone.ui.win` overlays they take space from
//! the chat, keep their scroll position, can be hidden and shown again, and
//! can take the keyboard.
//!
//! Lua supplies the content (all of it; Rust scrolls it) and the keys. Rust
//! sizes and places panels, draws an optional title row, keeps the scroll
//! position, and routes keys and the mouse wheel. While a panel has the
//! keyboard its own keys run first, then the `panel` keymap context (or the
//! panel's own context); the scroll and `dismiss` actions act on the panel,
//! and unmapped text is ignored rather than typed into the prompt.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use serde_json::{Value as Json, json};

use crate::app::App;
use crate::keymap::Context;
use crate::keys::Key;
use crate::ui::{Item, render_items};

/// The chat keeps at least this much room; a panel that does not fit is not
/// drawn this frame.
pub const MIN_CHAT_COLS: u16 = 20;
pub const MIN_CHAT_ROWS: u16 = 3;
/// Default size of a side panel, in columns.
const SIDE_SIZE: u16 = 30;
/// Default cap of an `auto` panel: columns for sides, rows for top/bottom.
const SIDE_MAX: u16 = 40;
const ROW_MAX: u16 = 10;
/// Rows the wheel scrolls.
const WHEEL_ROWS: i64 = 3;

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
    fn sideways(self) -> bool {
        matches!(self, Dock::Left | Dock::Right)
    }

    /// Placement order: side panels take full-height columns first, then
    /// top and bottom panels split what is left of the chat column.
    fn rank(self) -> u8 {
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

#[derive(Clone)]
pub struct Panel {
    pub id: String,
    pub dock: Dock,
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

    fn size(&self) -> Size {
        self.size.unwrap_or(if self.dock.sideways() {
            Size::Cells(SIDE_SIZE)
        } else {
            Size::Auto
        })
    }

    fn max(&self) -> u16 {
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
        self.top = (self.top as i64 + by).clamp(0, max_top as i64) as usize;
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

    fn page(&self) -> i64 {
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
fn wanted(size: Size, max: u16, room: u16, content: impl FnOnce() -> u16) -> u16 {
    match size {
        Size::Cells(n) => n,
        Size::Fraction(f) => (room as f64 * f).round() as u16,
        Size::Auto => content().min(max),
    }
}

impl App {
    pub fn panel(&self, id: &str) -> Option<&Panel> {
        self.panels.iter().find(|p| p.id == id)
    }

    pub fn panel_mut(&mut self, id: &str) -> Option<&mut Panel> {
        self.panels.iter_mut().find(|p| p.id == id)
    }

    /// Panels in placement order: left, right, top, bottom; then `order`,
    /// then age.
    pub fn panels_in_order(&self) -> Vec<&Panel> {
        let mut v: Vec<&Panel> = self.panels.iter().collect();
        v.sort_by_key(|p| (p.dock.rank(), p.order, p.seq));
        v
    }

    /// The panel with the keyboard, if any. A focused popup still comes first.
    pub fn focused_panel(&self) -> Option<&Panel> {
        let id = self.panel_focus.as_deref()?;
        self.panel(id).filter(|p| !p.hidden && p.focusable)
    }

    pub fn panel_info(&self, p: &Panel) -> Json {
        p.info(self.panel_focus.as_deref() == Some(p.id.as_str()))
    }

    /// Give a panel the keyboard, or (`None`) give it back to the prompt.
    pub fn focus_panel(&mut self, id: Option<&str>) -> Result<(), String> {
        if let Some(id) = id {
            let p = self.panel(id).ok_or_else(|| format!("no panel {id}"))?;
            if p.hidden {
                return Err(format!("panel {id} is hidden"));
            }
            if !p.focusable {
                return Err(format!("panel {id} is not focusable"));
            }
        }
        let before = self.focused_panel().map(|p| p.id.clone());
        let after = id.map(str::to_owned);
        if before == after && self.panel_focus == after {
            return Ok(());
        }
        self.flush_pending();
        self.panel_focus = after;
        self.emit_focus_changed();
        self.dirty = true;
        Ok(())
    }

    /// Drop the focus if its panel is gone, hidden or no longer focusable.
    pub fn check_panel_focus(&mut self) {
        if self.panel_focus.is_some() && self.focused_panel().is_none() {
            self.panel_focus = None;
            self.emit_focus_changed();
        }
    }

    /// Move the keyboard to the next (or previous) focusable panel; the
    /// prompt is part of the cycle.
    pub fn cycle_focus(&mut self, forward: bool) {
        let mut ring: Vec<Option<String>> = vec![None];
        ring.extend(
            self.panels_in_order()
                .into_iter()
                .filter(|p| p.focusable && !p.hidden)
                .map(|p| Some(p.id.clone())),
        );
        let now = self.focused_panel().map(|p| p.id.clone());
        let i = ring.iter().position(|r| *r == now).unwrap_or(0);
        let n = ring.len();
        let next = ring[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }]
        .clone();
        let _ = self.focus_panel(next.as_deref());
    }

    /// Scroll and dismiss actions while a panel has the keyboard. Returns
    /// whether `b` applied to the panel.
    pub fn panel_builtin(&mut self, b: crate::keymap::Builtin) -> bool {
        use crate::keymap::Builtin::*;
        let Some(id) = self.focused_panel().map(|p| p.id.clone()) else {
            return false;
        };
        if b == Dismiss {
            let _ = self.focus_panel(None);
            return true;
        }
        let p = self.panel_mut(&id).expect("focused panel");
        match b {
            Up => p.scroll_by(-1),
            Down => p.scroll_by(1),
            ScrollUp => p.scroll_by(-WHEEL_ROWS),
            ScrollDown => p.scroll_by(WHEEL_ROWS),
            PageUp => p.scroll_by(-p.page()),
            PageDown => p.scroll_by(p.page()),
            ScrollTop => p.scroll_to(false),
            ScrollBottom => p.scroll_to(true),
            _ => return false,
        }
        self.dirty = true;
        true
    }

    /// The panel drawn at a screen position.
    fn panel_at(&self, (x, y): (u16, u16)) -> Option<String> {
        self.panels
            .iter()
            .find(|p| {
                p.area
                    .is_some_and(|a| x >= a.x && x < a.right() && y >= a.y && y < a.bottom())
            })
            .map(|p| p.id.clone())
    }

    /// The panel at a screen position and the line of its content there
    /// (from 1; 0 on its title row).
    pub fn panel_hit(&self, at: (u16, u16)) -> Option<(String, usize)> {
        let id = self.panel_at(at)?;
        let p = self.panel(&id)?;
        let area = p.area?;
        let row = (at.1 - area.y) as usize;
        let title = usize::from(p.title.is_some());
        let line = if row < title {
            0
        } else {
            p.top + row - title + 1
        };
        Some((id, line))
    }

    /// The wheel over a panel scrolls it. Returns whether it did.
    pub fn panel_wheel(&mut self, at: (u16, u16), up: bool) -> bool {
        if self.focused_popup().is_some() {
            return false;
        }
        let Some(id) = self.panel_at(at) else {
            return false;
        };
        if let Some(p) = self.panel_mut(&id) {
            p.scroll_by(if up { -WHEEL_ROWS } else { WHEEL_ROWS });
        }
        self.dirty = true;
        true
    }

    /// A click focuses the focusable panel under it, or the prompt.
    pub fn panel_click(&mut self, at: (u16, u16)) {
        if self.focused_popup().is_some() {
            return;
        }
        match self.panel_at(at) {
            Some(id) if self.panel(&id).is_some_and(|p| p.focusable) => {
                let _ = self.focus_panel(Some(&id));
            }
            Some(_) => {}
            None => {
                let in_prompt = self.placed.get(&crate::app::PROMPT_WIN).is_some_and(|p| {
                    p.area.height > 0 && at.1 >= p.area.y && at.1 < p.area.bottom()
                });
                if in_prompt {
                    let _ = self.focus_panel(None);
                }
            }
        }
    }

    /// A focused panel's own keys: `keys[name](panel)`, then
    /// `on_key(name, panel)`. Returns whether the key was used.
    pub fn panel_key(&mut self, id: &str, key: Key) -> bool {
        let Some((own, on_key)) = self.panel(id).map(|p| {
            (
                p.keys.iter().find(|(k, _)| *k == key).map(|(_, cb)| *cb),
                p.on_key,
            )
        }) else {
            return false;
        };
        if let Some(cb) = own {
            let id = id.to_owned();
            self.call_callback(cb, "panel key", |lua| crate::lua::panel_handle(lua, &id));
            self.dirty = true;
            return true;
        }
        if let Some(cb) = on_key {
            let name = crate::keys::format(&key);
            let id = id.to_owned();
            let r = self.call_callback_multi(cb, "panel on_key", |lua| {
                Ok(vec![
                    mlua::Value::String(lua.create_string(&name)?),
                    crate::lua::panel_handle(lua, &id)?,
                ]
                .into())
            });
            self.dirty = true;
            // nil or false passes the key on to the keymaps.
            return !matches!(
                r.as_ref().and_then(|v| v.front()),
                None | Some(mlua::Value::Nil | mlua::Value::Boolean(false))
            );
        }
        false
    }

    /// A panel's content: its function called with `ctx`, or its list.
    fn panel_content(&mut self, id: &str, content: u64, ctx: Json) -> Vec<Vec<Item>> {
        self.guarded(&format!("panel.{id}"), |lua| {
            let v: mlua::Value = lua
                .named_registry_value::<mlua::Table>("bone.callbacks")?
                .get(content)?;
            let v = match v {
                mlua::Value::Function(f) => f.call::<mlua::Value>(bone_lua::to_lua(lua, &ctx)?)?,
                v => v,
            };
            crate::ui::parse_lines(v)
        })
        .flatten()
        .unwrap_or_default()
    }

    /// Place and draw the visible panels inside `room` (the chat's area).
    /// Returns what is left for the chat.
    pub fn draw_panels(&mut self, frame: &mut Frame<'_>, room: Rect) -> Rect {
        let order: Vec<String> = self
            .panels_in_order()
            .into_iter()
            .map(|p| p.id.clone())
            .collect();
        let mut room = room;
        for id in order {
            // Content functions may open, close or change panels.
            let Some(p) = self.panel(&id).cloned() else {
                continue;
            };
            if p.hidden {
                if let Some(p) = self.panel_mut(&id) {
                    p.area = None;
                }
                continue;
            }
            let dock = p.dock;
            let side = dock.sideways();
            let title_h = u16::from(p.title.is_some());
            let (room_len, keep) = if side {
                (room.width, MIN_CHAT_COLS)
            } else {
                (room.height, MIN_CHAT_ROWS)
            };
            let focused = self.focused_panel().is_some_and(|f| f.id == id);
            let spinner = self.spinner.frame();
            let ctx = |w: u16, h: u16| {
                json!({
                    "id": id,
                    "spinner": spinner,
                    "dock": p.dock.name(),
                    "width": w,
                    "height": h,
                    "focused": focused,
                    "top": p.top,
                    "title": p.title,
                })
            };
            // `auto` measures the content in the most room it could get.
            let mut lines = None;
            let want = wanted(p.size(), p.max(), room_len, || {
                let (w, h) = if side {
                    (p.max().min(room.width), room.height.saturating_sub(title_h))
                } else {
                    (room.width, p.max().min(room.height).saturating_sub(title_h))
                };
                let l = self.panel_content(&id, p.content, ctx(w, h));
                let n = if side {
                    l.iter()
                        .map(|l| crate::ui::items_width(l))
                        .max()
                        .unwrap_or(0)
                        .min(u16::MAX as usize) as u16
                } else {
                    (l.len().min(u16::MAX as usize) as u16).saturating_add(title_h)
                };
                lines = Some(l);
                n
            });
            let Some((rect, rest)) = split(room, p.dock, want, p.min.max(1), keep) else {
                if let Some(p) = self.panel_mut(&id) {
                    p.area = None;
                }
                continue;
            };
            room = rest;
            let body = Rect {
                y: rect.y + title_h.min(rect.height),
                height: rect.height.saturating_sub(title_h),
                ..rect
            };
            let lines = match lines {
                Some(l) => l,
                None => self.panel_content(&id, p.content, ctx(body.width, body.height)),
            };
            let Some(p) = self.panel_mut(&id) else {
                continue;
            };
            let view = body.height as usize;
            let max_top = lines.len().saturating_sub(view);
            if p.follow && p.at_end {
                p.top = max_top;
            }
            p.top = p.top.min(max_top);
            p.rows = lines.len();
            p.view = body.height;
            p.area = Some(rect);
            let top = p.top;
            let title = p.title.clone();

            if let Some(t) = title {
                let hl = if focused {
                    "PanelTitleFocus"
                } else {
                    "PanelTitle"
                };
                let items = [Item::Text(t, hl.into()), Item::Fill(" ".into(), hl.into())];
                frame.render_widget(
                    Paragraph::new(render_items(&items, rect.width as usize, &self.theme)),
                    Rect { height: 1, ..rect },
                );
            }
            let shown: Vec<_> = lines
                .iter()
                .skip(top)
                .take(view)
                .map(|l| render_items(l, body.width as usize, &self.theme))
                .collect();
            frame.render_widget(Paragraph::new(shown), body);
            if side {
                let x = if dock == Dock::Left {
                    rect.right()
                } else {
                    rect.x - 1
                };
                let style = self.theme.hl("WinSeparator");
                for y in rect.y..rect.bottom() {
                    if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                        cell.set_symbol("│").set_style(style);
                    }
                }
            }
        }
        room
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
        assert_eq!(p.top, 6);
        assert!(!p.at_end, "only following panels stick to the end");
        p.follow = true;
        p.scroll_to(true);
        assert!(p.at_end);
        p.scroll_by(-1);
        assert!(!p.at_end);
        assert_eq!(p.top, 5);
    }
}
