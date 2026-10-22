//! The window's main area, laid out by `bone.ui.layout` with the shared
//! solver, in character units: columns of the monospace font's advance
//! width and rows of its line height. Each leaf is drawn in its rectangle,
//! the chat, the prompt box, the statusline and divider, regions,
//! column separators and the message line. Rules are hairlines, and a
//! rounded prompt border is a card, so the window is not a wall of cells.

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
use crate::{ChatRow, ChatScroll, Gui, Row};

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
        let (col, row) = ui.ctx().fonts_mut(|f| {
            // A row is as tall as a line of text is laid out, so rows
            // stacked one after another stay on the cell grid.
            let line = f.layout_no_wrap("M".into(), font.clone(), egui::Color32::WHITE);
            (f.glyph_width(&font, 'M'), line.size().y)
        });
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
        self.lua.rich_keys.clear();
        let area = ui.max_rect();
        let cols = (area.width() / cells.col).floor().max(1.0) as u16;
        let rows = (area.height() / cells.row).floor().max(1.0) as u16;
        self.lua.screen_cols = cols;
        if self.lua.screen_size != (cols, rows) {
            self.lua.screen_size = (cols, rows);
            let size = serde_json::json!({ "width": cols, "height": rows });
            self.fire_event("resize", size);
        }
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
                "divider" => self.rule_leaf(ui, &cells, r),
                "statusline" => self.line_leaf(ui, &cells, r, buf),
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
        let rule = rule_color(self, ui);
        for (x, a, _, _) in std::mem::take(&mut plan.seps) {
            vline(ui, cells.rect(layout::Rect::new(x, a.y, 1, a.height)), rule);
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
    /// separator beside a side panel. The fill is a step off the canvas
    /// so a docked column reads as a sidebar.
    fn panels_ui(&mut self, ui: &mut egui::Ui, cells: &Cells, docked: &[PanelPlacement]) {
        let fill = lifted_color(
            (self.lua.theme.hl("Normal").bg)
                .and_then(color)
                .unwrap_or(ui.visuals().panel_fill),
        );
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
            // Rich content (`blocks`) in place of its lines.
            if !self.rich_ui(ui, &placement.id, cells.rect(f.body)) {
                self.lines_at(ui, cells, f.body, &f.lines);
            }
            if let Some(x) = f.separator {
                vline(
                    ui,
                    cells.rect(layout::Rect::new(x, f.area.y, 1, f.area.height)),
                    rule_color(self, ui),
                );
            }
        }
    }

    /// Lua windows over everything, bottom first, where the shared
    /// placement put them: the same anchors as the TUI's.
    fn popups_ui(&mut self, ui: &mut egui::Ui, cells: &Cells, screen: layout::Rect) {
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
        // A step off the canvas, with a shadow: the border glyphs stay,
        // the window does not sit flat on the transcript.
        let card = lifted_color(fill);
        let shadow = egui::epaint::Shadow {
            offset: [0, 6],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(80),
        };
        for (id, r, lines) in bone_ui::lua::layout_popups(self, &anchors) {
            let rect = cells.rect(r);
            let key = format!("popup:{id}");
            let rich = self.has_blocks(&key);
            // A rich window is a rounded card with room around its blocks.
            let radius = if rich { 12.0 } else { 0.0 };
            ui.painter().add(shadow.as_shape(rect, radius as u8));
            ui.painter().rect_filled(rect, radius, card);
            if !(rich && self.rich_ui(ui, &key, rect.shrink(12.0))) {
                self.lines_at(ui, cells, r, &lines);
            }
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
            let row = self.styled(items, r.width as usize);
            paint_row(ui, cells, at, &row);
            // A row naming an image in the prompt shows it.
            if !self.images.is_empty() {
                let text: String = row.iter().map(|(t, _)| t.as_str()).collect();
                if self.images.iter().any(|i| text.contains(&i.name)) {
                    let id = ui.id().with(("image row", at.x, at.y));
                    let resp = ui.interact(cells.rect(at), id, egui::Sense::click());
                    self.image_row(&resp, &text, &self.images);
                }
            }
        }
    }

    /// Items laid out in `width` columns, in their groups' styles.
    fn styled(&self, items: &[Item], width: usize) -> Row {
        bone_ui::model::layout_items(items, width)
            .into_iter()
            .map(|(t, hl)| (t, self.lua.theme.hl(hl)))
            .collect()
    }

    /// The divider, as a hairline instead of a row of box-drawing.
    fn rule_leaf(&self, ui: &egui::Ui, cells: &Cells, r: layout::Rect) {
        let rect = cells.rect(r);
        let y = rect.center().y;
        ui.painter()
            .hline(rect.x_range(), y, Stroke::new(1.0, rule_color(self, ui)));
    }

    /// Chat `buf`'s statusline: a full-width bar, then its text.
    fn line_leaf(&mut self, ui: &egui::Ui, cells: &Cells, r: layout::Rect, buf: usize) {
        let rect = cells.rect(layout::Rect { height: 1, ..r });
        if let Some(bg) = self.lua.theme.hl("StatusLine").bg.and_then(color) {
            ui.painter().rect_filled(rect, 0.0, bg);
        }
        let ctx = lua_ui::statusline_ctx(self, r.width, buf);
        let items = lua_ui::ui_items(self, "statusline", ctx, "StatusLine").unwrap_or_default();
        let row = self.styled(&items, r.width as usize);
        paint_row(ui, cells, layout::Rect { height: 1, ..r }, &row);
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            Stroke::new(1.0, rule_color(self, ui)),
        );
    }

    /// Chat `buf` in `r`, scrolled to follow the end.
    fn chat_leaf(&mut self, ui: &mut egui::Ui, cells: &Cells, r: layout::Rect, buf: usize) {
        let (was, follow) = self.lua.chat_views.get(&buf).copied().unwrap_or((0, true));
        let view = bone_ui::lua::ChatView {
            top: was,
            height: r.height as usize,
            follow,
        };
        let (rows, mut top) = self.layout_chat(buf, r.width as usize, view);
        // A key (pageup, ctrl+end, …) moves this chat. The offset has to
        // be set on the scroll area: while it is stuck to the end it
        // would pull a plain assignment back down.
        let mut pinned = false;
        if buf == self.current
            && let Some(scroll) = self.chat_scroll.take()
        {
            let max_top = rows.saturating_sub(r.height as usize);
            top = match scroll {
                ChatScroll::By(n) => (top as i64 + i64::from(n)).clamp(0, max_top as i64) as usize,
                ChatScroll::Top => 0,
                ChatScroll::Bottom => max_top,
                ChatScroll::To(row) => row.min(max_top),
            };
            pinned = true;
        }
        let at_end = top.saturating_add(r.height as usize) >= rows;
        let (gui, chat) = (&*self, &self.chats[buf]);
        let (font, col) = (cells.font.clone(), cells.col);
        let out = ui
            .scope_builder(UiBuilder::new().max_rect(cells.rect(r)), |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let mut area = ScrollArea::vertical()
                    .id_salt(("chat", buf))
                    .auto_shrink(false)
                    .stick_to_bottom(!pinned || at_end);
                if pinned || (top != was && !follow) {
                    area = area.vertical_scroll_offset(top as f32 * cells.row);
                }
                area.show_rows(ui, cells.row, rows, |ui, range| {
                    let rows = chat.rows().skip(range.start).take(range.len());
                    let width = ui.available_width();
                    for (n, row) in rows.enumerate() {
                        match row {
                            ChatRow::Text(row) => {
                                let resp = ui.label(row_job(ui, row, &font, col));
                                if resp.hovered() || resp.clicked() {
                                    let text: String =
                                        row.iter().map(|(t, _)| t.as_str()).collect();
                                    gui.chat_image_row(chat, range.start + n, &text, &resp);
                                }
                            }
                            // A block draws on its first row in view, over
                            // the rows it takes.
                            ChatRow::Block { block, part, .. } => {
                                let (rect, _) = ui.allocate_exact_size(
                                    vec2(width, cells.row),
                                    egui::Sense::hover(),
                                );
                                if *part == 0 || n == 0 {
                                    let top = rect.min - vec2(0.0, *part as f32 * cells.row);
                                    // Its images show on hover, as a row's do.
                                    if let Some(resp) = block.show(ui, top, width, true)
                                        && (resp.hovered() || resp.clicked())
                                    {
                                        let at = range.start + n - part;
                                        gui.chat_image_row(chat, at, &block.text(), &resp);
                                    }
                                }
                            }
                        }
                    }
                })
            })
            .inner;
        let offset = out.state.offset.y;
        let end = out.content_size.y - out.inner_rect.height();
        let top = (offset / cells.row).floor().max(0.0) as usize;
        (self.lua.chat_views).insert(buf, (top, offset >= end - cells.row / 2.0));
        if buf == self.current {
            self.lua.chat_top = top;
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
        // A rounded border is a card, not the box-drawing glyphs (those
        // rows stay empty and pad the text). Anything else is a sharp edge.
        let radius = if spec
            .border
            .as_ref()
            .is_some_and(|b| b.chars[0] == '\u{256d}')
        {
            10.0
        } else {
            0.0
        };
        if let Some(bg) = (spec.background.as_deref())
            .and_then(|hl| self.lua.theme.hl(hl).bg)
            .and_then(color)
        {
            ui.painter().rect_filled(px, radius, bg);
        }
        if let Some(b) = &spec.border {
            let fg = self.lua.theme.hl(&b.hl).fg.and_then(color);
            let stroke = Stroke::new(
                1.0,
                fg.unwrap_or(ui.visuals().widgets.noninteractive.fg_stroke.color),
            );
            ui.painter()
                .rect_stroke(px.shrink(1.0), radius, stroke, egui::StrokeKind::Inside);
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
        let hint_color = self
            .lua
            .theme
            .hl("Placeholder")
            .fg
            .and_then(color)
            .unwrap_or(Color32::from_rgb(0x66, 0x66, 0x66));
        self.sync_cursor(ui.ctx(), true);
        let mut edit = TextEdit::multiline(&mut self.draft)
            .id(crate::prompt::edit_id())
            .font(cells.font.clone())
            .frame(egui::Frame::NONE)
            .desired_width(f32::from(edit_area.width) * cells.col)
            .desired_rows(edit_area.height.max(1) as usize)
            .hint_text(egui::RichText::new(hint).color(hint_color))
            // Enter sends; Shift+Enter is the newline.
            .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter));
        if let Some(fg) = fg {
            edit = edit.text_color(fg);
        }
        let response = ui.put(cells.rect(edit_area), edit);
        self.sync_cursor(ui.ctx(), false);
        // Typing, or only moving the cursor: the event says which.
        self.prompt_changed();
        let _ = response.changed();
        if self.asks.is_empty()
            && !response.has_focus()
            && !ui.ctx().memory(|m| m.focused().is_some())
        {
            response.request_focus();
        }
    }

    /// Chat `buf`'s prompt text: the draft on screen, else empty.
    fn prompt_of(&self, buf: usize) -> TextBuffer {
        if buf == self.current {
            return self.buffer();
        }
        let mut t = TextBuffer::default();
        if let Some((draft, _)) = self.drafts.get(&buf) {
            t.set_text(draft);
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
        .layout_job(row_job(ui, row, &cells.font, cells.col));
    let rect = cells.rect(r);
    ui.painter()
        .with_clip_rect(rect)
        .galley(rect.min, galley, Color32::PLACEHOLDER);
    // Painted text is invisible to screen readers: a row with words gets a
    // label node (hover only, so clicks still go where they did).
    let text: String = row.iter().map(|(t, _)| t.as_str()).collect();
    if text.chars().any(char::is_alphanumeric) {
        let id = ui.id().with(("cells", r.x, r.y));
        let resp = ui.interact(rect, id, egui::Sense::hover());
        let text = text.trim().to_owned();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &text));
    }
}

fn rule_color(gui: &Gui, ui: &egui::Ui) -> Color32 {
    gui.lua
        .theme
        .hl("WinSeparator")
        .fg
        .and_then(color)
        .unwrap_or(ui.visuals().widgets.noninteractive.fg_stroke.color)
}

fn vline(ui: &egui::Ui, rect: egui::Rect, color: Color32) {
    ui.painter()
        .vline(rect.center().x, rect.y_range(), Stroke::new(1.0, color));
}

/// A step lighter than `c`, so a panel or popup sits off the canvas.
fn lifted_color(c: Color32) -> Color32 {
    Color32::from_rgb(
        c.r().saturating_add(14),
        c.g().saturating_add(14),
        c.b().saturating_add(14),
    )
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
