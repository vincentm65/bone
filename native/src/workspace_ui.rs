//! Native workspace presentation. Conversations stay in DesktopApp::tabs;
//! every layout operation changes stable ID references, never a connection.
use std::collections::HashMap;

use eframe::egui;

use crate::workspace::{Axis, Id, Node, Pane};
use crate::{DesktopApp, RowIndicator, file_refs, icons, layout};

const TAB_HEIGHT: f32 = 36.0;

#[derive(Default)]
pub(crate) struct WindowUi {
    pub rendering: Id,
    pointers: HashMap<Id, egui::Pos2>,
    dialog_window: Option<Id>,
    close_window: Option<Id>,
    quitting: bool,
    restored_root: bool,
    focus_window: Option<Id>,
    reveal_tab: Option<Id>,
}

impl WindowUi {
    pub(crate) fn closing_window(&self) -> bool {
        self.close_window.is_some()
    }
}

#[derive(Clone, Copy)]
struct DragTab(Id);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DropZone {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

impl DropZone {
    fn at(rect: egui::Rect, pos: egui::Pos2) -> Option<Self> {
        if !rect.is_positive() || !rect.contains(pos) {
            return None;
        }
        let x = (pos.x - rect.left()) / rect.width();
        let y = (pos.y - rect.top()) / rect.height();
        let (distance, edge) = [
            (x, Self::Left),
            (1.0 - x, Self::Right),
            (y, Self::Top),
            (1.0 - y, Self::Bottom),
        ]
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))?;
        Some(if distance < 0.25 { edge } else { Self::Center })
    }

    fn split(self) -> Option<(Axis, bool)> {
        match self {
            Self::Center => None,
            Self::Left => Some((Axis::Horizontal, true)),
            Self::Right => Some((Axis::Horizontal, false)),
            Self::Top => Some((Axis::Vertical, true)),
            Self::Bottom => Some((Axis::Vertical, false)),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Center => "Move to this pane",
            Self::Left => "Split left",
            Self::Right => "Split right",
            Self::Top => "Split above",
            Self::Bottom => "Split below",
        }
    }

    fn preview(self, mut rect: egui::Rect) -> egui::Rect {
        match self {
            Self::Center => {}
            Self::Left => rect.max.x = rect.center().x,
            Self::Right => rect.min.x = rect.center().x,
            Self::Top => rect.max.y = rect.center().y,
            Self::Bottom => rect.min.y = rect.center().y,
        }
        rect.shrink(4.0)
    }
}

pub(crate) fn viewport(id: Id) -> egui::ViewportId {
    if id == 0 {
        egui::ViewportId::ROOT
    } else {
        egui::ViewportId::from_hash_of(("bone-window", id))
    }
}

impl DesktopApp {
    pub(crate) fn conversation_location(&self, tab: Id) -> String {
        let Some((window, pane)) = self.workspace.tab_location(tab) else {
            return String::new();
        };
        let number = self
            .workspace
            .window(window)
            .and_then(|window| window.root.panes().iter().position(|p| p.id == pane))
            .map_or(1, |index| index + 1);
        format!("{} · Pane {number}", window_name(window))
    }

    pub(crate) fn sync_window_selection(&mut self, window: Id) {
        self.selected = self
            .workspace
            .active_tab(window)
            .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
            .unwrap_or(usize::MAX);
    }

    pub(crate) fn focus_conversation(&mut self, tab: Id, ctx: &egui::Context) {
        let changed = self.workspace.active_tab(self.window_ui.rendering) != Some(tab);
        if let Some(window) = self.workspace.focus_tab(tab) {
            if changed {
                ctx.memory_mut(|memory| memory.stop_text_input());
                self.window_ui.reveal_tab = Some(tab);
            }
            self.sync_window_selection(window);
            if window != self.window_ui.rendering {
                self.window_ui.focus_window = Some(window);
                ctx.send_viewport_cmd_to(viewport(window), egui::ViewportCommand::Focus);
            }
            self.note_layout_change(ctx);
            ctx.request_repaint();
        }
    }

    fn focus_group(&mut self, pane: Id, ctx: &egui::Context) {
        if self.workspace.focused_pane(self.window_ui.rendering) != Some(pane) {
            ctx.memory_mut(|memory| memory.stop_text_input());
        }
        self.workspace.focus_pane(pane);
        self.sync_window_selection(self.window_ui.rendering);
        self.note_layout_change(ctx);
    }

    pub(crate) fn select_group_tab(&mut self, index: usize, ctx: &egui::Context) -> bool {
        let tab = self
            .workspace
            .focused_pane(self.window_ui.rendering)
            .and_then(|id| self.workspace.pane(id))
            .and_then(|pane| pane.tabs.get(index))
            .copied();
        if let Some(tab) = tab {
            self.focus_conversation(tab, ctx);
            true
        } else {
            false
        }
    }

    pub(crate) fn cycle_group_tab(&mut self, direction: isize, ctx: &egui::Context) -> bool {
        let Some(pane) = self
            .workspace
            .focused_pane(self.window_ui.rendering)
            .and_then(|id| self.workspace.pane(id))
        else {
            return false;
        };
        if pane.tabs.is_empty() {
            return false;
        }
        let index = pane
            .tabs
            .iter()
            .position(|id| Some(*id) == pane.active)
            .unwrap_or(0);
        let next = (index as isize + direction).rem_euclid(pane.tabs.len() as isize) as usize;
        self.select_group_tab(next, ctx)
    }

    pub(crate) fn split_conversation(&mut self, tab: Id, axis: Axis, ctx: &egui::Context) {
        if self.workspace.split_tab(tab, axis).is_some() {
            self.focus_conversation(tab, ctx);
        }
    }

    pub(crate) fn new_native_window(&mut self, ctx: &egui::Context) {
        let id = self.workspace.new_window();
        self.window_ui.focus_window = Some(id);
        self.note_layout_change(ctx);
        ctx.request_repaint();
    }

    fn move_conversation(&mut self, tab: Id, pane: Id, index: usize, ctx: &egui::Context) {
        if self.workspace.move_tab(tab, pane, index) {
            self.focus_conversation(tab, ctx);
        }
    }

    pub(crate) fn tab_context_menu(&mut self, tab: Id, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        for (label, axis) in [
            ("Split right", Axis::Horizontal),
            ("Split down", Axis::Vertical),
        ] {
            if ui.button(label).clicked() {
                self.split_conversation(tab, axis, &ctx);
                ui.close();
            }
        }
        if let Some((_, pane_id)) = self.workspace.tab_location(tab) {
            let pane = self.workspace.pane(pane_id).unwrap();
            let index = pane.tabs.iter().position(|id| *id == tab).unwrap();
            let count = pane.tabs.len();
            if ui
                .add_enabled(index > 0, egui::Button::new("Move tab left"))
                .clicked()
            {
                self.move_conversation(tab, pane_id, index - 1, &ctx);
                ui.close();
            }
            if ui
                .add_enabled(index + 1 < count, egui::Button::new("Move tab right"))
                .clicked()
            {
                self.move_conversation(tab, pane_id, index + 2, &ctx);
                ui.close();
            }
        }
        ui.menu_button("Move tab to…", |ui| {
            let targets: Vec<_> = self
                .workspace
                .windows
                .iter()
                .flat_map(|window| {
                    window
                        .root
                        .panes()
                        .into_iter()
                        .enumerate()
                        .map(move |(i, pane)| (window.id, pane.id, i + 1, pane.tabs.len()))
                })
                .collect();
            for (window, pane, number, count) in targets {
                if self.workspace.tab_location(tab) == Some((window, pane)) {
                    continue;
                }
                if ui
                    .button(format!("{} · Pane {number}", window_name(window)))
                    .clicked()
                {
                    self.move_conversation(tab, pane, count, &ctx);
                    ui.close();
                }
            }
            if ui.button("New window").clicked() {
                if let Some(window) = self.workspace.detach_tab(tab) {
                    self.window_ui.focus_window = Some(window);
                    self.focus_conversation(tab, &ctx);
                }
                ui.close();
            }
        });
        if let Some((source, pane)) = self.workspace.tab_location(tab) {
            ui.menu_button("Merge group into…", |ui| {
                let targets: Vec<_> = self
                    .workspace
                    .windows
                    .iter()
                    .flat_map(|window| {
                        window
                            .root
                            .panes()
                            .into_iter()
                            .enumerate()
                            .map(move |(i, p)| (window.id, i + 1, p.id))
                    })
                    .filter(|(_, _, id)| *id != pane)
                    .collect();
                for (window, number, target) in targets {
                    if ui
                        .button(format!("{} · Pane {number}", window_name(window)))
                        .clicked()
                    {
                        if self.workspace.merge_pane(pane, target) {
                            self.focus_conversation(tab, &ctx);
                        }
                        ui.close();
                    }
                }
            });
            ui.menu_button("Move group to…", |ui| {
                let targets: Vec<_> = self
                    .workspace
                    .windows
                    .iter()
                    .map(|window| window.id)
                    .filter(|id| *id != source)
                    .collect();
                for window in targets {
                    if ui.button(window_name(window)).clicked() {
                        if self.workspace.move_pane_to_window(pane, window) {
                            self.window_ui.focus_window = Some(window);
                            self.focus_conversation(tab, &ctx);
                        }
                        ui.close();
                    }
                }
                if ui.button("New window").clicked() {
                    if let Some(window) = self.workspace.detach_pane(pane) {
                        self.window_ui.focus_window = Some(window);
                        self.note_layout_change(&ctx);
                        ctx.request_repaint();
                    }
                    ui.close();
                }
            });
        }
    }

    pub(crate) fn window_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button("New window  Ctrl+Shift+N").clicked() {
            self.new_native_window(ui.ctx());
            ui.close();
        }
        if let Some(tab) = self.tabs.get(self.selected).map(|tab| tab.id) {
            ui.separator();
            self.tab_context_menu(tab, ui);
        }
        ui.separator();
        let windows: Vec<_> = self
            .workspace
            .windows
            .iter()
            .map(|window| window.id)
            .collect();
        ui.menu_button("Focus window", |ui| {
            for id in windows {
                if ui.button(window_name(id)).clicked() {
                    self.workspace.active_window = id;
                    self.window_ui.focus_window = Some(id);
                    ui.ctx()
                        .send_viewport_cmd_to(viewport(id), egui::ViewportCommand::Focus);
                    ui.close();
                }
            }
        });
        if ui
            .button(if self.window_ui.rendering == 0 {
                "Quit Bone Desktop…"
            } else {
                "Close window…"
            })
            .clicked()
        {
            self.window_ui.close_window = Some(self.window_ui.rendering);
            ui.close();
        }
    }

    pub(crate) fn render_workspace(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let record_root_geometry = self.window_ui.restored_root;
        if !self.window_ui.restored_root {
            self.window_ui.restored_root = true;
            if let Some(window) = self.workspace.window(0) {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(window.size.into()));
                if let Some(pos) = window.position {
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos.into()));
                }
            }
        }
        self.render_native_window(0, ui, record_root_geometry);
        let windows: Vec<_> = self
            .workspace
            .windows
            .iter()
            .filter(|window| window.id != 0)
            .cloned()
            .collect();
        for window in windows {
            // A previous window's action may have removed this one in this pass.
            if self.workspace.window(window.id).is_none() {
                continue;
            }
            let mut builder = egui::ViewportBuilder::default()
                .with_title(format!("Bone Desktop — {}", window_name(window.id)))
                .with_inner_size(window.size)
                .with_min_inner_size([520.0, 400.0]);
            if let Some(pos) = window.position {
                builder = builder.with_position(pos);
            }
            ctx.show_viewport_immediate(viewport(window.id), builder, |ui, _class| {
                self.render_native_window(window.id, ui, true);
            });
        }
        if let Some(window) = self.window_ui.focus_window.take() {
            ctx.send_viewport_cmd_to(viewport(window), egui::ViewportCommand::Focus);
        }
        self.window_ui.rendering = self.workspace.active_window;
        self.sync_window_selection(self.workspace.active_window);
    }

    fn render_native_window(&mut self, window: Id, ui: &mut egui::Ui, record_geometry: bool) {
        let ctx = ui.ctx().clone();
        self.window_ui.rendering = window;
        self.sync_window_selection(window);
        if ui.input(|input| input.viewport().focused == Some(true)) {
            self.workspace.active_window = window;
        }
        let size = ui.input(|input| input.viewport_rect().size());
        let position = ui.input(|input| {
            input
                .viewport()
                .outer_rect
                .map(|rect| [rect.min.x, rect.min.y])
        });
        if let Some(saved) = self.workspace.window_mut(window)
            && record_geometry
            && (saved.size != [size.x, size.y] || position.is_some() && saved.position != position)
        {
            saved.size = [size.x, size.y];
            if position.is_some() {
                saved.position = position;
            }
            self.note_layout_change(&ctx);
        }
        if ui.input(|input| input.viewport().close_requested()) && !self.window_ui.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.window_ui.close_window = Some(window);
        }
        // Pointer coordinates are viewport-local, including on OS file-drop frames.
        if let Some(pos) = ui.input(|input| input.pointer.latest_pos()) {
            self.window_ui.pointers.insert(window, pos);
        }
        self.last_pointer = self.window_ui.pointers.get(&window).copied();
        if self.window_ui.close_window.is_none() {
            self.handle_keymap(ui);
            self.handle_shortcuts(ui);
            self.handle_pane_keys(ui);
        }
        if self.modal_open() || file_refs::is_open(&ctx) {
            ui.disable();
        }
        egui::Panel::top(egui::Id::new(("toolbar", window))).show(ui, |ui| self.toolbar(ui));
        let plan = layout::responsive_plan(ui.available_width());
        let hidden = ctx.data(|data| {
            data.get_temp::<bool>(egui::Id::new(("sidebar-hidden", window)))
                .unwrap_or(false)
        });
        let (sidebar_width, resized) = if plan.show_sidebar && !hidden {
            let id = egui::Id::new(("sidebar", window));
            let default_width =
                layout::default_sidebar_width(ui.input(|i| i.viewport_rect().width()));
            Self::cap_panel_width(&ctx, id, plan.sidebar_cap);
            if !self.display.sidebar_width_manual {
                Self::set_panel_width(&ctx, id, default_width);
            }
            let response = egui::Panel::left(id)
                .resizable(true)
                .default_size(if self.display.sidebar_width_manual {
                    self.display.sidebar_width as f32
                } else {
                    default_width
                })
                .min_size(layout::SIDEBAR_WIDTH_MIN as f32)
                .max_size(plan.sidebar_cap)
                .show(ui, |ui| self.sidebar(ui));
            let width = response.response.rect.width();
            (Some(width), (width - default_width).abs() > 0.5)
        } else {
            (None, false)
        };
        self.render_window_dialogs(window, &ctx);
        if let Some(root) = self
            .workspace
            .window(window)
            .map(|window| window.root.clone())
        {
            let rect = ui.available_rect_before_wrap();
            // The app has no central-panel background, so anything left unpainted
            // (notably the pane tab strip and the split-handle gutter) showed the
            // window clear color — a near-black bar that read as a stray band
            // ignoring the pane/separator edges. Lay a solid surface down first.
            ui.painter().rect_filled(rect, 0.0, ui.visuals().panel_fill);
            self.render_node(&root, rect, ui);
        }
        self.sync_window_selection(window);
        // Remember the origin of dialogs opened by a conversation body this pass.
        if (self.shared_dialog_open() || file_refs::is_open(&ctx))
            && self.window_ui.dialog_window.is_none()
        {
            self.window_ui.dialog_window = Some(window);
        }
        self.sync_display(&ctx, sidebar_width, resized);
        self.window_close_dialog(window, &ctx);
    }

    fn render_node(&mut self, node: &Node, rect: egui::Rect, ui: &mut egui::Ui) {
        match node {
            Node::Pane(pane) => self.render_group(pane.id, rect, ui),
            Node::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => {
                let horizontal = *axis == Axis::Horizontal;
                let length = if horizontal {
                    rect.width()
                } else {
                    rect.height()
                };
                let minimum = if horizontal { 240.0 } else { 180.0 };
                if length < minimum * 2.0 + 6.0 {
                    // Narrow windows retain their tree but show the focused branch.
                    let focused = self.workspace.focused_pane(self.window_ui.rendering);
                    let in_second = second.panes().iter().any(|pane| Some(pane.id) == focused);
                    self.render_node(if in_second { second } else { first }, rect, ui);
                    return;
                }
                let available = length - 6.0;
                let offset = (available * ratio).clamp(minimum, available - minimum);
                let mut a = rect;
                let mut b = rect;
                let mut handle = rect;
                if horizontal {
                    a.max.x = rect.min.x + offset;
                    handle.min.x = a.max.x;
                    handle.max.x = a.max.x + 6.0;
                    b.min.x = handle.max.x;
                } else {
                    a.max.y = rect.min.y + offset;
                    handle.min.y = a.max.y;
                    handle.max.y = a.max.y + 6.0;
                    b.min.y = handle.max.y;
                }
                let response = ui
                    .interact(handle, egui::Id::new(("split", id)), egui::Sense::drag())
                    .on_hover_cursor(if horizontal {
                        egui::CursorIcon::ResizeHorizontal
                    } else {
                        egui::CursorIcon::ResizeVertical
                    });
                ui.painter().rect_filled(
                    handle.shrink(2.0),
                    0.0,
                    ui.visuals().widgets.noninteractive.bg_stroke.color,
                );
                if response.dragged()
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    let offset = if horizontal {
                        pos.x - rect.min.x
                    } else {
                        pos.y - rect.min.y
                    };
                    if self.workspace.set_ratio(*id, offset / available) {
                        self.note_layout_change(ui.ctx());
                    }
                }
                self.render_node(first, a, ui);
                self.render_node(second, b, ui);
            }
        }
    }

    fn render_group(&mut self, pane_id: Id, rect: egui::Rect, parent: &mut egui::Ui) {
        let Some(pane) = self.workspace.pane(pane_id).cloned() else {
            return;
        };
        let mut ui = parent.new_child(
            egui::UiBuilder::new()
                .id(egui::Id::new(("group", pane_id)))
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        ui.set_clip_rect(rect.intersect(parent.clip_rect()));
        ui.set_min_width(0.0);
        ui.set_max_width(rect.width());
        if ui.input(|input| {
            input.pointer.any_pressed()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| rect.contains(pos))
        }) && !self.modal_open()
        {
            self.focus_group(pane_id, ui.ctx());
        }
        self.tab_strip(&pane, &mut ui);
        ui.separator();
        // Keep the strip out of split hit testing: drops there reorder tabs.
        let content_rect = ui.available_rect_before_wrap();
        let active = self.workspace.pane(pane_id).and_then(|pane| pane.active);
        if let Some(index) = active.and_then(|id| self.tabs.iter().position(|tab| tab.id == id)) {
            // Absolute conversation UI identity survives reparenting into another pane/window.
            let mut body = ui.new_child(
                egui::UiBuilder::new()
                    .id(egui::Id::new(("conversation", self.tabs[index].id)))
                    .max_rect(ui.available_rect_before_wrap())
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            if self.conversation_pane(index, &mut body) {
                self.note_layout_change(ui.ctx());
            }
        } else {
            ui.centered_and_justified(|ui| {
                if ui.button("+ New conversation").clicked() {
                    self.focus_group(pane_id, ui.ctx());
                    self.apply_shortcut(egui::Key::T, ui.ctx());
                }
            });
        }
        self.pane_drop_target(pane_id, content_rect, parent);
    }

    fn pane_drop_target(&mut self, pane: Id, rect: egui::Rect, ui: &mut egui::Ui) {
        let response = ui.interact(
            rect,
            egui::Id::new(("pane-drop", pane)),
            egui::Sense::hover(),
        );
        if !ui.is_enabled() || self.modal_open() || file_refs::is_open(ui.ctx()) {
            return;
        }
        let Some(payload) = response.dnd_hover_payload::<DragTab>() else {
            return;
        };
        let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let Some(mut zone) = DropZone::at(rect, pos) else {
            return;
        };
        if self
            .workspace
            .pane(pane)
            .is_some_and(|p| p.tabs.is_empty() || p.tabs == [payload.0])
        {
            zone = DropZone::Center;
        }
        let preview = zone.preview(rect);
        let color = ui.visuals().selection.stroke.color;
        ui.painter()
            .rect_filled(preview, 6.0, color.gamma_multiply(0.12));
        ui.painter().rect_stroke(
            preview,
            6.0,
            egui::Stroke::new(2.0, color),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            preview.center(),
            egui::Align2::CENTER_CENTER,
            zone.label(),
            egui::FontId::proportional(15.0),
            ui.visuals().text_color(),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if let Some(payload) = response.dnd_release_payload::<DragTab>() {
            if let Some((axis, before)) = zone.split() {
                if self
                    .workspace
                    .split_tab_into(payload.0, pane, axis, before)
                    .is_some()
                {
                    self.focus_conversation(payload.0, ui.ctx());
                }
            } else {
                self.move_conversation(payload.0, pane, usize::MAX, ui.ctx());
            }
        }
    }

    fn tab_strip(&mut self, pane: &Pane, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let reveal_id = egui::Id::new(("revealed-tab", pane.id));
        let previous = ctx.data(|data| data.get_temp::<Id>(reveal_id));
        let reveal = previous != pane.active || self.window_ui.reveal_tab == pane.active;
        // Give the row its tab height before placing the smaller controls so
        // all centers agree on the first frame, including in empty panes.
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), TAB_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.add_space(4.0);
                // Fixed controls stay visible. Conversation names scroll horizontally, never wrap.
                if icons::button(
                    ui,
                    icons::Icon::Plus,
                    "New conversation in this pane (Ctrl+T)",
                )
                .clicked()
                {
                    self.focus_group(pane.id, &ctx);
                    self.apply_shortcut(egui::Key::T, &ctx);
                }
                let pane_menu = icons::button(ui, icons::Icon::Panes, "Manage tabs and panes");
                egui::Popup::menu(&pane_menu).show(|ui| {
                    for id in &pane.tabs {
                        if let Some(tab) = self.tabs.iter().find(|tab| tab.id == *id)
                            && ui
                                .selectable_label(
                                    pane.active == Some(*id),
                                    format!("{} · {}", tab.title(), tab.navigation_status().0),
                                )
                                .clicked()
                        {
                            self.focus_conversation(*id, &ctx);
                            ui.close();
                        }
                    }
                    ui.separator();
                    if let Some(tab) = pane.active {
                        self.tab_context_menu(tab, ui);
                    }
                    ui.menu_button("Focus pane", |ui| {
                        let panes: Vec<_> = self
                            .workspace
                            .window(self.window_ui.rendering)
                            .unwrap()
                            .root
                            .panes()
                            .into_iter()
                            .enumerate()
                            .map(|(i, pane)| (i + 1, pane.id))
                            .collect();
                        for (number, id) in panes {
                            if ui.button(format!("Pane {number}")).clicked() {
                                self.focus_group(id, &ctx);
                                ui.close();
                            }
                        }
                    });
                });
                ui.add_space(4.0);
                let tab_width = ((ui.available_width()
                    - pane.tabs.len().saturating_sub(1) as f32 * 4.0)
                    / pane.tabs.len().max(1) as f32)
                    .clamp(132.0, 220.0);
                egui::ScrollArea::horizontal()
                    .id_salt(("tabs", pane.id))
                    .auto_shrink([false, true])
                    .max_height(44.0)
                    .show(ui, |ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), TAB_HEIGHT),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                for id in &pane.tabs {
                                    if self.workspace.tab_location(*id).map(|(_, id)| id)
                                        != Some(pane.id)
                                    {
                                        continue;
                                    }
                                    let Some(tab) = self.tabs.iter().find(|tab| tab.id == *id)
                                    else {
                                        continue;
                                    };
                                    let title = tab.title();
                                    let status = tab.navigation_status();
                                    let draft =
                                        !tab.composer.is_empty() || !tab.attachments.is_empty();
                                    let (response, close) = workspace_tab(
                                        ui,
                                        *id,
                                        &title,
                                        status,
                                        draft,
                                        pane.active == Some(*id),
                                        tab_width,
                                    );
                                    if reveal && pane.active == Some(*id) {
                                        response.scroll_to_me(Some(egui::Align::Center));
                                        ctx.data_mut(|data| data.insert_temp(reveal_id, *id));
                                        self.window_ui.reveal_tab = None;
                                    }
                                    if response.clicked() {
                                        self.focus_conversation(*id, &ctx);
                                    }
                                    response.dnd_set_drag_payload(DragTab(*id));
                                    let after = ui
                                        .input(|i| i.pointer.hover_pos())
                                        .is_some_and(|pos| pos.x > response.rect.center().x);
                                    if response.dnd_hover_payload::<DragTab>().is_some() {
                                        let x = if after {
                                            response.rect.right() + 27.0
                                        } else {
                                            response.rect.left()
                                        };
                                        ui.painter().vline(
                                            x,
                                            response.rect.y_range(),
                                            egui::Stroke::new(
                                                2.0,
                                                ui.visuals().selection.stroke.color,
                                            ),
                                        );
                                    }
                                    if let Some(payload) = response.dnd_release_payload::<DragTab>()
                                    {
                                        let index = pane
                                            .tabs
                                            .iter()
                                            .position(|tab| *tab == *id)
                                            .unwrap_or(0);
                                        self.move_conversation(
                                            payload.0,
                                            pane.id,
                                            index + usize::from(after),
                                            &ctx,
                                        );
                                    }
                                    response.context_menu(|ui| {
                                        self.tab_context_menu(*id, ui);
                                        ui.separator();
                                        if ui.button("Close conversation").clicked() {
                                            if let Some(index) =
                                                self.tabs.iter().position(|tab| tab.id == *id)
                                            {
                                                self.close_tab(index);
                                            }
                                            ui.close();
                                        }
                                    });
                                    if (close || response.clicked_by(egui::PointerButton::Middle))
                                        && let Some(index) =
                                            self.tabs.iter().position(|tab| tab.id == *id)
                                    {
                                        self.close_tab(index);
                                    }
                                }
                                let (_, target) = ui.allocate_exact_size(
                                    egui::vec2(32.0, TAB_HEIGHT),
                                    egui::Sense::hover(),
                                );
                                if let Some(payload) = target.dnd_release_payload::<DragTab>() {
                                    self.move_conversation(
                                        payload.0,
                                        pane.id,
                                        pane.tabs.len(),
                                        &ctx,
                                    );
                                }
                            },
                        );
                    });
            },
        );
    }

    fn render_window_dialogs(&mut self, window: Id, ctx: &egui::Context) {
        let shared = self.shared_dialog_open() || file_refs::is_open(ctx);
        if !shared {
            self.window_ui.dialog_window = None;
        }
        if shared && self.window_ui.dialog_window.is_none() {
            self.window_ui.dialog_window = Some(self.workspace.active_window);
        }
        // Native close confirmation owns input ahead of daemon key capture.
        if self.window_ui.close_window.is_some() {
            return;
        }
        if self.pending_key_request().is_some() {
            self.key_capture_dialog(ctx);
            return;
        }
        if self.window_ui.dialog_window == Some(window) {
            self.navigation_palette(ctx);
            self.command_dialog(ctx);
            self.task_dialogs(ctx);
            file_refs::dialog(ctx);
            let connected = self
                .review
                .pending
                .is_none_or(|(id, _, _)| self.tabs.iter().any(|tab| tab.id == id && tab.connected));
            self.review.check_pending(connected);
            if self.review.pending.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_secs(1));
            }
            if self.review.show(ctx, &self.theme_colors()) {
                self.request_review();
            }
            self.server_dialog(ctx);
            self.provider_dialog(ctx);
            self.config_dialog(ctx);
            self.setup_dialog(ctx);
            self.activity_dialog(ctx);
            self.process_view_dialog(ctx);
            self.job_view_dialog(ctx);
            self.stats_dialog(ctx);
            self.catalog_dialog(ctx);
        }
        if self
            .close_target
            .and_then(|id| self.workspace.tab_location(id))
            .map(|(id, _)| id)
            == Some(window)
        {
            self.close_dialog(ctx);
        }
        for tab in &mut self.tabs {
            if self.workspace.tab_location(tab.id).map(|(id, _)| id) == Some(window) {
                tab.editor_dialog(ctx);
            }
        }
    }

    fn window_close_dialog(&mut self, window: Id, ctx: &egui::Context) {
        if self.window_ui.close_window != Some(window) {
            return;
        }
        let response =
            crate::surface::modal(ctx, egui::Id::new(("close-window", window))).show(ctx, |ui| {
                ui.heading(if window == 0 {
                    "Quit Bone Desktop?"
                } else {
                    "Close this window?"
                });
                if window == 0 {
                    ui.label(
                    "All desktop windows will close. Conversations, drafts and layout are saved.",
                );
                    ui.label("Connections will be released; running work may be interrupted.");
                    if self.tabs.iter().any(|tab| !tab.queue.is_empty()) {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "Unsent queued messages are not saved on quit and will be discarded.",
                        );
                    }
                    if self.tabs.iter().any(|tab| !tab.attachments.is_empty()) {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "Unsent image attachments are not saved and will be discarded.",
                        );
                    }
                } else {
                    ui.label(
                        "Conversations move to the main window, including drafts and running work.",
                    );
                    ui.label("No conversation will be stopped or deleted.");
                }
                ui.horizontal(|ui| {
                    if ui.button("Keep open").clicked() {
                        self.window_ui.close_window = None;
                    }
                    if ui
                        .button(if window == 0 {
                            "Save and quit"
                        } else {
                            "Move conversations and close"
                        })
                        .clicked()
                    {
                        if window == 0 {
                            if self.save_workspace_now() {
                                self.window_ui.quitting = true;
                                self.window_ui.close_window = None;
                                ctx.send_viewport_cmd_to(
                                    egui::ViewportId::ROOT,
                                    egui::ViewportCommand::Close,
                                );
                            }
                        } else {
                            self.rehome_window(window, ctx);
                            self.window_ui.close_window = None;
                        }
                    }
                });
                if !self.sidebar_notice.is_empty() {
                    ui.label(&self.sidebar_notice);
                }
            });
        if response.should_close() {
            self.window_ui.close_window = None;
        }
    }

    fn rehome_window(&mut self, window: Id, ctx: &egui::Context) {
        let tabs: Vec<_> = self
            .workspace
            .window(window)
            .into_iter()
            .flat_map(|window| window.root.panes())
            .flat_map(|pane| pane.tabs.clone())
            .collect();
        let target = self.workspace.focused_pane(0).unwrap();
        for tab in tabs {
            self.workspace.move_tab(tab, target, usize::MAX);
        }
        self.workspace.remove_window(window);
        self.window_ui.pointers.remove(&window);
        if self.window_ui.dialog_window == Some(window) {
            self.window_ui.dialog_window = Some(0);
        }
        self.workspace.active_window = 0;
        self.window_ui.focus_window = Some(0);
        self.note_layout_change(ctx);
        ctx.request_repaint();
    }

    pub(crate) fn save_workspace_now(&mut self) -> bool {
        let Some(path) = &self.layout_path else {
            return true;
        };
        match layout::save(path, &self.current_layout()) {
            Ok(()) => {
                self.layout_dirty_since = None;
                true
            }
            Err(error) => {
                self.sidebar_notice = format!("Layout save failed: {error}");
                false
            }
        }
    }
}

/// A tab has one surface, with independent hit targets for its body and close
/// control. Status and draft markers never compete with title truncation.
fn workspace_tab(
    ui: &mut egui::Ui,
    id: Id,
    title: &str,
    (status, indicator, color): (&str, RowIndicator, egui::Color32),
    draft: bool,
    active: bool,
    width: f32,
) -> (egui::Response, bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, TAB_HEIGHT), egui::Sense::hover());
    let widget = egui::Id::new(("workspace-tab", id));
    let close_rect = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 27.0, rect.top() + 6.0),
        egui::pos2(rect.right() - 3.0, rect.bottom() - 6.0),
    );
    let body_rect =
        egui::Rect::from_min_max(rect.min, egui::pos2(close_rect.left(), rect.bottom()));
    let response = ui.interact(body_rect, widget, egui::Sense::click_and_drag());
    let close = ui.interact(close_rect, widget.with("close"), egui::Sense::click());
    let hovered = response.hovered() || close.hovered();
    // The active tab is marked by a lighter grey fill (its title is also
    // bold); no underline is drawn.
    let fill = if active {
        ui.visuals().extreme_bg_color
    } else if hovered {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        ui.visuals().faint_bg_color
    };
    ui.painter().rect_filled(rect, 5.0, fill);
    let center = egui::pos2(rect.left() + 14.0, rect.center().y);
    match indicator {
        RowIndicator::None => {}
        RowIndicator::Spinner => {
            let mut indicator_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id(widget.with("indicator"))
                    .max_rect(egui::Rect::from_center_size(center, egui::vec2(12.0, 12.0))),
            );
            indicator_ui.add(egui::Spinner::new().size(12.0).color(color));
        }
        RowIndicator::Queued => crate::task_row::paint_queued(ui.painter(), center, color),
        RowIndicator::Glyph(glyph) => {
            ui.painter().text(
                center,
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::proportional(14.0),
                color,
            );
        }
    }
    let title_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 28.0, rect.top()),
        egui::pos2(
            close_rect.left() - if draft { 12.0 } else { 4.0 },
            rect.bottom(),
        ),
    );
    // The full tab was allocated above. Drawing its contents must not advance
    // the parent cursor back into that allocation (overlapping the next tab).
    let mut title_ui = ui.new_child(
        egui::UiBuilder::new()
            .id(widget.with("title"))
            .max_rect(title_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    title_ui.set_clip_rect(title_rect.intersect(ui.clip_rect()));
    title_ui.set_max_width(title_rect.width());
    let text = egui::RichText::new(title).size(13.0);
    title_ui.add(
        egui::Label::new(if active { text.strong() } else { text })
            .truncate()
            .selectable(false),
    );
    if draft {
        ui.painter().circle_filled(
            egui::pos2(close_rect.left() - 7.0, rect.center().y),
            2.5,
            ui.visuals().text_color(),
        );
    }
    if close.hovered() {
        ui.painter()
            .rect_filled(close_rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
    }
    ui.painter().text(
        close_rect.center(),
        egui::Align2::CENTER_CENTER,
        "×",
        egui::FontId::proportional(16.0),
        ui.visuals().weak_text_color(),
    );
    let closed = close.on_hover_text("Close conversation").clicked();
    let hint = if draft { " · Unsent draft" } else { "" };
    (
        response.on_hover_text(format!("{title}\n{status}{hint}")),
        closed,
    )
}

fn window_name(id: Id) -> String {
    if id == 0 {
        "Main window".into()
    } else {
        format!("Window {id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        app: &mut DesktopApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.render_native_window(0, ui, false),
        )
    }

    fn fixture(ctx: &egui::Context) -> DesktopApp {
        let mut app = DesktopApp::open(ctx.clone(), false, None);
        app.tabs[0].composer = "Source draft".into();
        app.tabs[0].queue.push_back("Queued instruction".into());
        app.add_tab(crate::Intent::New, ctx);
        app.tabs[1].composer = "Target draft".into();
        settle(&mut app, ctx);
        app
    }

    fn settle(app: &mut DesktopApp, ctx: &egui::Context) {
        for _ in 0..3 {
            frame(app, ctx, vec![]).drop_without_applying_deltas();
        }
    }

    fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn drag(
        app: &mut DesktopApp,
        ctx: &egui::Context,
        tab: Id,
        target: egui::Pos2,
    ) -> egui::FullOutput {
        let start = ctx
            .read_response(egui::Id::new(("workspace-tab", tab)))
            .unwrap()
            .rect
            .center();
        frame(app, ctx, pointer(start, true)).drop_without_applying_deltas();
        frame(
            app,
            ctx,
            vec![egui::Event::PointerMoved(start + egui::vec2(12.0, 8.0))],
        )
        .drop_without_applying_deltas();
        frame(app, ctx, vec![egui::Event::PointerMoved(target)]).drop_without_applying_deltas();
        let preview = frame(app, ctx, vec![egui::Event::PointerMoved(target)]);
        assert!(
            egui::DragAndDrop::payload::<DragTab>(ctx).is_some(),
            "tab starts a real pointer drag"
        );
        frame(app, ctx, pointer(target, false)).drop_without_applying_deltas();
        preview
    }

    #[test]
    fn tab_allocations_never_overlap_with_titles_icons_or_overflow() {
        for width in [132.0, 180.0, 220.0] {
            let ctx = egui::Context::default();
            let mut rects = Vec::new();
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(320.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    rects.clear();
                    egui::ScrollArea::horizontal().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for id in 0..12 {
                                let indicator = match id % 3 {
                                    0 => RowIndicator::None,
                                    1 => RowIndicator::Spinner,
                                    _ => RowIndicator::Glyph("!"),
                                };
                                let title = if id % 2 == 0 {
                                    "Short"
                                } else {
                                    "A very long title that needs truncation"
                                };
                                let (response, _) = workspace_tab(
                                    ui,
                                    id,
                                    title,
                                    ("Status", indicator, egui::Color32::WHITE),
                                    id % 2 == 0,
                                    id == 1,
                                    width,
                                );
                                rects.push(egui::Rect::from_min_size(
                                    response.rect.min,
                                    egui::vec2(width, 36.0),
                                ));
                            }
                        });
                    });
                },
            )
            .drop_without_applying_deltas();
            assert_eq!(rects.len(), 12);
            for pair in rects.windows(2) {
                assert!(
                    pair[1].left() >= pair[0].right(),
                    "overlapping tabs: {pair:?}"
                );
                assert_eq!(pair[0].top(), pair[1].top());
            }
        }
    }

    #[test]
    fn dragging_tabs_to_each_edge_previews_and_creates_the_correct_split() {
        for zone in [
            DropZone::Left,
            DropZone::Right,
            DropZone::Top,
            DropZone::Bottom,
        ] {
            let ctx = egui::Context::default();
            let mut app = fixture(&ctx);
            let tab = app.tabs[0].id;
            let commands = app.tabs[0].commands.clone();
            let target = app.workspace.focused_pane(0).unwrap();
            let rect = ctx
                .read_response(egui::Id::new(("pane-drop", target)))
                .unwrap()
                .rect;
            let pos = match zone {
                DropZone::Left => egui::pos2(rect.left() + 10.0, rect.center().y),
                DropZone::Right => egui::pos2(rect.right() - 10.0, rect.center().y),
                DropZone::Top => egui::pos2(rect.center().x, rect.top() + 10.0),
                DropZone::Bottom => egui::pos2(rect.center().x, rect.bottom() - 10.0),
                DropZone::Center => unreachable!(),
            };
            let preview = drag(&mut app, &ctx, tab, pos);
            assert!(
                preview.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.text() == zone.label())),
                "missing {zone:?} preview"
            );
            preview.drop_without_applying_deltas();
            let Node::Split {
                axis,
                first,
                second,
                ..
            } = &app.workspace.window(0).unwrap().root
            else {
                panic!("{zone:?} did not create a split");
            };
            let (expected_axis, before) = zone.split().unwrap();
            assert_eq!(*axis, expected_axis);
            assert_eq!((if before { first } else { second }).panes()[0].tabs, [tab]);
            assert_eq!(app.workspace.active_tab(0), Some(tab));
            assert_eq!(app.tabs[0].composer, "Source draft");
            assert_eq!(
                app.tabs[0].queue.front().map(String::as_str),
                Some("Queued instruction")
            );
            assert!(app.tabs[0].commands.same_channel(&commands));
        }
    }

    #[test]
    fn dragging_to_pane_center_merges_without_an_extra_split() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let tab = app.tabs[0].id;
        app.split_conversation(app.tabs[1].id, Axis::Horizontal, &ctx);
        settle(&mut app, &ctx);
        let target = app.workspace.focused_pane(0).unwrap();
        let rect = ctx
            .read_response(egui::Id::new(("pane-drop", target)))
            .unwrap()
            .rect;
        drag(&mut app, &ctx, tab, rect.center()).drop_without_applying_deltas();
        assert_eq!(app.workspace.window(0).unwrap().root.panes().len(), 1);
        assert_eq!(
            app.workspace.pane(target).unwrap().tabs,
            [app.tabs[1].id, tab]
        );
    }

    #[test]
    fn tab_strip_drag_reorders_after_the_target_without_splitting() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let tab = app.tabs[0].id;
        let other = app.tabs[1].id;
        let rect = ctx
            .read_response(egui::Id::new(("workspace-tab", other)))
            .unwrap()
            .rect;
        drag(
            &mut app,
            &ctx,
            tab,
            egui::pos2(rect.right() - 5.0, rect.center().y),
        )
        .drop_without_applying_deltas();
        assert_eq!(app.workspace.window(0).unwrap().root.panes().len(), 1);
        let pane = app.workspace.focused_pane(0).unwrap();
        assert_eq!(app.workspace.pane(pane).unwrap().tabs, [other, tab]);
    }

    #[test]
    fn tab_close_control_targets_its_own_tab_without_selecting_the_next() {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        let tab = app.tabs[0].id;
        let active = app.workspace.active_tab(0);
        let rect = ctx
            .read_response(egui::Id::new(("workspace-tab", tab)).with("close"))
            .unwrap()
            .rect;
        frame(&mut app, &ctx, pointer(rect.center(), true)).drop_without_applying_deltas();
        frame(&mut app, &ctx, pointer(rect.center(), false)).drop_without_applying_deltas();
        assert_eq!(app.close_target, Some(tab));
        assert_eq!(app.workspace.active_tab(0), active);
    }
}
