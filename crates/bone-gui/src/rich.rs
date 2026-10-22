//! Rich content in panels and windows: a panel or window opened with
//! `blocks` (see `runtime/gui/api.lua`) is drawn as GUI blocks stacked in a
//! scroll area, in place of grid lines. Lua gives the blocks each frame,
//! marks the selected one (kept in view), and hears about clicks through
//! `on_block(id)`.

use std::sync::Arc;

use bone_ui::lua::Host;
use eframe::egui;
use mlua::{Function, Table, Value};
use serde_json::Value as Json;

use crate::Gui;
use crate::block::Block;

/// One block of rich content: its id (for `on_block`), the block, and
/// whether it is the selected one.
struct Entry {
    id: Json,
    block: Arc<Block>,
    selected: bool,
}

impl Gui {
    /// Whether `key` (a panel's id, or `popup:<id>`) has rich content.
    pub(crate) fn has_blocks(&self, key: &str) -> bool {
        let Some(lua) = self.lua.state.as_ref() else {
            return false;
        };
        let rich = || -> mlua::Result<bool> {
            let bone: Table = lua.globals().get("bone")?;
            let all: Option<Table> = bone.get("_gui_blocks")?;
            Ok(all.is_some_and(|t| t.contains_key(key).unwrap_or(false)))
        };
        rich().unwrap_or(false)
    }

    /// `key`'s blocks for a `width` × `height` point area, or `None` when
    /// it has none (or they failed: reported once, until it is redefined).
    fn entries(&mut self, key: &str, width: f32, height: f32) -> Option<Vec<Entry>> {
        let broken = format!("blocks.{key}");
        if self.lua.ui_broken.contains(&broken) || !self.has_blocks(key) {
            return None;
        }
        let r = self.with_api(|lua| {
            let all: Table = lua.globals().get::<Table>("bone")?.get("_gui_blocks")?;
            let Some(rich) = all.get::<Option<Table>>(key)? else {
                return Ok(Json::Null);
            };
            let f: Function = rich.get("blocks")?;
            let ctx = lua.create_table()?;
            ctx.set("width", width)?;
            ctx.set("height", height)?;
            let v: Value = f.call(ctx)?;
            bone_lua::from_lua(&v)
        });
        let list = match r {
            Ok(Json::Array(list)) => list,
            Ok(_) => return Some(Vec::new()),
            Err(e) => {
                self.lua.ui_broken.insert(broken);
                self.lua_error(&format!("blocks of {key} failed"), &e);
                return None;
            }
        };
        Some(
            list.into_iter()
                .map(|e| Entry {
                    block: Arc::new(self.block(&e["block"])),
                    selected: e["selected"] == true,
                    id: e["id"].clone(),
                })
                .collect(),
        )
    }

    /// Draw `key`'s rich content in `rect`; false when it has none (its
    /// lines are drawn instead).
    pub(crate) fn rich_ui(&mut self, ui: &mut egui::Ui, key: &str, rect: egui::Rect) -> bool {
        let Some(entries) = self.entries(key, rect.width(), rect.height()) else {
            return false;
        };
        let selected = entries
            .iter()
            .find(|e| e.selected)
            .map(|e| e.id.to_string());
        let moved = self.lua.rich_selected.get(key) != selected.as_ref();
        let mut clicked = None;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("rich", key)));
        egui::ScrollArea::vertical()
            .id_salt(("rich scroll", key))
            .auto_shrink(false)
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let width = ui.available_width();
                for e in &entries {
                    let size = e.block.measure(ui.ctx(), width);
                    let (r, resp) =
                        ui.allocate_exact_size(egui::vec2(width, size.y), egui::Sense::click());
                    if ui.is_rect_visible(r) {
                        e.block.show(ui, r.min, width, false);
                    }
                    if resp.clicked() {
                        clicked = Some(e.id.clone());
                    }
                    if e.selected && moved {
                        ui.scroll_to_rect(r, None);
                    }
                }
                // Past the last block, so a card's clip does not cut its
                // last line when the list is scrolled to the end.
                if key.starts_with("popup:") {
                    ui.add_space(12.0);
                }
            });
        match selected {
            Some(s) => self.lua.rich_selected.insert(key.to_owned(), s),
            None => self.lua.rich_selected.remove(key),
        };
        self.lua.rich_keys.insert(key.to_owned());
        if let Some(id) = clicked {
            let key = key.to_owned();
            let r = self.with_api(|lua| {
                let all: Table = lua.globals().get::<Table>("bone")?.get("_gui_blocks")?;
                let rich: Option<Table> = all.get(key.as_str())?;
                if let Some(f) =
                    rich.and_then(|r| r.get::<Option<Function>>("on_block").ok().flatten())
                {
                    f.call::<()>(bone_lua::to_lua(lua, &id)?)?;
                }
                Ok(())
            });
            if let Err(e) = r {
                self.lua_error("on_block", &e);
            }
            Host::changed(self);
        }
        true
    }
}
