//! Borders, corner radii, gradients and cursor icons for the painter.

use eframe::egui::{self, Color32, CursorIcon, Stroke, StrokeKind};

use crate::node::{BorderStyle, Corners, Edges, Gradient};

pub(in crate::nodeview) fn cursor_icon(name: &str) -> Option<CursorIcon> {
    Some(match name {
        "default" => CursorIcon::Default,
        "none" => CursorIcon::None,
        "pointer" => CursorIcon::PointingHand,
        "text" => CursorIcon::Text,
        "vertical-text" => CursorIcon::VerticalText,
        "grab" => CursorIcon::Grab,
        "grabbing" => CursorIcon::Grabbing,
        "move" => CursorIcon::Move,
        "all-scroll" => CursorIcon::AllScroll,
        "not-allowed" => CursorIcon::NotAllowed,
        "no-drop" => CursorIcon::NoDrop,
        "wait" => CursorIcon::Wait,
        "progress" => CursorIcon::Progress,
        "crosshair" => CursorIcon::Crosshair,
        "cell" => CursorIcon::Cell,
        "help" => CursorIcon::Help,
        "copy" => CursorIcon::Copy,
        "alias" => CursorIcon::Alias,
        "zoom-in" => CursorIcon::ZoomIn,
        "zoom-out" => CursorIcon::ZoomOut,
        "ew-resize" => CursorIcon::ResizeHorizontal,
        "ns-resize" => CursorIcon::ResizeVertical,
        "nesw-resize" => CursorIcon::ResizeNeSw,
        "nwse-resize" => CursorIcon::ResizeNwSe,
        "col-resize" => CursorIcon::ResizeColumn,
        "row-resize" => CursorIcon::ResizeRow,
        _ => return None,
    })
}

pub(in crate::nodeview) fn to_corners(c: Corners) -> egui::CornerRadius {
    let u = |v: f32| v.clamp(0.0, 255.0).round() as u8;
    egui::CornerRadius {
        nw: u(c.tl),
        ne: u(c.tr),
        se: u(c.br),
        sw: u(c.bl),
    }
}

/// The four borders: one stroke when they are alike and solid, else a
/// strip or a dashed line for each side.
pub(in crate::nodeview) fn draw_border(
    painter: &egui::Painter,
    r: egui::Rect,
    corners: egui::CornerRadius,
    b: Edges,
    style: BorderStyle,
    color: Color32,
) {
    let alike = b.top == b.right && b.right == b.bottom && b.bottom == b.left;
    if alike && style == BorderStyle::Solid {
        painter.rect_stroke(r, corners, Stroke::new(b.top, color), StrokeKind::Inside);
        return;
    }
    let sides = [
        (b.top, r.left_top(), r.right_top(), egui::vec2(0.0, 0.5)),
        (
            b.right,
            r.right_top(),
            r.right_bottom(),
            egui::vec2(-0.5, 0.0),
        ),
        (
            b.bottom,
            r.right_bottom(),
            r.left_bottom(),
            egui::vec2(0.0, -0.5),
        ),
        (b.left, r.left_bottom(), r.left_top(), egui::vec2(0.5, 0.0)),
    ];
    for (w, from, to, inward) in sides {
        if w <= 0.0 {
            continue;
        }
        let shift = inward * w;
        let (from, to) = (from + shift, to + shift);
        match style {
            BorderStyle::Solid => {
                let strip = egui::Rect::from_two_pos(from, to).expand2(egui::vec2(
                    if from.y == to.y { 0.0 } else { w / 2.0 },
                    if from.x == to.x { 0.0 } else { w / 2.0 },
                ));
                painter.rect_filled(strip, 0.0, color);
            }
            BorderStyle::Dashed | BorderStyle::Dotted => {
                let (dash, gap) = if style == BorderStyle::Dashed {
                    (w * 3.0, w * 2.0)
                } else {
                    (w, w * 1.5)
                };
                painter.extend(egui::Shape::dashed_line(
                    &[from, to],
                    Stroke::new(w, color),
                    dash,
                    gap,
                ));
            }
        }
    }
}

/// A rounded box filled with a linear gradient. The outline is cut into
/// slabs between the stops (each is one straight blend), and each slab is a
/// fan of triangles colored by where its corners fall along the gradient's
/// line, so the stops come out exact.
pub(in crate::nodeview) fn gradient_shape(
    r: egui::Rect,
    radius: Corners,
    g: &Gradient,
) -> egui::Shape {
    use std::f32::consts::{FRAC_PI_2, PI};
    let mut points: Vec<egui::Pos2> = Vec::new();
    // Clockwise from the top-left: each corner's arc, its center and
    // starting angle.
    let arcs = [
        (
            radius.tl,
            egui::pos2(r.min.x + radius.tl, r.min.y + radius.tl),
            PI,
        ),
        (
            radius.tr,
            egui::pos2(r.max.x - radius.tr, r.min.y + radius.tr),
            1.5 * PI,
        ),
        (
            radius.br,
            egui::pos2(r.max.x - radius.br, r.max.y - radius.br),
            0.0,
        ),
        (
            radius.bl,
            egui::pos2(r.min.x + radius.bl, r.max.y - radius.bl),
            FRAC_PI_2,
        ),
    ];
    for (rad, center, start) in arcs {
        if rad <= 0.5 {
            points.push(center);
            continue;
        }
        let steps = ((rad / 3.0).ceil() as usize).clamp(3, 10);
        for i in 0..=steps {
            let a = start + FRAC_PI_2 * i as f32 / steps as f32;
            points.push(center + egui::vec2(a.cos(), a.sin()) * rad);
        }
    }
    let angle = g.angle.to_radians();
    let dir = egui::vec2(angle.sin(), -angle.cos());
    let length = ((r.width() * angle.sin()).abs() + (r.height() * angle.cos()).abs()).max(1.0);
    let middle = r.center();
    // Where along the gradient (0 to 1) a point falls.
    let along = |p: egui::Pos2| (p - middle).dot(dir) / length + 0.5;
    let color_at = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        let (mut lo, mut hi) = (g.stops[0], g.stops[g.stops.len() - 1]);
        for w in g.stops.windows(2) {
            if t >= w[0].0 && t <= w[1].0 {
                (lo, hi) = (w[0], w[1]);
                break;
            }
        }
        let k = if hi.0 > lo.0 {
            ((t - lo.0) / (hi.0 - lo.0)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * k).round() as u8;
        Color32::from_rgba_unmultiplied(
            mix(lo.1.0, hi.1.0),
            mix(lo.1.1, hi.1.1),
            mix(lo.1.2, hi.1.2),
            mix(lo.1.3, hi.1.3),
        )
    };
    // The slabs: from 0 to 1 cut at each stop between.
    let mut cuts: Vec<f32> = vec![0.0];
    cuts.extend(g.stops.iter().map(|s| s.0).filter(|t| *t > 0.0 && *t < 1.0));
    cuts.push(1.0);
    cuts.sort_by(|a, b| a.total_cmp(b));
    cuts.dedup();
    let mut mesh = egui::Mesh::default();
    for w in cuts.windows(2) {
        let (t0, t1) = (
            if w[0] == 0.0 { f32::MIN } else { w[0] },
            if w[1] == 1.0 { f32::MAX } else { w[1] },
        );
        let slab = clip_between(&points, &along, t0, t1);
        if slab.len() < 3 {
            continue;
        }
        let first = mesh.vertices.len() as u32;
        for p in &slab {
            mesh.colored_vertex(*p, color_at(along(*p)));
        }
        for i in 1..slab.len() as u32 - 1 {
            mesh.add_triangle(first, first + i, first + i + 1);
        }
    }
    egui::Shape::mesh(mesh)
}

/// The part of a convex polygon where `along` is within `[lo, hi]`.
fn clip_between(
    poly: &[egui::Pos2],
    along: &impl Fn(egui::Pos2) -> f32,
    lo: f32,
    hi: f32,
) -> Vec<egui::Pos2> {
    let keep = |poly: Vec<egui::Pos2>, edge: f32, above: bool| {
        let mut out = Vec::new();
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (ta, tb) = (along(a) - edge, along(b) - edge);
            let (ina, inb) = (
                if above { ta >= 0.0 } else { ta <= 0.0 },
                if above { tb >= 0.0 } else { tb <= 0.0 },
            );
            if ina {
                out.push(a);
            }
            if ina != inb && (ta - tb).abs() > f32::EPSILON {
                out.push(a + (b - a) * (ta / (ta - tb)));
            }
        }
        out
    };
    let mut p = poly.to_vec();
    if lo != f32::MIN {
        p = keep(p, lo, true);
    }
    if hi != f32::MAX {
        p = keep(p, hi, false);
    }
    p
}
