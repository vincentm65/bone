//! Small vector controls: independent of the installed font's symbol coverage.
use eframe::egui::{self, Response, Sense, Stroke, Ui, pos2, vec2};

#[derive(Clone, Copy)]
pub enum Icon {
    Close,
    Sidebar,
    Plus,
    Panes,
    ChevronRight,
    ChevronDown,
}

pub fn button(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    let pane_control = matches!(icon, Icon::Plus | Icon::Panes);
    let size = if pane_control {
        vec2(32.0, 32.0)
    } else {
        vec2(26.0, 24.0)
    };
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tooltip)
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let menu_open = matches!(icon, Icon::Panes)
            && egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
        if pane_control
            || response.hovered()
            || response.has_focus()
            || matches!(icon, Icon::Sidebar)
        {
            let fill = if menu_open {
                ui.visuals().widgets.active.weak_bg_fill
            } else {
                visuals.weak_bg_fill
            };
            ui.painter().rect_filled(rect, 6.0, fill);
        }
        let stroke = Stroke::new(1.5, visuals.fg_stroke.color);
        let r = rect.shrink2(vec2(6.0, 5.0));
        match icon {
            Icon::Close => {
                let c = rect.center();
                ui.painter()
                    .line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], stroke);
                ui.painter()
                    .line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], stroke);
            }
            Icon::Plus => {
                let center = rect.center();
                ui.painter()
                    .line_segment([center - vec2(6.0, 0.0), center + vec2(6.0, 0.0)], stroke);
                ui.painter()
                    .line_segment([center - vec2(0.0, 6.0), center + vec2(0.0, 6.0)], stroke);
            }
            Icon::Panes => {
                let panes = egui::Rect::from_center_size(rect.center(), vec2(16.0, 14.0));
                ui.painter()
                    .rect_stroke(panes, 2.0, stroke, egui::StrokeKind::Inside);
                ui.painter().line_segment(
                    [
                        pos2(panes.center().x, panes.top()),
                        pos2(panes.center().x, panes.bottom()),
                    ],
                    stroke,
                );
            }
            Icon::Sidebar => {
                ui.painter()
                    .rect_stroke(r, 1.0, stroke, egui::StrokeKind::Inside);
                let x = r.left() + 4.0;
                ui.painter()
                    .line_segment([pos2(x, r.top()), pos2(x, r.bottom())], stroke);
            }
            Icon::ChevronRight | Icon::ChevronDown => {
                let c = r.center();
                let points = if matches!(icon, Icon::ChevronDown) {
                    [
                        c + vec2(-4.0, -2.0),
                        c + vec2(0.0, 2.0),
                        c + vec2(4.0, -2.0),
                    ]
                } else {
                    [
                        c + vec2(-2.0, -4.0),
                        c + vec2(2.0, 0.0),
                        c + vec2(-2.0, 4.0),
                    ]
                };
                ui.painter().line_segment([points[0], points[1]], stroke);
                ui.painter().line_segment([points[1], points[2]], stroke);
            }
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect,
                4.0,
                ui.visuals().selection.stroke,
                egui::StrokeKind::Inside,
            );
        }
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tooltip)
}
