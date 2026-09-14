//! Shared presentation for utility screens and focused dialogs.
use eframe::egui::{self, vec2};

pub fn frame(ctx: &egui::Context) -> egui::Frame {
    let style = ctx.style_of(ctx.theme());
    egui::Frame::popup(&style)
        // Utility screens are information dense. Keep a comfortable outer
        // gutter, while letting their own toolbars and cards provide the
        // visual rhythm inside.
        .inner_margin(18)
        .corner_radius(crate::theme::SURFACE_RADIUS)
        .fill(style.visuals.window_fill)
        // A drop shadow lifts the dialog off the transcript so it reads as a
        // foreground surface rather than a recolored region of the window.
        .shadow(egui::Shadow {
            offset: [0, 12],
            blur: 32,
            spread: 0,
            color: egui::Color32::from_black_alpha(110),
        })
}

/// Fill for a grouped content card. Slightly lighter than the dialog surface so
/// cards read as raised panels sitting on top of the dialog, not recessed wells.
pub fn card_fill(visuals: &egui::Visuals) -> egui::Color32 {
    visuals
        .window_fill
        .lerp_to_gamma(visuals.text_color(), 0.035)
}

/// A grouped content card: one raised surface holding a run of related rows.
/// Rows inside should be separated with `ui.separator()` rather than given a
/// card each, so a page reads as a short list instead of a stack of boxes.
pub fn card(ui: &egui::Ui) -> egui::Frame {
    let visuals = ui.visuals();
    egui::Frame::new()
        .fill(card_fill(visuals))
        .stroke(egui::Stroke::new(
            1.0,
            visuals
                .widgets
                .noninteractive
                .bg_stroke
                .color
                .gamma_multiply(0.55),
        ))
        .corner_radius(egui::CornerRadius::same(crate::theme::CONTROL_RADIUS))
        .inner_margin(egui::Margin::symmetric(12, 8))
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
            ui.spacing_mut().item_spacing = vec2(8.0, 7.0);
            ui.spacing_mut().button_padding = vec2(11.0, 6.0);
            ui.spacing_mut().interact_size.y = crate::theme::CONTROL_HEIGHT;
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
            if self.scroll || size.y < 500.0 || size.x < 600.0 {
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
    let narrow = ui.available_width() < 420.0;
    let mut trailing = Some(trailing);
    ui.horizontal_wrapped(|ui| {
        for label in ["Settings", "Plugins", "Usage"] {
            if tab(ui, label == active, label).clicked() && label != active {
                selected = Some(label);
            }
        }
        if !narrow {
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                trailing.take().unwrap(),
            );
        }
    });
    if let Some(trailing) = trailing {
        // Bound the trailing row to a single control height. A bare
        // `with_layout` seeds the child's `min_rect` at the vertical centre of
        // the remaining panel (right_to_left + Align::Center), so advancing the
        // parent cursor past it would drop all body content to the middle of
        // the panel — even when the trailing closure renders nothing.
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), crate::theme::CONTROL_HEIGHT),
            egui::Layout::right_to_left(egui::Align::Center),
            trailing,
        );
    }
    ui.add_space(8.0);
    selected
}

/// Navigation uses an underline so it reads differently from an action button.
pub fn tab(ui: &mut egui::Ui, selected: bool, label: impl Into<String>) -> egui::Response {
    let color = if selected {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let response = ui.add(
        egui::Button::new(egui::RichText::new(label.into()).color(color))
            .frame(false)
            .min_size(vec2(0.0, crate::theme::CONTROL_HEIGHT)),
    );
    if selected {
        let rect = response.rect.shrink2(vec2(10.0, 0.0));
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(2.0, ui.visuals().hyperlink_color),
        );
    }
    response
}

pub fn divider(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, height), egui::Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
}
