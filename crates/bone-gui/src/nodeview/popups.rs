//! Lua's windows (`bone.ui.popup`, `win`) in the node view. A window of
//! `lines` is sized in monospace cells and placed in pixels. A window with
//! a node callback is given its anchor instead, and Lua sizes the card.

use bone_ui::lua::drop_callback;
use eframe::egui::{self, Color32, FontId};
use mlua::{Lua, Value};

use crate::Gui;

/// A character cell of the monospace font: its advance and the height a
/// line of it is laid out at.
pub(crate) fn cell_size(ctx: &egui::Context) -> (f32, f32) {
    let font = FontId::monospace(egui::TextStyle::Monospace.resolve(&ctx.global_style()).size);
    let (col, row) = ctx.fonts_mut(|f| {
        let line = f.layout_no_wrap("M".into(), font.clone(), Color32::WHITE);
        (f.glyph_width(&font, 'M'), line.size().y)
    });
    (col.max(1.0), row.max(1.0))
}

impl Gui {
    /// The character cell, the unit of Lua's windows (`bone.ui.popup`, `win`:
    /// sizes and offsets in columns and rows) and of the `resize` event;
    /// and where the window's anchors (all of it, the chat, the prompt)
    /// are, in pixels.
    pub(super) fn window_units(
        &mut self,
        ui: &egui::Ui,
        area: egui::Rect,
        leaves: Vec<(String, egui::Rect)>,
    ) {
        let (col, row) = cell_size(ui.ctx());
        let first = self.view.cell.is_none();
        self.view.cell = Some((col, row));
        let cols = (area.width() / col).floor().max(1.0) as u16;
        let rows = (area.height() / row).floor().max(1.0) as u16;
        self.view.screen_cols = cols;
        // `resize` is the shared event, in columns and rows.
        if self.view.screen_size != (cols, rows) {
            self.view.screen_size = (cols, rows);
            let size = serde_json::json!({ "width": cols, "height": rows });
            self.fire_event("resize", size);
            // The event may update state used by the node tree.  It fires
            // after this frame's tree was built, so ensure that state is
            // reflected on the next frame.
            self.view.invalidate();
            ui.ctx().request_repaint();
        }
        // Panel callbacks use their last allocated size. Correct the initial
        // estimate (and resizes) on the next build; positions alone do not
        // affect their content. A zero-basis growing panel keeps this stable.
        fn panel_sizes(
            leaves: &[(String, egui::Rect)],
        ) -> impl Iterator<Item = (&str, egui::Vec2)> {
            leaves
                .iter()
                .filter(|(id, _)| id.starts_with("panel:"))
                .map(|(id, rect)| (id.as_str(), rect.size()))
        }
        let panels_changed = !panel_sizes(&self.view.leaves).eq(panel_sizes(&leaves));
        self.view.area = Some(area);
        self.view.leaves = leaves;
        // Windows are placed by the next build, which asks for them.
        if first || panels_changed {
            self.view.invalidate();
            ui.ctx().request_repaint();
        }
    }

    /// Place every window (`bone.ui.popup` and `win`). A window of `lines`
    /// follows the TUI: its size in cells of the monospace font, centered in
    /// its anchor or `row`/`col` cells from its edges, returned in pixels. A
    /// window with a node callback is not placed in cells: `x`/`y`/`w`/`h`
    /// are the anchor in pixels and `bounds` is true. Lua draws both
    /// (`bone.nodes.popups`).
    pub(crate) fn popups_for_lua(&mut self, lua: &Lua) -> mlua::Result<mlua::Table> {
        self.forget_window_nodes(lua)?;
        let out = lua.create_table()?;
        if self.lua.popups.is_empty() {
            return Ok(out);
        }
        // Before the first frame is drawn the cell is measured here.
        let Some((col, row)) = self.view.cell.or_else(|| self.repaint.get().map(cell_size)) else {
            return Ok(out);
        };
        let vs = &self.view;
        let screen = vs.area.unwrap_or_else(|| {
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(vs.size.0, vs.size.1))
        });
        let leaf = |name: &str| {
            (vs.leaves.iter())
                .find(|(n, r)| n == name && r.width() > 0.0)
                .map(|(_, r)| *r)
        };
        let chat = leaf("chat").unwrap_or(screen);
        let prompt_top = leaf("prompt").map_or(screen.max.y, |r| r.min.y);
        // Everything above the prompt, the window's width.
        let above_prompt = egui::Rect::from_min_max(
            egui::pos2(screen.min.x, chat.min.y),
            egui::pos2(screen.max.x, prompt_top.max(chat.min.y)),
        );
        let order: Vec<bone_ui::popup::Popup> = bone_ui::popup::in_order(&self.lua.popups)
            .into_iter()
            .cloned()
            .collect();
        for p in order {
            let prompt = p.anchor == "prompt";
            let area = match p.anchor.as_str() {
                "chat" => chat,
                "prompt" => above_prompt,
                _ => screen,
            };
            // A node window is not measured in cells. The anchor is the room
            // the card may occupy; Lua sizes and centers the card inside it.
            if let Some(cb) = self
                .view
                .bound
                .window_nodes
                .get(&format!("popup:{}", p.id))
                .copied()
            {
                if let Some(q) = self.lua.popups.iter_mut().find(|q| q.id == p.id) {
                    // The anchor, in cells, for what reads the cell rect back.
                    q.rect = Some(bone_ui::layout::Rect::new(
                        ((area.min.x - screen.min.x) / col).round() as u16,
                        ((area.min.y - screen.min.y) / row).round() as u16,
                        (area.width() / col).floor() as u16,
                        (area.height() / row).floor() as u16,
                    ));
                }
                let t = lua.create_table()?;
                t.set("id", p.id)?;
                t.set("x", area.min.x)?;
                t.set("y", area.min.y)?;
                t.set("w", area.width())?;
                t.set("h", area.height())?;
                t.set("bounds", true)?;
                t.set("z", p.z)?;
                t.set("focus", p.focus)?;
                t.set("node", self.stored_function(lua, cb)?)?;
                out.push(t)?;
                continue;
            }
            let (cols, rows) = (
                (area.width() / col).floor() as u16,
                (area.height() / row).floor() as u16,
            );
            if cols == 0 || rows == 0 {
                continue;
            }
            let avail_w = p.width.unwrap_or(cols).min(cols);
            let avail_h = p.height.unwrap_or(rows).min(rows);
            let lines = bone_ui::lua::ui::popup_lines(self, p.id, p.lines, avail_w, avail_h);
            // A render callback may have closed the window.
            if !self.lua.popups.iter().any(|q| q.id == p.id) {
                continue;
            }
            let content_w = (lines.iter())
                .map(|l| {
                    (bone_ui::model::layout_items(l, avail_w as usize).iter())
                        .map(|(t, _)| bone_ui::text::width(t))
                        .sum::<usize>()
                })
                .max()
                .unwrap_or(0) as u16;
            let w = p.width.unwrap_or(content_w).min(cols);
            let h = p.height.unwrap_or(lines.len() as u16).min(rows);
            if w == 0 || h == 0 {
                continue;
            }
            let (wpx, hpx) = (f32::from(w) * col, f32::from(h) * row);
            // Centered, or `n` cells from the start (negative: from the end).
            let at = |n: Option<i32>, size: f32, total: f32, unit: f32| {
                let free = (total - size).max(0.0);
                match n {
                    None => (free / 2.0).floor(),
                    Some(n) if n < 0 => (free - (-n - 1) as f32 * unit).max(0.0),
                    Some(n) => (n as f32 * unit).min(free),
                }
            };
            // Above the prompt, a window sits right on it at the left.
            let x = area.min.x + at(p.col.or(prompt.then_some(0)), wpx, area.width(), col);
            let y = area.min.y + at(p.row.or(prompt.then_some(-1)), hpx, area.height(), row);
            if let Some(q) = self.lua.popups.iter_mut().find(|q| q.id == p.id) {
                // In cells of the window, for what reads it back.
                q.rect = Some(bone_ui::layout::Rect::new(
                    ((x - screen.min.x) / col).round() as u16,
                    ((y - screen.min.y) / row).round() as u16,
                    w,
                    h,
                ));
            }
            let t = lua.create_table()?;
            t.set("id", p.id)?;
            t.set("x", x)?;
            t.set("y", y)?;
            t.set("w", wpx)?;
            t.set("h", hpx)?;
            t.set("z", p.z)?;
            t.set("focus", p.focus)?;
            let rows_out = lua.create_table()?;
            for items in lines.iter().take(h as usize) {
                let spans = lua.create_table()?;
                for (text, hl) in bone_ui::model::layout_items(items, w as usize) {
                    let s = lua.create_table()?;
                    s.set("text", text)?;
                    s.set("hl", hl)?;
                    spans.push(s)?;
                }
                rows_out.push(spans)?;
            }
            t.set("lines", rows_out)?;
            if let Some(cb) = self.view.bound.window_nodes.get(&format!("popup:{}", p.id)) {
                t.set("node", self.stored_function(lua, *cb)?)?;
            }
            out.push(t)?;
        }
        let cell = lua.create_table()?;
        cell.set("w", col)?;
        cell.set("h", row)?;
        out.set("cell", cell)?;
        Ok(out)
    }

    /// Let go of the content functions of windows and panels that closed.
    pub(in crate::nodeview) fn forget_window_nodes(&mut self, lua: &Lua) -> mlua::Result<()> {
        let (popups, panels) = (&self.lua.popups, &self.lua.panels);
        let gone: Vec<String> = (self.view.bound.window_nodes.keys())
            .filter(|k| match k.split_once(':') {
                Some(("popup", id)) => !popups.iter().any(|p| p.id.to_string() == id),
                Some(("panel", id)) => !panels.iter().any(|p| p.id == id),
                _ => true,
            })
            .cloned()
            .collect();
        for k in gone {
            if let Some(cb) = self.view.bound.window_nodes.remove(&k) {
                drop_callback(lua, cb)?;
            }
        }
        Ok(())
    }

    /// A function stored with `store_callback`.
    pub(in crate::nodeview) fn stored_function(&self, lua: &Lua, cb: u64) -> mlua::Result<Value> {
        lua.named_registry_value::<mlua::Table>(bone_ui::lua::CALLBACKS)?
            .get(cb)
    }
}
