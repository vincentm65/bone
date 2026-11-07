//! Borders and cursor icons for the painter.

use crate::backend::{Cursor, Rect, Shape, vec2};
use crate::node::{BorderStyle, Corners, Edges, Rgba};

use super::Fx;

pub(in crate::nodeview) fn cursor_icon(name: &str) -> Option<Cursor> {
    Some(match name {
        "default" => Cursor::Default,
        "none" => Cursor::None,
        "pointer" => Cursor::PointingHand,
        "text" => Cursor::Text,
        "vertical-text" => Cursor::VerticalText,
        "grab" => Cursor::Grab,
        "grabbing" => Cursor::Grabbing,
        "move" => Cursor::Move,
        "all-scroll" => Cursor::AllScroll,
        "not-allowed" => Cursor::NotAllowed,
        "no-drop" => Cursor::NoDrop,
        "wait" => Cursor::Wait,
        "progress" => Cursor::Progress,
        "crosshair" => Cursor::Crosshair,
        "cell" => Cursor::Cell,
        "help" => Cursor::Help,
        "copy" => Cursor::Copy,
        "alias" => Cursor::Alias,
        "zoom-in" => Cursor::ZoomIn,
        "zoom-out" => Cursor::ZoomOut,
        "ew-resize" => Cursor::ResizeHorizontal,
        "ns-resize" => Cursor::ResizeVertical,
        "nesw-resize" => Cursor::ResizeNeSw,
        "nwse-resize" => Cursor::ResizeNwSe,
        "col-resize" => Cursor::ResizeColumn,
        "row-resize" => Cursor::ResizeRow,
        _ => return None,
    })
}

/// The four borders: one stroke when they are alike and solid, else a
/// strip or a dashed line for each side.
pub(in crate::nodeview) fn draw_border(
    fx: &mut Fx,
    clip: Rect,
    r: Rect,
    corners: Corners,
    b: Edges,
    style: BorderStyle,
    color: Rgba,
) {
    let alike = b.top == b.right && b.right == b.bottom && b.bottom == b.left;
    if alike && style == BorderStyle::Solid {
        fx.paint(
            Shape::Stroke {
                rect: r,
                corners,
                width: b.top,
                color,
                outside: false,
            },
            clip,
        );
        return;
    }
    let sides = [
        (b.top, r.left_top(), r.right_top(), vec2(0.0, 0.5)),
        (b.right, r.right_top(), r.right_bottom(), vec2(-0.5, 0.0)),
        (b.bottom, r.right_bottom(), r.left_bottom(), vec2(0.0, -0.5)),
        (b.left, r.left_bottom(), r.left_top(), vec2(0.5, 0.0)),
    ];
    for (w, from, to, inward) in sides {
        if w <= 0.0 {
            continue;
        }
        let shift = inward * w;
        let (from, to) = (from + shift, to + shift);
        match style {
            BorderStyle::Solid => {
                let strip = Rect::from_two_pos(from, to).expand2(vec2(
                    if from.y == to.y { 0.0 } else { w / 2.0 },
                    if from.x == to.x { 0.0 } else { w / 2.0 },
                ));
                fx.paint(
                    Shape::Rect {
                        rect: strip,
                        corners: Corners::default(),
                        fill: color,
                    },
                    clip,
                );
            }
            BorderStyle::Dashed | BorderStyle::Dotted => {
                let (dash, gap) = if style == BorderStyle::Dashed {
                    (w * 3.0, w * 2.0)
                } else {
                    (w, w * 1.5)
                };
                let len = (to - from).length();
                if len <= 0.0 {
                    continue;
                }
                let dir = (to - from) * (1.0 / len);
                let mut at = 0.0;
                while at < len {
                    let end = (at + dash).min(len);
                    fx.paint(
                        Shape::Path {
                            points: vec![from + dir * at, from + dir * end],
                            closed: false,
                            stroke: Some((w, color)),
                            fill: None,
                        },
                        clip,
                    );
                    at = end + gap;
                }
            }
        }
    }
}
