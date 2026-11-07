//! What a frame paints: a list of simple shapes, each with the clip it is
//! drawn in and how opaque, for the window's renderer to draw in order.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::geom::{Pos, Rect, Vec2};
use super::text::Shaped;
use crate::node::{Corners, Gradient, Rgba};

/// Decoded pixels to draw: straight (not premultiplied) RGBA, row by row.
pub struct Picture {
    /// Unique for the life of the process, for a renderer's texture cache.
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Picture {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Picture {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            rgba,
        }
    }

    pub fn size(&self) -> Vec2 {
        Vec2 {
            x: self.width as f32,
            y: self.height as f32,
        }
    }
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Picture({}, {}x{})", self.id, self.width, self.height)
    }
}

#[derive(Clone, Debug)]
pub enum Shape {
    /// A filled box.
    Rect {
        rect: Rect,
        corners: Corners,
        fill: Rgba,
    },
    /// A box's outline, inside its edge or outside it.
    Stroke {
        rect: Rect,
        corners: Corners,
        width: f32,
        color: Rgba,
        outside: bool,
    },
    /// A box's shadow, under it.
    Shadow {
        rect: Rect,
        corners: Corners,
        offset: Vec2,
        blur: f32,
        spread: f32,
        color: Rgba,
    },
    Gradient {
        rect: Rect,
        corners: Corners,
        gradient: Gradient,
    },
    /// Laid-out text with its top left at `at`; `color` over the runs' own.
    Text {
        at: Pos,
        text: Arc<Shaped>,
        color: Option<Rgba>,
    },
    /// A picture in `rect`, the part `uv` of it (0 to 1).
    Image {
        rect: Rect,
        uv: Rect,
        corners: Corners,
        picture: Arc<Picture>,
    },
    Circle {
        center: Pos,
        radius: f32,
        fill: Option<Rgba>,
        stroke: Option<(f32, Rgba)>,
    },
    /// A line through `points`, closed or not; a fill is the polygon under
    /// it (convex).
    Path {
        points: Vec<Pos>,
        closed: bool,
        stroke: Option<(f32, Rgba)>,
        fill: Option<Rgba>,
    },
}

#[derive(Clone, Debug)]
pub struct Item {
    pub shape: Shape,
    /// Nothing is drawn outside it.
    pub clip: Rect,
    /// Every color's alpha is multiplied by it.
    pub opacity: f32,
}

/// The frame's shapes, in painting order.
#[derive(Default, Debug)]
pub struct DrawList {
    pub items: Vec<Item>,
}

impl DrawList {
    pub fn push(&mut self, shape: Shape, clip: Rect, opacity: f32) {
        if opacity <= 0.0 || !clip.is_positive() {
            return;
        }
        self.items.push(Item {
            shape,
            clip,
            opacity,
        });
    }

    /// The text painted, each with the box it covers.
    pub fn texts(&self) -> impl Iterator<Item = (&str, Rect)> {
        self.items.iter().filter_map(|i| match &i.shape {
            Shape::Text { at, text, .. } => Some((text.text.as_str(), text.rect_at(*at))),
            _ => None,
        })
    }
}

/// The color `c` at `opacity`.
pub fn faded(c: Rgba, opacity: f32) -> Rgba {
    if opacity >= 1.0 {
        return c;
    }
    Rgba(c.0, c.1, c.2, (f32::from(c.3) * opacity).round() as u8)
}
