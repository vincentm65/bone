//! Shared task and agent row widgets.
use eframe::egui;

/// Leading status indicator drawn before a sidebar row's title: an animated
/// spinner while the conversation's turn is live and running, a check while it
/// is waiting on the user, or a static glyph for other states.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RowIndicator {
    None,
    Spinner,
    Queued,
    Glyph(&'static str),
}

/// A bounded, full-width task target with left-aligned, truncated text. The
/// optional leading `indicator` is drawn in `color`; `text` carries its own tint.
pub(crate) fn task_row(
    ui: &mut egui::Ui,
    indicator: RowIndicator,
    color: egui::Color32,
    text: egui::RichText,
    selected: bool,
) -> egui::Response {
    let label = text.text().to_owned();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::click());
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() {
        ui.painter()
            .rect_filled(rect, crate::theme::CONTROL_RADIUS, visuals.bg_fill);
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            crate::theme::CONTROL_RADIUS,
            egui::Stroke::new(1.0, ui.visuals().hyperlink_color),
            egui::StrokeKind::Inside,
        );
    }
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(4.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            match indicator {
                RowIndicator::None => {}
                RowIndicator::Spinner => {
                    ui.add(egui::Spinner::new().size(12.0).color(color));
                    ui.add_space(2.0);
                }
                RowIndicator::Queued => {
                    let (icon, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    paint_queued(ui.painter(), icon.center(), color);
                    ui.add_space(2.0);
                }
                RowIndicator::Glyph(glyph) => {
                    ui.add(
                        egui::Label::new(egui::RichText::new(glyph).color(color).strong())
                            .selectable(false),
                    );
                    ui.add_space(2.0);
                }
            }
            // The row owns clicks (including its context menu), not text selection.
            ui.add(egui::Label::new(text).truncate().selectable(false));
        },
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), &label)
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Font-independent clock used by queued work in either task surface.
pub(crate) fn paint_queued(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.0, color);
    painter.circle_stroke(center, 5.0, stroke);
    painter.line_segment([center + egui::vec2(0.0, -3.0), center], stroke);
    painter.line_segment([center, center + egui::vec2(2.5, 1.0)], stroke);
}
