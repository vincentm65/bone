//! Rich text blocks: what a chat view returns as `block` for the GUI to draw
//! itself (the TUI draws the view's `lines`). Paragraphs of styled spans in
//! a proportional or monospace font that the GUI wraps, in a box with
//! padding, a background, a border and rounded corners, as wide as its text
//! up to a fraction of the chat, on the left or the right. A block takes as
//! many chat rows as its height needs, so scrolling stays in rows.
//!
//! ```lua
//! { block = {
//!     paragraphs = { { spans = { { "Hello ", "Normal" }, { text = "world", bold = true } } } },
//!     font = "sans", size = 15, align = "right", max_width = 0.8,
//!     bg = "UserMessage", border = "PromptBorder", radius = 12, pad = { 8, 12 }, margin = { 6, 6 },
//!   },
//!   lines = { … } }  -- for a frontend that does not draw blocks
//! ```
//!
//! A paragraph also takes `font`, `size`, `fg` (its spans' default group),
//! `gap` (points before it), `indent`, and `bg` with `pad` (a code block's
//! box). A span is `{ text, group }` or `{ text =, hl =, bold, italic,
//! code, size }`. Sizes are points.

use std::sync::{Arc, Mutex};

use bone_ui::style::Modifier;
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, FontFamily, FontId, Galley, Stroke};
use serde_json::Value;

use crate::Gui;
use crate::view::color;

pub struct Block {
    paras: Vec<Para>,
    right: bool,
    max_width: f32,
    bg: Option<Color32>,
    border: Option<Color32>,
    radius: f32,
    /// Vertical and horizontal padding, and the space above and below.
    pad: (f32, f32),
    margin: (f32, f32),
    /// Laid out at one width: the galleys and the box's size.
    layout: Mutex<Option<Layout>>,
}

struct Para {
    job: LayoutJob,
    gap: f32,
    indent: f32,
    bg: Option<Color32>,
    pad: f32,
}

#[derive(Clone)]
struct Layout {
    width: f32,
    galleys: Vec<Arc<Galley>>,
    size: egui::Vec2,
}

/// A row of the chat: a line of text in the cell grid, or one of the rows
/// a block takes (`part` of `rows`).
#[derive(Clone)]
pub enum ChatRow {
    Text(crate::Row),
    Block {
        block: Arc<Block>,
        part: usize,
        rows: usize,
    },
}

impl ChatRow {
    /// The row as plain text (a block's on its first row).
    pub fn text(&self) -> String {
        match self {
            ChatRow::Text(r) => r.iter().map(|(t, _)| t.as_str()).collect(),
            ChatRow::Block { block, part: 0, .. } => block.text(),
            ChatRow::Block { .. } => String::new(),
        }
    }
}

/// A number, or `{ a, b }` for two.
fn pair(v: &Value, default: (f32, f32)) -> (f32, f32) {
    match v {
        Value::Number(n) => {
            let n = n.as_f64().unwrap_or(0.0) as f32;
            (n, n)
        }
        Value::Array(a) => (
            a.first()
                .and_then(Value::as_f64)
                .map_or(default.0, |n| n as f32),
            a.get(1)
                .and_then(Value::as_f64)
                .map_or(default.1, |n| n as f32),
        ),
        _ => default,
    }
}

fn num(v: &Value, default: f32) -> f32 {
    v.as_f64().map_or(default, |n| n as f32)
}

impl Gui {
    /// Group `name`'s color (foreground, or background with `bg`).
    fn group_color(&self, name: &str, bg: bool) -> Option<Color32> {
        let s = self.lua.theme.hl(name);
        if bg { s.bg } else { s.fg }.and_then(color)
    }

    /// A view's `block`, as `width` columns of chat rows; `None` (the
    /// view's lines are drawn) before there are fonts to measure with.
    pub(crate) fn block_rows(&mut self, spec: &Value, width: usize) -> Option<Vec<ChatRow>> {
        let ctx = self.repaint.get()?.clone();
        let (_, _, col, row) = self.lua.cells?;
        let block = Arc::new(self.block(spec));
        let size = block.measure(&ctx, width as f32 * col);
        let rows = ((size.y / row).ceil() as usize).max(1);
        Some(
            (0..rows)
                .map(|part| ChatRow::Block {
                    block: block.clone(),
                    part,
                    rows,
                })
                .collect(),
        )
    }

    fn block(&self, spec: &Value) -> Block {
        let font = spec["font"].as_str().unwrap_or("sans");
        let size = num(&spec["size"], 15.0);
        let fg = spec["fg"].as_str().unwrap_or("Normal");
        let paras = (spec["paragraphs"].as_array().into_iter().flatten())
            .map(|p| self.para(p, font, size, fg))
            .collect();
        let group = |k: &str, bg: bool| spec[k].as_str().and_then(|g| self.group_color(g, bg));
        Block {
            paras,
            right: spec["align"] == "right",
            max_width: num(&spec["max_width"], 1.0).clamp(0.1, 1.0),
            bg: group("bg", true),
            border: group("border", false),
            radius: num(&spec["radius"], 0.0),
            pad: pair(&spec["pad"], (0.0, 0.0)),
            margin: pair(&spec["margin"], (0.0, 0.0)),
            layout: Mutex::new(None),
        }
    }

    fn para(&self, p: &Value, font: &str, size: f32, fg: &str) -> Para {
        let font = p["font"].as_str().unwrap_or(font);
        let size = num(&p["size"], size);
        let fg = p["fg"].as_str().unwrap_or(fg);
        let mut job = LayoutJob::default();
        for s in p["spans"].as_array().into_iter().flatten() {
            let (text, group) = match s {
                Value::String(t) => (t.as_str(), fg),
                Value::Array(a) => (
                    a.first().and_then(Value::as_str).unwrap_or(""),
                    a.get(1).and_then(Value::as_str).unwrap_or(fg),
                ),
                _ => (
                    s["text"].as_str().unwrap_or(""),
                    s["hl"].as_str().unwrap_or(fg),
                ),
            };
            let style = self.lua.theme.hl(group);
            let m = style.add_modifier;
            let code = s["code"] == true;
            let family = if code || font == "mono" {
                FontFamily::Monospace
            } else {
                FontFamily::Proportional
            };
            let size = num(&s["size"], if code { size * 0.92 } else { size });
            let mut color = style.fg.and_then(color).unwrap_or(Color32::from_gray(0xee));
            if s["bold"] == true || m.contains(Modifier::BOLD) {
                // egui's bundled fonts have no bold face: bold is brighter.
                color = color.lerp_to_gamma(Color32::WHITE, 0.35);
            }
            if m.contains(Modifier::DIM) {
                color = color.gamma_multiply(0.6);
            }
            let background = if code {
                self.group_color("CodeBlock", true)
                    .unwrap_or(Color32::from_gray(0x2a))
            } else {
                style
                    .bg
                    .and_then(crate::view::color)
                    .unwrap_or(Color32::TRANSPARENT)
            };
            let underline = if m.contains(Modifier::UNDERLINED) || s["link"].is_string() {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            };
            job.append(
                text,
                0.0,
                TextFormat {
                    font_id: FontId::new(size, family),
                    color,
                    background,
                    italics: s["italic"] == true || m.contains(Modifier::ITALIC),
                    underline,
                    ..Default::default()
                },
            );
        }
        // An empty paragraph is still a line tall.
        if job.text.is_empty() {
            let family = if font == "mono" {
                FontFamily::Monospace
            } else {
                FontFamily::Proportional
            };
            job.append(
                " ",
                0.0,
                TextFormat::simple(FontId::new(size, family), Color32::TRANSPARENT),
            );
        }
        Para {
            job,
            gap: num(&p["gap"], 0.0),
            indent: num(&p["indent"], 0.0),
            bg: p["bg"].as_str().and_then(|g| self.group_color(g, true)),
            pad: num(&p["pad"], 0.0),
        }
    }
}

impl Block {
    /// The text, paragraphs on lines of their own.
    pub fn text(&self) -> String {
        let paras: Vec<&str> = self.paras.iter().map(|p| p.job.text.as_str()).collect();
        paras.join("\n")
    }

    /// Lay out at `width` points (kept for that width): the size the block
    /// takes, margins included.
    pub fn measure(&self, ctx: &egui::Context, width: f32) -> egui::Vec2 {
        let mut layout = self.layout.lock().unwrap();
        if let Some(l) = layout.as_ref().filter(|l| l.width == width) {
            return l.size;
        }
        let inner = (width * self.max_width - 2.0 * self.pad.1).max(20.0);
        let mut galleys = Vec::new();
        let (mut w, mut h) = (0.0f32, 0.0f32);
        for p in &self.paras {
            let mut job = p.job.clone();
            job.wrap.max_width = (inner - p.indent - 2.0 * p.pad).max(10.0);
            let g = ctx.fonts_mut(|f| f.layout_job(job));
            w = w.max(g.size().x + p.indent + 2.0 * p.pad);
            h += p.gap + g.size().y + 2.0 * p.pad;
            galleys.push(g);
        }
        // A box with a background spans the paragraphs' widest line; text
        // with none keeps the whole width (a code block inside still fits).
        let box_w = if self.bg.is_some() || self.border.is_some() {
            w + 2.0 * self.pad.1
        } else {
            width * self.max_width
        };
        let size = egui::vec2(
            box_w.min(width),
            h + 2.0 * self.pad.0 + self.margin.0 + self.margin.1,
        );
        *layout = Some(Layout {
            width,
            galleys,
            size,
        });
        size
    }

    /// Draw it with its top-left at `at`, in a chat `width` points wide.
    pub fn paint(&self, painter: &egui::Painter, at: egui::Pos2, width: f32) {
        let ctx = painter.ctx().clone();
        let size = self.measure(&ctx, width);
        let layout = self.layout.lock().unwrap().clone();
        let Some(layout) = layout else { return };
        let x = if self.right {
            at.x + width - size.x
        } else {
            at.x
        };
        let boxed = egui::Rect::from_min_size(
            egui::pos2(x, at.y + self.margin.0),
            egui::vec2(size.x, size.y - self.margin.0 - self.margin.1),
        );
        if let Some(bg) = self.bg {
            painter.rect_filled(boxed, self.radius, bg);
        }
        if let Some(b) = self.border {
            painter.rect_stroke(
                boxed,
                self.radius,
                Stroke::new(1.0, b),
                egui::StrokeKind::Inside,
            );
        }
        let mut y = boxed.min.y + self.pad.0;
        for (p, g) in self.paras.iter().zip(&layout.galleys) {
            y += p.gap;
            let left = boxed.min.x + self.pad.1 + p.indent;
            if let Some(bg) = p.bg {
                let r = egui::Rect::from_min_size(
                    egui::pos2(left, y),
                    egui::vec2(boxed.max.x - self.pad.1 - left, g.size().y + 2.0 * p.pad),
                );
                painter.rect_filled(r, 6.0, bg);
            }
            painter.galley(
                egui::pos2(left + p.pad, y + p.pad),
                g.clone(),
                Color32::PLACEHOLDER,
            );
            y += g.size().y + 2.0 * p.pad;
        }
    }
}
