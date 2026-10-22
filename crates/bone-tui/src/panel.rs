//! Panels in the TUI: their state, sizing and ops are `bone-ui`'s
//! (`bone_ui::panel`, `bone_ui::lua`); here they meet the TUI's keys, mouse
//! and painting.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

pub use bone_ui::panel::*;

use crate::app::App;
use crate::keys::Key;
use bone_ui::lua::panels as shared;

use crate::ui::render_items;

impl App {
    pub fn panel(&self, id: &str) -> Option<&Panel> {
        shared::panel(&self.panels, id)
    }

    pub fn panel_mut(&mut self, id: &str) -> Option<&mut Panel> {
        self.panels.iter_mut().find(|p| p.id == id)
    }

    /// Panels in placement order: left, right, top, bottom; then `order`,
    /// then age.
    pub fn panels_in_order(&self) -> Vec<&Panel> {
        shared::in_order(&self.panels)
    }

    /// The panel with the keyboard, if any. A focused popup still comes first.
    pub fn focused_panel(&self) -> Option<&Panel> {
        shared::focused(&self.panels, &self.panel_focus)
    }

    /// Give a panel the keyboard, or (`None`) give it back to the prompt.
    pub fn focus_panel(&mut self, id: Option<&str>) -> Result<(), String> {
        shared::focus_panel(self, id)
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
        shared::at(&self.panels, x, y).map(|p| p.id.clone())
    }

    /// The panel at a screen position and the line of its content there
    /// (from 1; 0 on its title row).
    pub fn panel_hit(&self, at: (u16, u16)) -> Option<(String, usize)> {
        shared::hit(&self.panels, at.0, at.1)
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

    /// Measure an auto tree leaf (see `bone_ui::lua::panels::measure_panel`).
    pub(crate) fn measure_panel(&mut self, id: &str, width: u16, height: u16, rows: bool) -> u16 {
        shared::measure_panel(self, id, width, height, rows)
    }

    /// Dock the panels the layout does not place into `room`; what is left.
    pub(crate) fn allocate_panels(
        &mut self,
        room: Rect,
        full_height: bool,
        layout: &crate::ui::LayoutNode,
        placements: &mut Vec<PanelPlacement>,
    ) -> Rect {
        let room = bone_ui::layout::Rect::new(room.x, room.y, room.width, room.height);
        let r = shared::allocate_panels(self, room, full_height, layout, placements);
        Rect::new(r.x, r.y, r.width, r.height)
    }

    /// Publish every panel rectangle before any final render callback runs.
    pub(crate) fn publish_panels(&mut self, placements: &[PanelPlacement]) {
        shared::publish_panels(self, placements);
    }

    pub(crate) fn paint_panels(&mut self, frame: &mut Frame<'_>, placements: &[PanelPlacement]) {
        let rect = |r: bone_ui::layout::Rect| Rect::new(r.x, r.y, r.width, r.height);
        for placement in placements {
            let Some(f) = shared::panel_frame(self, placement) else {
                continue;
            };
            let area = rect(f.area);
            if let Some(items) = &f.title {
                frame.render_widget(
                    Paragraph::new(render_items(items, area.width as usize, &self.theme)),
                    Rect { height: 1, ..area },
                );
            }
            let body = rect(f.body);
            let shown: Vec<_> = (f.lines.iter())
                .map(|l| render_items(l, body.width as usize, &self.theme))
                .collect();
            frame.render_widget(Paragraph::new(shown), body);
            if let Some(x) = f.separator {
                let style = self.theme.hl("WinSeparator");
                for y in area.y..area.bottom() {
                    if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                        cell.set_symbol("│").set_style(style);
                    }
                }
            }
        }
    }
}
