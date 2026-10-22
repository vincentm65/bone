//! The window's main area, laid out by `bone.ui.layout` with the shared
//! solver, in character units: columns of the monospace font's advance
//! width and rows of its line height. Each leaf is drawn in its rectangle,
//! as in the TUI: the chat, the prompt box, the statusline and divider,
//! regions, column separators and the message line.

use bone_ui::chat::find_chat;
use bone_ui::editor::TextBuffer;
use bone_ui::layout::{self, Measure, Plan, kind};
use bone_ui::lua::Geometry;
use bone_ui::lua::panels;
use bone_ui::lua::ui as lua_ui;
use bone_ui::model::Item;
use bone_ui::panel::PanelPlacement;
use bone_ui::prompt::Insets;
use eframe::egui::{
    self, Color32, FontId, Key, KeyboardShortcut, Modifiers, Pos2, ScrollArea, Stroke, TextEdit,
    TextStyle, UiBuilder, vec2,
};

use crate::view::{color, row_job};
use crate::{Gui, Row};

/// Where character cells are on screen.
pub(crate) struct Cells {
    pub origin: Pos2,
    pub col: f32,
    pub row: f32,
    pub font: FontId,
}

impl Cells {
    pub fn of(ui: &egui::Ui) -> Self {
        let font = FontId::monospace(TextStyle::Monospace.resolve(ui.style()).size);
        let (col, row) = ui
            .ctx()
            .fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        Cells {
            origin: ui.max_rect().min,
            col: col.max(1.0),
            row: row.max(1.0),
            font,
        }
    }

    pub fn rect(&self, r: layout::Rect) -> egui::Rect {
        egui::Rect::from_min_size(
            self.origin + vec2(f32::from(r.x) * self.col, f32::from(r.y) * self.row),
            vec2(
                f32::from(r.width) * self.col,
                f32::from(r.height) * self.row,
            ),
        )
    }
}

impl Gui {
    /// Lay the window out with `bone.ui.layout` and draw every leaf.
    pub(crate) fn screen_ui(&mut self, ui: &mut egui::Ui) {
        let cells = Cells::of(ui);
        self.lua.cells = Some((cells.origin.x, cells.origin.y, cells.col, cells.row));
        let area = ui.max_rect();
        let cols = (area.width() / cells.col).floor().max(1.0) as u16;
        let rows = (area.height() / cells.row).floor().max(1.0) as u16;
        self.lua.screen_cols = cols;
        self.lua.spinner = lua_ui::read_spinner(self);
        // The Normal group's background (if any) fills the screen.
        if let Some(bg) = self.lua.theme.hl("Normal").bg.and_then(color) {
            ui.painter().rect_filled(area, 0.0, bg);
        }
        let layout = lua_ui::layout(self);
        let full = layout::Rect::new(0, 0, cols, rows);
        // Screen-height side panels take their columns from everything.
        let mut docked = Vec::new();
        let room = panels::allocate_panels(self, full, true, &layout, &mut docked);
        // Without a "message" leaf, notifications take rows at the bottom.
        let msg_rows = if layout.contains("message") {
            0
        } else {
            self.message_rows(room)
        };
        let main = layout::Rect {
            height: room.height.saturating_sub(msg_rows),
            ..room
        };
        let mut plan = Plan::default();
        layout::place(
            &mut Measurer(self),
            &layout,
            main,
            &mut plan,
            &mut Vec::new(),
        );
        // Docked panels go beside the chat; the layout's own panel leaves
        // where it put them.
        for (name, r) in &mut plan.leaves {
            if name == "chat" {
                *r = panels::allocate_panels(self, *r, false, &layout, &mut docked);
            }
            if let Some(id) = name.strip_prefix("panel:") {
                let shown = panels::panel(&self.lua.panels, id).is_some_and(|p| !p.hidden);
                if shown && r.width > 0 && r.height > 0 && !docked.iter().any(|p| p.id == id) {
                    docked.push(PanelPlacement {
                        id: id.to_owned(),
                        area: *r,
                        separator: None,
                    });
                } else {
                    r.width = 0;
                    r.height = 0;
                }
            }
        }
        panels::publish_panels(self, &docked);
        self.lua.geometry = Geometry {
            leaves: plan.leaves.clone(),
            seps: (plan.seps.iter())
                .map(|(x, a, _, path)| (layout::Rect::new(*x, a.y, 1, a.height), path.clone()))
                .collect(),
        };
        // A view's timer (`bone.chat.refresh_in`) asks for the next frame.
        self.lua.chat_expiry = None;
        let mut drawn = Vec::new();
        for (name, r) in std::mem::take(&mut plan.leaves) {
            if r.width == 0 || r.height == 0 {
                continue;
            }
            let k = kind(&name);
            let buf = match name.split_once(':') {
                Some((_, id)) if k != "panel" => find_chat(&self.chats, id),
                _ => Some(self.current),
            };
            let Some(buf) = buf.filter(|b| !drawn.contains(&(k.to_owned(), *b))) else {
                continue;
            };
            drawn.push((k.to_owned(), buf));
            self.lua.rendering = Some(buf);
            match k {
                "chat" => self.chat_leaf(ui, &cells, r, buf),
                "prompt" => self.prompt_leaf(ui, &cells, r, buf),
                "divider" | "statusline" => self.line_leaf(ui, &cells, r, buf, k),
                "message" => self.message_leaf(ui, &cells, r),
                _ => {}
            }
            self.lua.rendering = None;
        }
        for (name, r) in std::mem::take(&mut plan.regions) {
            // Measurement saw available space; painting sees final space.
            if let Some((_, lines)) =
                lua_ui::region_sized(self, &name, r.width, r.height, true, true)
            {
                self.lines_at(ui, &cells, r, &lines);
            }
        }
        let sep_style = self.lua.theme.hl("WinSeparator");
        for (x, a, sep, _) in std::mem::take(&mut plan.seps) {
            for y in a.y..a.bottom() {
                let row = vec![(sep.clone(), sep_style)];
                paint_row(ui, &cells, layout::Rect::new(x, y, 1, 1), &row);
            }
        }
        self.panels_ui(ui, &cells, &docked);
        if msg_rows > 0 {
            let r = layout::Rect::new(room.x, main.bottom(), room.width, msg_rows);
            self.message_leaf(ui, &cells, r);
        }
        self.popups_ui(ui, &cells, full);
        // Timers from Lua ask for the next frame; a due UI timer is used up.
        let now = std::time::Instant::now();
        if self.lua.ui_expiry.is_some_and(|at| at <= now) {
            self.lua.ui_expiry = None;
        }
        for at in [self.lua.chat_expiry, self.lua.ui_expiry]
            .into_iter()
            .flatten()
        {
            ui.ctx()
                .request_repaint_after(at.saturating_duration_since(now));
        }
    }

    /// Each placed panel: its title row, the content in view, and a
    /// separator bar beside a side panel.
    fn panels_ui(&mut self, ui: &egui::Ui, cells: &Cells, docked: &[PanelPlacement]) {
        let fill = (self.lua.theme.hl("Normal").bg)
            .and_then(color)
            .unwrap_or(ui.visuals().panel_fill);
        let sep = self.lua.theme.hl("WinSeparator");
        for placement in docked {
            let Some(f) = panels::panel_frame(self, placement) else {
                continue;
            };
            ui.painter().rect_filled(cells.rect(f.area), 0.0, fill);
            if let Some(items) = &f.title {
                let row = self.styled(items, f.area.width as usize);
                paint_row(
                    ui,
                    cells,
                    layout::Rect {
                        height: 1,
                        ..f.area
                    },
                    &row,
                );
            }
            self.lines_at(ui, cells, f.body, &f.lines);
            if let Some(x) = f.separator {
                for y in f.area.y..f.area.bottom() {
                    paint_row(
                        ui,
                        cells,
                        layout::Rect::new(x, y, 1, 1),
                        &vec![("│".into(), sep)],
                    );
                }
            }
        }
    }

    /// Lua windows over everything, bottom first, where the shared
    /// placement put them: the same anchors as the TUI's.
    fn popups_ui(&mut self, ui: &egui::Ui, cells: &Cells, screen: layout::Rect) {
        if self.lua.popups.is_empty() {
            return;
        }
        let leaf = |name: &str| {
            (self.lua.geometry.leaves.iter())
                .find(|(n, r)| n == name && r.width > 0)
                .map(|(_, r)| *r)
        };
        let chat = leaf("chat").unwrap_or(screen);
        let prompt_top = leaf("prompt").map_or(screen.bottom(), |r| r.y);
        let anchors = bone_ui::popup::Anchors {
            screen,
            chat,
            prompt: layout::Rect {
                y: chat.y,
                height: prompt_top.saturating_sub(chat.y),
                ..screen
            },
        };
        let fill = (self.lua.theme.hl("Normal").bg)
            .and_then(color)
            .unwrap_or(ui.visuals().panel_fill);
        for (_, r, lines) in bone_ui::lua::layout_popups(self, &anchors) {
            ui.painter().rect_filled(cells.rect(r), 0.0, fill);
            self.lines_at(ui, cells, r, &lines);
        }
    }

    /// Rows the message line takes in `area`.
    fn message_rows(&self, area: layout::Rect) -> u16 {
        match &self.status {
            Some(s) => (s.text.lines().count().max(1) as u16)
                .min(area.height / 2)
                .max(1),
            None => 0,
        }
    }

    fn message_leaf(&self, ui: &egui::Ui, cells: &Cells, r: layout::Rect) {
        let Some(s) = &self.status else {
            return;
        };
        let style = self
            .lua
            .theme
            .hl(if s.error { "ErrorMsg" } else { "Normal" });
        for (i, line) in s.text.lines().take(r.height as usize).enumerate() {
            let at = layout::Rect::new(r.x, r.y + i as u16, r.width, 1);
            paint_row(ui, cells, at, &vec![(line.to_owned(), style)]);
        }
    }

    /// Lines of Lua items, one per row of `r`.
    fn lines_at(&self, ui: &egui::Ui, cells: &Cells, r: layout::Rect, lines: &[Vec<Item>]) {
        for (i, items) in lines.iter().take(r.height as usize).enumerate() {
            let at = layout::Rect::new(r.x, r.y + i as u16, r.width, 1);
            paint_row(ui, cells, at, &self.styled(items, r.width as usize));
        }
    }

    /// Items laid out in `width` columns, in their groups' styles.
    fn styled(&self, items: &[Item], width: usize) -> Row {
        bone_ui::model::layout_items(items, width)
            .into_iter()
            .map(|(t, hl)| (t, self.lua.theme.hl(hl)))
            .collect()
    }

    /// Chat `buf`'s statusline or divider row.
    fn line_leaf(&mut self, ui: &egui::Ui, cells: &Cells, r: layout::Rect, buf: usize, k: &str) {
        let hl = if k == "divider" {
            "WinSeparator"
        } else {
            "StatusLine"
        };
        let ctx = lua_ui::statusline_ctx(self, r.width, buf);
        let items = lua_ui::ui_items(self, k, ctx, hl).unwrap_or_default();
        let row = self.styled(&items, r.width as usize);
        paint_row(ui, cells, layout::Rect { height: 1, ..r }, &row);
    }

    /// Chat `buf` in `r`, scrolled to follow the end.
    fn chat_leaf(&mut self, ui: &mut egui::Ui, cells: &Cells, r: layout::Rect, buf: usize) {
        let rows = self.layout_chat(buf, r.width as usize);
        let chat = &self.chats[buf];
        let font = cells.font.clone();
        let offset = ui
            .scope_builder(UiBuilder::new().max_rect(cells.rect(r)), |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ScrollArea::vertical()
                    .id_salt(("chat", buf))
                    .auto_shrink(false)
                    .stick_to_bottom(true)
                    .show_rows(ui, cells.row, rows, |ui, range| {
                        for row in chat.rows().skip(range.start).take(range.len()) {
                            ui.label(row_job(row, &font, ui.visuals()));
                        }
                    })
                    .state
                    .offset
                    .y
            })
            .inner;
        if buf == self.current {
            self.lua.chat_top = (offset / cells.row).floor().max(0.0) as usize;
        }
    }

    /// Chat `buf`'s prompt box: background, border and edge lines from
    /// `bone.ui.prompt`, the prefix, and the text (edited natively, so input
    /// methods and selection work).
    fn prompt_leaf(&mut self, ui: &mut egui::Ui, cells: &Cells, r: layout::Rect, buf: usize) {
        let text = self.prompt_of(buf);
        let ctx = lua_ui::prompt_ctx(self, r.width, buf, &text);
        let spec = lua_ui::prompt_spec(self, &ctx);
        let insets = Insets::of(&spec, r.width);
        let px = cells.rect(r);
        if let Some(bg) = (spec.background.as_deref())
            .and_then(|hl| self.lua.theme.hl(hl).bg)
            .and_then(color)
        {
            ui.painter().rect_filled(px, 0.0, bg);
        }
        if let Some(b) = &spec.border {
            let fg = self.lua.theme.hl(&b.hl).fg.and_then(color);
            let stroke = Stroke::new(
                1.0,
                fg.unwrap_or(ui.visuals().widgets.noninteractive.fg_stroke.color),
            );
            let radius = if b.chars[0] == '╭' { 6.0 } else { 0.0 };
            let half = vec2(cells.col, cells.row) * 0.5;
            ui.painter().rect_stroke(
                egui::Rect::from_min_max(px.min + half, px.max - half),
                radius,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        for (edge, y) in [("top", r.y), ("bottom", r.bottom().saturating_sub(1))] {
            let has = if edge == "top" {
                spec.has_top
            } else {
                spec.has_bottom
            };
            if has {
                let hl = spec
                    .border
                    .as_ref()
                    .map_or("Normal", |b| b.hl.as_str())
                    .to_owned();
                let items = lua_ui::prompt_edge(self, edge, &ctx, &hl);
                let at = layout::Rect::new(r.x + 1, y, r.width.saturating_sub(2), 1);
                let row = self.styled(&items, at.width as usize);
                paint_row(ui, cells, at, &row);
            }
        }
        let text_area = layout::Rect::new(
            r.x + insets.left,
            r.y + insets.top,
            r.width.saturating_sub(insets.left + insets.right),
            r.height.saturating_sub(insets.top + insets.bottom),
        );
        let prefix = self.styled(&spec.prefix, insets.gutter);
        paint_row(
            ui,
            cells,
            layout::Rect {
                height: 1,
                ..text_area
            },
            &prefix,
        );
        let edit_area = layout::Rect {
            x: text_area.x + insets.gutter as u16,
            width: text_area.width.saturating_sub(insets.gutter as u16),
            ..text_area
        };
        if buf != self.current {
            // Another chat's draft, shown as it is.
            let row = vec![(text.text(), self.lua.theme.hl("Normal"))];
            paint_row(
                ui,
                cells,
                layout::Rect {
                    height: 1,
                    ..edit_area
                },
                &row,
            );
            return;
        }
        let hint: String = spec
            .placeholder
            .iter()
            .map(|i| match i {
                Item::Text(t, _) | Item::Fill(t, _) => t.as_str(),
            })
            .collect();
        let fg = self.lua.theme.hl("Normal").fg.and_then(color);
        let mut edit = TextEdit::multiline(&mut self.draft)
            .font(cells.font.clone())
            .frame(egui::Frame::NONE)
            .desired_width(f32::from(edit_area.width) * cells.col)
            .desired_rows(edit_area.height.max(1) as usize)
            .hint_text(hint)
            // Enter sends; Shift+Enter is the newline.
            .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter));
        if let Some(fg) = fg {
            edit = edit.text_color(fg);
        }
        let response = ui.put(cells.rect(edit_area), edit);
        if response.changed() {
            self.prompt_changed();
        }
        if self.asks.is_empty()
            && !response.has_focus()
            && !ui.ctx().memory(|m| m.focused().is_some())
        {
            response.request_focus();
        }
    }

    /// Chat `buf`'s prompt text: the draft on screen, else empty.
    fn prompt_of(&self, buf: usize) -> TextBuffer {
        let mut t = TextBuffer::default();
        if buf == self.current {
            t.set_text(&self.draft);
        }
        t
    }
}

/// One row of styled text, clipped to `r`.
fn paint_row(ui: &egui::Ui, cells: &Cells, r: layout::Rect, row: &Row) {
    if r.width == 0 || row.is_empty() {
        return;
    }
    let galley = ui
        .painter()
        .layout_job(row_job(row, &cells.font, ui.visuals()));
    let rect = cells.rect(r);
    ui.painter()
        .with_clip_rect(rect)
        .galley(rect.min, galley, Color32::PLACEHOLDER);
}

/// What the layout asks the GUI while it sizes leaves.
struct Measurer<'a>(&'a mut Gui);

impl Measure for Measurer<'_> {
    fn panel_hidden(&self, id: &str) -> bool {
        panels::panel(&self.0.lua.panels, id).is_none_or(|p| p.hidden)
    }

    fn prompt_height(&mut self, name: &str, width: u16) -> u16 {
        let gui = &mut *self.0;
        let buf = name
            .split_once(':')
            .and_then(|(_, id)| find_chat(&gui.chats, id))
            .unwrap_or(gui.current);
        let text = gui.prompt_of(buf);
        let ctx = lua_ui::prompt_ctx(gui, width, buf, &text);
        let spec = lua_ui::prompt_spec(gui, &ctx);
        bone_ui::prompt::natural_height(&spec, &text, width, gui.lua.options.prompt_max_height)
    }

    fn line_defined(&self, name: &str) -> bool {
        lua_ui::ui_defined(self.0, name)
    }

    fn message_rows(&self, area: layout::Rect) -> u16 {
        self.0.message_rows(area)
    }

    fn panel_size(&mut self, id: &str, width: u16, height: u16, rows: bool) -> u16 {
        panels::measure_panel(self.0, id, width, height, rows)
    }

    fn region_size(&mut self, name: &str, width: u16, height: u16, rows: bool) -> Option<u16> {
        lua_ui::region_sized(self.0, name, width, height, rows, false).map(|(size, _)| size)
    }
}
