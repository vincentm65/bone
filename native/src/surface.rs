//! Shared presentation for utility screens and focused dialogs.
use eframe::egui::{self, vec2};

pub fn frame(ctx: &egui::Context) -> egui::Frame {
    let style = ctx.style_of(ctx.theme());
    egui::Frame::popup(&style)
        .inner_margin(22)
        .corner_radius(12)
        .fill(style.visuals.panel_fill)
}

pub fn modal(ctx: &egui::Context, id: egui::Id) -> egui::Modal {
    egui::Modal::new(id)
        .frame(frame(ctx))
        .backdrop_color(egui::Color32::from_black_alpha(140))
}

pub struct Surface<'a> {
    id: egui::Id,
    title: &'a str,
    description: &'a str,
    size: egui::Vec2,
    scroll: bool,
}

impl<'a> Surface<'a> {
    pub fn new(title: &'a str, description: &'a str) -> Self {
        Self {
            id: egui::Id::new(("utility", title)),
            title,
            description,
            size: vec2(520.0, 640.0),
            scroll: true,
        }
    }

    pub fn id(mut self, id: egui::Id) -> Self {
        self.id = id;
        self
    }

    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.size = vec2(width, height);
        self
    }

    /// Content with its own scrolling keeps search, navigation and actions fixed.
    pub fn body_scroll(mut self, scroll: bool) -> Self {
        self.scroll = scroll;
        self
    }

    pub fn show<R>(
        self,
        ctx: &egui::Context,
        open: &mut bool,
        content: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        let bounds = ctx.content_rect().size() - vec2(72.0, 72.0);
        let size = self.size.min(bounds).max(vec2(120.0, 100.0));
        let mut close = false;
        let response = modal(ctx, self.id).show(ctx, |ui| {
            ui.set_width(size.x);
            ui.set_max_height(size.y);
            if !self.scroll {
                ui.set_min_height(size.y);
            }
            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
            ui.spacing_mut().button_padding = vec2(12.0, 7.0);
            ui.spacing_mut().interact_size.y = 34.0;
            ui.horizontal(|ui| {
                ui.heading(self.title);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = crate::icons::button(ui, crate::icons::Icon::Close, "Close (Esc)")
                        .clicked();
                });
            });
            if !self.description.is_empty() {
                ui.weak(self.description);
            }
            ui.add_space(2.0);
            ui.separator();
            // Keep the frame's measured width above small text/scrollbar rounding
            // changes. Without a fixed floor, fractional display scales can make
            // a centered modal move by a pixel as its children are re-laid out.
            ui.set_min_width(size.x + 8.0);
            let height = ui.available_height().max(48.0);
            if self.scroll || size.y < 500.0 {
                egui::ScrollArea::vertical()
                    .id_salt("surface-body")
                    .max_height(height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        // On very short windows the whole body can scroll, so
                        // navigation and actions remain reachable.
                        if !self.scroll {
                            ui.set_max_height(560.0);
                        }
                        content(ui)
                    })
                    .inner
            } else {
                ui.allocate_ui_with_layout(
                    vec2(size.x, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_max_size(vec2(size.x, height));
                        content(ui)
                    },
                )
                .inner
            }
        });
        #[cfg(test)]
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("last-surface-rect"), response.response.rect)
        });
        if close || response.should_close() {
            *open = false;
        }
        response.inner
    }
}

pub fn empty(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.add_space(28.0);
    ui.vertical_centered(|ui| {
        ui.strong(title);
        ui.weak(detail);
    });
    ui.add_space(28.0);
}

pub fn primary(ui: &mut egui::Ui, label: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Button::new(label).fill(ui.visuals().selection.bg_fill))
}

/// Utilities share a navigation bar; switching preserves each screen's state.
pub fn navigation(
    ui: &mut egui::Ui,
    active: &str,
    trailing: impl FnOnce(&mut egui::Ui),
) -> Option<&'static str> {
    let mut selected = None;
    ui.horizontal(|ui| {
        for label in ["Settings", "Catalog", "Usage"] {
            if ui.selectable_label(label == active, label).clicked() && label != active {
                selected = Some(label);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), trailing);
    });
    ui.add_space(4.0);
    selected
}

pub fn divider(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, height), egui::Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
}
