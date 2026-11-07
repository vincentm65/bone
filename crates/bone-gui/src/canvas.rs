//! The canvas node: Lua draws on a box. Its `draw` function returns a list
//! of shapes (`bone.gui.draw.rect{…}`, `circle`, `line`, `polyline`, `text`,
//! `image`); this reads them and paints them in the box, so a custom effect,
//! a chart or a pattern is Lua with no engine change.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::Gui;
use crate::backend::text::{Family, Job, RunStyle};
use crate::backend::{Pos, Rect, Shape, pos, vec2};
use crate::node::{self, Corners, ImageSource, Rgba};
use crate::nodeview::Fx;

/// Shapes past this many in one draw are dropped.
const MAX_OPS: usize = 20_000;

pub(crate) enum Op {
    /// Rects, circles and lines, in the box's own coordinates.
    Shape(Shape),
    Text {
        at: Pos,
        text: String,
        size: f32,
        color: Rgba,
        /// Where `at` is on the text, 0 to 1 across and down.
        align: (f32, f32),
        mono: bool,
    },
    Image {
        rect: Rect,
        source: ImageSource,
        radius: f32,
    },
}

/// `shape` moved by `by`.
fn moved(shape: &Shape, by: crate::backend::Vec2) -> Shape {
    let mut s = shape.clone();
    match &mut s {
        Shape::Rect { rect, .. }
        | Shape::Stroke { rect, .. }
        | Shape::Shadow { rect, .. }
        | Shape::Gradient { rect, .. }
        | Shape::Image { rect, .. } => *rect = rect.translate(by),
        Shape::Text { at, .. } => *at += by,
        Shape::Circle { center, .. } => *center += by,
        Shape::Path { points, .. } => {
            for p in points {
                *p += by;
            }
        }
    }
    s
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
        fx: &mut Fx,
        clip: Rect,
        rect: Rect,
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
        let clip = clip.intersect(rect);
        let shift = rect.min.to_vec2();
        let mut images = Vec::new();
        for op in &d.ops {
            match op {
                Op::Shape(shape) => fx.paint(moved(shape, shift), clip),
                Op::Text {
                    at,
                    text,
                    size,
                    color,
                    align,
                    mono,
                } => {
                    let family = if *mono { Family::Mono } else { Family::Sans };
                    let shaped = fx.shaper.shape(Job::simple(
                        text.clone(),
                        RunStyle::plain(family, *size, *color),
                        None,
                    ));
                    let at = *at + shift
                        - vec2(shaped.size.x * align.0, shaped.size.y * align.1);
                    fx.paint(
                        Shape::Text {
                            at,
                            text: shaped,
                            color: None,
                        },
                        clip,
                    );
                }
                Op::Image {
                    rect,
                    source,
                    radius,
                } => images.push((rect.translate(shift), source.clone(), *radius)),
            }
        }
        for (r, source, radius) in images {
            let img = node::Image {
                source,
                fit: node::Fit::Fill,
                natural: None,
            };
            let corners = Corners {
                tl: radius,
                tr: radius,
                br: radius,
                bl: radius,
            };
            self.image_ui(fx, clip, r, corners, &img);
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
        let color = |k: &str, bg: bool| -> Option<Rgba> {
            let s = o.get(k)?.as_str()?;
            node::color(self, s, bg)
        };
        let width = num("width").or_else(|| num("stroke_width")).unwrap_or(1.0);
        let stroke = |k: &str| color(k, false).map(|c| (width, c));
        let point = |p: &Value| -> Option<Pos> {
            let a = p.as_array()?;
            Some(pos(a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32))
        };
        let white = Rgba(0xff, 0xff, 0xff, 0xff);
        Some(match o.get("op")?.as_str()? {
            "rect" => {
                let rect = Rect::from_xywh(num("x")?, num("y")?, num("w")?, num("h")?);
                let r = num("radius").unwrap_or(0.0);
                let corners = Corners {
                    tl: r,
                    tr: r,
                    br: r,
                    bl: r,
                };
                let fill = color("fill", true);
                let line = stroke("stroke");
                // One rect for its fill and stroke: a fill, its outline over it.
                match (fill, line) {
                    (Some(fill), None) => Op::Shape(Shape::Rect {
                        rect,
                        corners,
                        fill,
                    }),
                    (fill, Some((width, c))) => {
                        if let Some(fill) = fill {
                            return Some(Op::Shape(Shape::Path {
                                points: rounded(rect, r),
                                closed: true,
                                stroke: Some((width, c)),
                                fill: Some(fill),
                            }));
                        }
                        Op::Shape(Shape::Stroke {
                            rect,
                            corners,
                            width,
                            color: c,
                            outside: false,
                        })
                    }
                    (None, None) => return None,
                }
            }
            "circle" => Op::Shape(Shape::Circle {
                center: pos(num("x")?, num("y")?),
                radius: num("r")?,
                fill: color("fill", true),
                stroke: stroke("stroke"),
            }),
            "line" => Op::Shape(Shape::Path {
                points: vec![pos(num("x1")?, num("y1")?), pos(num("x2")?, num("y2")?)],
                closed: false,
                stroke: Some((width, color("color", false).unwrap_or(white))),
                fill: None,
            }),
            "polyline" => {
                let points: Vec<Pos> = o
                    .get("points")?
                    .as_array()?
                    .iter()
                    .filter_map(point)
                    .collect();
                let fill = color("fill", true).filter(|_| points.len() >= 3);
                Op::Shape(Shape::Path {
                    points,
                    closed: o.get("closed").and_then(Value::as_bool).unwrap_or(false),
                    stroke: Some((width, color("color", false).unwrap_or(white))),
                    fill,
                })
            }
            "text" => Op::Text {
                at: pos(num("x")?, num("y")?),
                text: o.get("text")?.as_str()?.to_owned(),
                size: num("size").unwrap_or(14.0),
                color: color("color", false).unwrap_or(white),
                align: (
                    match o.get("align").and_then(Value::as_str) {
                        Some("center") => 0.5,
                        Some("right") => 1.0,
                        _ => 0.0,
                    },
                    match o.get("baseline").and_then(Value::as_str) {
                        Some("middle") => 0.5,
                        _ => 0.0,
                    },
                ),
                mono: o.get("font").and_then(Value::as_str) == Some("mono"),
            },
            "image" => Op::Image {
                rect: Rect::from_xywh(num("x")?, num("y")?, num("w")?, num("h")?),
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

/// The outline of `r` with corners of radius `radius`, clockwise.
fn rounded(r: Rect, radius: f32) -> Vec<Pos> {
    use std::f32::consts::{FRAC_PI_2, PI};
    let rad = radius.min(r.width() / 2.0).min(r.height() / 2.0).max(0.0);
    if rad <= 0.5 {
        return vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()];
    }
    let mut out = Vec::new();
    for (cx, cy, start) in [
        (r.min.x + rad, r.min.y + rad, PI),
        (r.max.x - rad, r.min.y + rad, 1.5 * PI),
        (r.max.x - rad, r.max.y - rad, 0.0),
        (r.min.x + rad, r.max.y - rad, FRAC_PI_2),
    ] {
        let steps = ((rad / 3.0).ceil() as usize).clamp(3, 10);
        for i in 0..=steps {
            let a = start + FRAC_PI_2 * i as f32 / steps as f32;
            out.push(pos(cx + a.cos() * rad, cy + a.sin() * rad));
        }
    }
    out
}
