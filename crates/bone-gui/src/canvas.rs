//! The canvas node: Lua draws on a box. Its `draw` function returns a list
//! of shapes (`bone.gui.draw.rect{…}`, `circle`, `line`, `polyline`, `text`,
//! `image`); this reads them and paints them in the box, so a custom effect,
//! a chart or a pattern is Lua with no engine change.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontFamily, FontId, Pos2, Shape, Stroke};
use serde_json::Value;

use crate::Gui;
use crate::node::{self, ImageSource};

/// Shapes past this many in one draw are dropped.
const MAX_OPS: usize = 20_000;

pub(crate) enum Op {
    /// Rects, circles and lines, as egui draws them, in the box's own
    /// coordinates.
    Shape(Shape),
    Text {
        at: Pos2,
        text: String,
        size: f32,
        color: Color32,
        align: Align2,
        mono: bool,
    },
    Image {
        rect: egui::Rect,
        source: ImageSource,
        radius: f32,
    },
}

/// A canvas's last draw, kept until it is due again.
pub(crate) struct Drawn {
    size: (f32, f32),
    at: Instant,
    ops: Vec<Op>,
}

#[derive(Default)]
pub(crate) struct Canvases {
    drawn: HashMap<String, Drawn>,
    /// The canvases whose draw failed, so an error is said once.
    failed: std::collections::HashSet<String>,
    started: Option<Instant>,
}

impl Canvases {
    /// Forget what was drawn: Lua built the tree again.
    pub fn clear(&mut self) {
        self.drawn.clear();
    }
}

impl Gui {
    /// Paint canvas `key` (draw function `draw`, redrawn `fps` times a
    /// second, or once) in `rect`; true while it wants another frame.
    pub(crate) fn canvas_ui(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        rect: egui::Rect,
        key: &str,
        draw: u64,
        fps: f32,
    ) -> bool {
        let size = (rect.width(), rect.height());
        let now = Instant::now();
        let started = *self.view.bound.canvases.started.get_or_insert(now);
        // A frame up to half a period early is on time: frames come with
        // the display, a little either side of 1/fps, and a strict test
        // draws every other one (or every two, then three).
        let due = match self.view.bound.canvases.drawn.get(key) {
            None => true,
            Some(d) => {
                d.size != size
                    || (fps > 0.0 && now.duration_since(d.at) >= Duration::from_secs_f32(0.5 / fps))
            }
        };
        if due {
            let ops = self.run_draw(key, draw, size, now.duration_since(started).as_secs_f64());
            let drawn = &mut self.view.bound.canvases.drawn;
            drawn.insert(key.to_owned(), Drawn { size, at: now, ops });
        }
        let Some(d) = self.view.bound.canvases.drawn.get(key) else {
            return false;
        };
        let painter = painter.with_clip_rect(painter.clip_rect().intersect(rect));
        let shift = rect.min.to_vec2();
        for op in &d.ops {
            match op {
                Op::Shape(shape) => {
                    let mut shape = shape.clone();
                    shape.translate(shift);
                    painter.add(shape);
                }
                Op::Text {
                    at,
                    text,
                    size,
                    color,
                    align,
                    mono,
                } => {
                    let family = if *mono {
                        FontFamily::Monospace
                    } else {
                        FontFamily::Proportional
                    };
                    painter.text(
                        *at + shift,
                        *align,
                        text,
                        FontId::new(*size, family),
                        *color,
                    );
                }
                Op::Image {
                    rect,
                    source,
                    radius,
                } => {
                    let corners = egui::CornerRadius::same(radius.clamp(0.0, 255.0) as u8);
                    let img = node::Image {
                        source: source.clone(),
                        fit: node::Fit::Fill,
                        natural: None,
                    };
                    self.image_ui(ui, &painter, rect.translate(shift), corners, &img);
                }
            }
        }
        fps > 0.0
    }

    /// Call the draw function and read what it returned.
    fn run_draw(&mut self, key: &str, draw: u64, size: (f32, f32), time: f64) -> Vec<Op> {
        let Some(table) = self.view.sink_table() else {
            return Vec::new();
        };
        let r = self.with_api(|lua| {
            let Some(f) = table.raw_get::<Option<mlua::Function>>(draw)? else {
                return Ok(Value::Null);
            };
            let ctx = lua.create_table()?;
            ctx.set("width", size.0)?;
            ctx.set("height", size.1)?;
            ctx.set("time", time)?;
            let v: mlua::Value = f.call(ctx)?;
            bone_lua::from_lua(&v)
        });
        match r {
            Ok(v) => {
                self.view.bound.canvases.failed.remove(key);
                self.read_ops(&v)
            }
            Err(e) => {
                if self.view.bound.canvases.failed.insert(key.to_owned()) {
                    bone_ui::lua::Host::lua_error(self, "canvas", &e);
                }
                Vec::new()
            }
        }
    }

    fn read_ops(&self, v: &Value) -> Vec<Op> {
        let list: Vec<&Value> = match v {
            Value::Array(a) => a.iter().collect(),
            // A table with holes comes back as an object keyed "1", "2"…
            Value::Object(o) => {
                let mut keys: Vec<(i64, &Value)> = o
                    .iter()
                    .filter_map(|(k, v)| k.parse::<i64>().ok().map(|k| (k, v)))
                    .collect();
                keys.sort_by_key(|(k, _)| *k);
                keys.into_iter().map(|(_, v)| v).collect()
            }
            _ => Vec::new(),
        };
        list.into_iter()
            .take(MAX_OPS)
            .filter_map(|o| self.read_op(o))
            .collect()
    }

    fn read_op(&self, o: &Value) -> Option<Op> {
        let num = |k: &str| o.get(k).and_then(Value::as_f64).map(|n| n as f32);
        let color = |k: &str, bg: bool| -> Option<Color32> {
            let s = o.get(k)?.as_str()?;
            node::color(self, s, bg).map(|c| Color32::from_rgba_unmultiplied(c.0, c.1, c.2, c.3))
        };
        let width = num("width").or_else(|| num("stroke_width")).unwrap_or(1.0);
        let stroke = |k: &str| color(k, false).map(|c| Stroke::new(width, c));
        let point = |p: &Value| -> Option<Pos2> {
            let a = p.as_array()?;
            Some(Pos2::new(
                a.first()?.as_f64()? as f32,
                a.get(1)?.as_f64()? as f32,
            ))
        };
        Some(match o.get("op")?.as_str()? {
            "rect" => Op::Shape(Shape::Rect(egui::epaint::RectShape::new(
                egui::Rect::from_min_size(
                    Pos2::new(num("x")?, num("y")?),
                    egui::vec2(num("w")?, num("h")?),
                ),
                num("radius").unwrap_or(0.0),
                color("fill", true).unwrap_or(Color32::TRANSPARENT),
                stroke("stroke").unwrap_or(Stroke::NONE),
                egui::StrokeKind::Inside,
            ))),
            "circle" => Op::Shape(Shape::Circle(egui::epaint::CircleShape {
                center: Pos2::new(num("x")?, num("y")?),
                radius: num("r")?,
                fill: color("fill", true).unwrap_or(Color32::TRANSPARENT),
                stroke: stroke("stroke").unwrap_or(Stroke::NONE),
            })),
            "line" => line(
                vec![
                    Pos2::new(num("x1")?, num("y1")?),
                    Pos2::new(num("x2")?, num("y2")?),
                ],
                Stroke::new(width, color("color", false).unwrap_or(Color32::WHITE)),
                false,
                None,
            ),
            "polyline" => line(
                o.get("points")?
                    .as_array()?
                    .iter()
                    .filter_map(point)
                    .collect(),
                Stroke::new(width, color("color", false).unwrap_or(Color32::WHITE)),
                o.get("closed").and_then(Value::as_bool).unwrap_or(false),
                color("fill", true),
            ),
            "text" => Op::Text {
                at: Pos2::new(num("x")?, num("y")?),
                text: o.get("text")?.as_str()?.to_owned(),
                size: num("size").unwrap_or(14.0),
                color: color("color", false).unwrap_or(Color32::WHITE),
                align: match (
                    o.get("align").and_then(Value::as_str),
                    o.get("baseline").and_then(Value::as_str),
                ) {
                    (Some("center"), Some("middle")) => Align2::CENTER_CENTER,
                    (Some("center"), _) => Align2::CENTER_TOP,
                    (Some("right"), Some("middle")) => Align2::RIGHT_CENTER,
                    (Some("right"), _) => Align2::RIGHT_TOP,
                    (_, Some("middle")) => Align2::LEFT_CENTER,
                    _ => Align2::LEFT_TOP,
                },
                mono: o.get("font").and_then(Value::as_str) == Some("mono"),
            },
            "image" => Op::Image {
                rect: egui::Rect::from_min_size(
                    Pos2::new(num("x")?, num("y")?),
                    egui::vec2(num("w")?, num("h")?),
                ),
                source: match (
                    o.get("id").and_then(Value::as_str),
                    o.get("path").and_then(Value::as_str),
                ) {
                    (Some(id), _) => ImageSource::Attachment(id.to_owned()),
                    (None, Some(p)) => ImageSource::Path(p.to_owned()),
                    _ => return None,
                },
                radius: num("radius").unwrap_or(0.0),
            },
            _ => return None,
        })
    }
}

/// A line through `points`, closed or not, with the polygon filled under it
/// when it has a fill and room for one.
fn line(points: Vec<Pos2>, stroke: Stroke, closed: bool, fill: Option<Color32>) -> Op {
    let mut shapes = Vec::new();
    if let (Some(f), true) = (fill, points.len() >= 3) {
        shapes.push(Shape::convex_polygon(points.clone(), f, Stroke::NONE));
    }
    shapes.push(if closed {
        Shape::closed_line(points, stroke)
    } else {
        Shape::line(points, stroke)
    });
    Op::Shape(Shape::Vec(shapes))
}
