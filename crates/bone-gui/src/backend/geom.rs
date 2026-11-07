//! Points, vectors and rectangles in logical pixels, as the node view uses
//! them, with no renderer's types.

use std::ops::{Add, AddAssign, Mul, Neg, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

pub const fn vec2(x: f32, y: f32) -> Vec2 {
    Vec2 { x, y }
}

pub const fn pos(x: f32, y: f32) -> Pos {
    Pos { x, y }
}

impl Vec2 {
    pub const ZERO: Vec2 = vec2(0.0, 0.0);

    pub fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    pub fn dot(self, o: Vec2) -> f32 {
        self.x * o.x + self.y * o.y
    }
}

impl Pos {
    pub const ZERO: Pos = pos(0.0, 0.0);

    pub fn to_vec2(self) -> Vec2 {
        vec2(self.x, self.y)
    }

    pub fn distance(self, o: Pos) -> f32 {
        (self - o).length()
    }
}

impl Add<Vec2> for Pos {
    type Output = Pos;
    fn add(self, v: Vec2) -> Pos {
        pos(self.x + v.x, self.y + v.y)
    }
}

impl AddAssign<Vec2> for Pos {
    fn add_assign(&mut self, v: Vec2) {
        self.x += v.x;
        self.y += v.y;
    }
}

impl Sub<Vec2> for Pos {
    type Output = Pos;
    fn sub(self, v: Vec2) -> Pos {
        pos(self.x - v.x, self.y - v.y)
    }
}

impl Sub for Pos {
    type Output = Vec2;
    fn sub(self, o: Pos) -> Vec2 {
        vec2(self.x - o.x, self.y - o.y)
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        vec2(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        vec2(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, k: f32) -> Vec2 {
        vec2(self.x * k, self.y * k)
    }
}

impl Neg for Vec2 {
    type Output = Vec2;
    fn neg(self) -> Vec2 {
        vec2(-self.x, -self.y)
    }
}

/// From `min` (top left) to `max` (bottom right).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub min: Pos,
    pub max: Pos,
}

impl Rect {
    pub const NOTHING: Rect = Rect {
        min: pos(f32::INFINITY, f32::INFINITY),
        max: pos(f32::NEG_INFINITY, f32::NEG_INFINITY),
    };
    pub const EVERYTHING: Rect = Rect {
        min: pos(f32::NEG_INFINITY, f32::NEG_INFINITY),
        max: pos(f32::INFINITY, f32::INFINITY),
    };

    pub fn from_min_max(min: Pos, max: Pos) -> Rect {
        Rect { min, max }
    }

    pub fn from_min_size(min: Pos, size: Vec2) -> Rect {
        Rect {
            min,
            max: min + size,
        }
    }

    pub fn from_xywh(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos(x, y), vec2(w, h))
    }

    pub fn from_center_size(center: Pos, size: Vec2) -> Rect {
        Rect::from_min_size(center - size * 0.5, size)
    }

    pub fn from_two_pos(a: Pos, b: Pos) -> Rect {
        Rect {
            min: pos(a.x.min(b.x), a.y.min(b.y)),
            max: pos(a.x.max(b.x), a.y.max(b.y)),
        }
    }

    pub fn width(&self) -> f32 {
        self.max.x - self.min.x
    }

    pub fn height(&self) -> f32 {
        self.max.y - self.min.y
    }

    pub fn size(&self) -> Vec2 {
        vec2(self.width(), self.height())
    }

    pub fn center(&self) -> Pos {
        pos(
            (self.min.x + self.max.x) / 2.0,
            (self.min.y + self.max.y) / 2.0,
        )
    }

    /// Positive width and height: something of it shows.
    pub fn is_positive(&self) -> bool {
        self.width() > 0.0 && self.height() > 0.0
    }

    pub fn contains(&self, p: Pos) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }

    pub fn intersect(&self, o: Rect) -> Rect {
        Rect {
            min: pos(self.min.x.max(o.min.x), self.min.y.max(o.min.y)),
            max: pos(self.max.x.min(o.max.x), self.max.y.min(o.max.y)),
        }
    }

    pub fn intersects(&self, o: Rect) -> bool {
        self.min.x <= o.max.x
            && o.min.x <= self.max.x
            && self.min.y <= o.max.y
            && o.min.y <= self.max.y
    }

    pub fn union(&self, o: Rect) -> Rect {
        Rect {
            min: pos(self.min.x.min(o.min.x), self.min.y.min(o.min.y)),
            max: pos(self.max.x.max(o.max.x), self.max.y.max(o.max.y)),
        }
    }

    pub fn expand(&self, by: f32) -> Rect {
        self.expand2(vec2(by, by))
    }

    pub fn expand2(&self, by: Vec2) -> Rect {
        Rect {
            min: self.min - by,
            max: self.max + by,
        }
    }

    pub fn translate(&self, by: Vec2) -> Rect {
        Rect {
            min: self.min + by,
            max: self.max + by,
        }
    }

    pub fn left_top(&self) -> Pos {
        self.min
    }

    pub fn right_top(&self) -> Pos {
        pos(self.max.x, self.min.y)
    }

    pub fn left_bottom(&self) -> Pos {
        pos(self.min.x, self.max.y)
    }

    pub fn right_bottom(&self) -> Pos {
        self.max
    }
}

impl From<crate::flex::Rect> for Rect {
    fn from(r: crate::flex::Rect) -> Rect {
        Rect::from_xywh(r.x, r.y, r.w, r.h)
    }
}
