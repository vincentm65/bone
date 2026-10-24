//! The node tree's layout: a small flexbox over logical pixels. One line
//! per container, or several for a wrapping row (`flex_wrap`); `grow` and
//! `shrink`, `gap`, `justify` and `align` (`align_self`), `order`, auto
//! margins, min/max sizes, percentages, aspect ratios, absolute children,
//! margins, padding and borders. Text is measured through [`Metrics`], so
//! the solver knows nothing about egui.

use crate::node::{
    Align, Dim, Direction, Edges, Image, Input, Justify, Kind, Node, Overflow, Text, TextInput,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// How big text is: `max_width` is where it wraps (`None`: never).
pub trait Metrics {
    fn text(&self, text: &Text, max_width: Option<f32>) -> Size;
    /// The prompt field: as wide as it may be, as tall as its text.
    fn input(&self, input: &Input, max_width: Option<f32>) -> Size;
    /// A text field: as wide as it may be, as tall as its rows.
    fn text_input(&self, input: &TextInput, max_width: Option<f32>) -> Size;
    /// A picture: its own size, scaled down to the width on offer.
    fn image(&self, image: &Image, max_width: Option<f32>) -> Size {
        let Some((w, h)) = image.natural else {
            return Size::default();
        };
        let w = max_width.map_or(w, |m| w.min(m));
        Size {
            w,
            h: w * h / image.natural.map_or(1.0, |n| n.0),
        }
    }
}

/// A node with the border box the solver gave it (margins excluded).
pub struct Placed<'a> {
    pub node: &'a Node,
    pub rect: Rect,
    /// Where the children go: `rect` less border and padding.
    pub inner: Rect,
    /// The children's height in a scroll container (at least `inner`'s).
    pub content: Option<f32>,
    pub children: Vec<Placed<'a>>,
}

fn resolve(d: Dim, base: f32) -> Option<f32> {
    match d {
        Dim::Auto => None,
        Dim::Px(p) => Some(p),
        Dim::Pct(p) => Some(base * p),
        Dim::Mix { pct, px } => Some(base * pct + px),
    }
}

fn clamp(v: f32, min: Dim, max: Dim, base: f32) -> f32 {
    let v = resolve(max, base).map_or(v, |m| v.min(m));
    resolve(min, base).map_or(v, |m| v.max(m)).max(0.0)
}

fn inset(n: &Node) -> Edges {
    let (p, b) = (n.style.padding, n.style.border);
    Edges {
        top: p.top + b.top,
        right: p.right + b.right,
        bottom: p.bottom + b.bottom,
        left: p.left + b.left,
    }
}

/// The border box `n` wants when `avail` is the width on offer (`None`:
/// as much as it likes).
pub fn natural(n: &Node, avail: Option<f32>, m: &dyn Metrics) -> Size {
    measure(n, avail, false, m)
}

/// The size `n` has when its border box is `width` wide: its height is
/// what is asked (`width` is not room on offer, so a maximum is not applied
/// to it again).
pub fn natural_at(n: &Node, width: f32, m: &dyn Metrics) -> Size {
    measure(n, Some(width), true, m)
}

fn measure(n: &Node, avail: Option<f32>, definite: bool, m: &dyn Metrics) -> Size {
    let s = &n.style;
    let inset = inset(n);
    let explicit_w = match (definite, avail) {
        (true, Some(w)) => Some(w),
        _ => resolve(s.width, avail.unwrap_or(0.0)).filter(|_| {
            // A percentage of nothing is automatic.
            avail.is_some() || matches!(s.width, Dim::Px(_))
        }),
    };
    // Text wraps where it is allowed to end: the width on offer, or less
    // when the node has a maximum.
    let limit = resolve(s.max_width, avail.unwrap_or(0.0))
        .filter(|_| !definite && (avail.is_some() || matches!(s.max_width, Dim::Px(_))));
    let content_avail = explicit_w
        .or(match (avail, limit) {
            (Some(a), Some(l)) => Some(a.min(l)),
            (a, l) => a.or(l),
        })
        .map(|w| (w - inset.horizontal()).max(0.0));
    let content = match &n.kind {
        Kind::Text(t) => m.text(t, content_avail.filter(|_| t.wrap)),
        Kind::Input(i) => m.input(i, content_avail),
        Kind::Image(i) => m.image(i, content_avail),
        Kind::TextInput(t) => m.text_input(t, content_avail),
        Kind::List(l) if l.fit => Size { w: 0.0, h: l.total },
        Kind::List(_) | Kind::Field(_) | Kind::Canvas(_) => Size::default(),
        Kind::Box => {
            let sizes: Vec<(Size, Edges)> = n
                .children
                .iter()
                .filter(|c| !c.style.absolute)
                .map(|c| {
                    let mg = c.style.margin;
                    let a = content_avail.map(|w| (w - mg.horizontal()).max(0.0));
                    (natural(c, a, m), mg)
                })
                .collect();
            let gap = s.main_gap();
            let outer = |(z, mg): &(Size, Edges)| (z.w + mg.horizontal(), z.h + mg.vertical());
            match s.direction {
                Direction::Row if s.flex_wrap && content_avail.is_some() => {
                    // Lines as wide as there is room for, one under another.
                    let room = content_avail.unwrap_or(0.0);
                    let (mut width, mut height) = (0.0f32, 0.0f32);
                    let (mut line_w, mut line_h, mut lines) = (0.0f32, 0.0f32, 0);
                    for x in &sizes {
                        let (w, h) = outer(x);
                        if line_w > 0.0 && line_w + gap + w > room {
                            width = width.max(line_w);
                            height += line_h;
                            (line_w, line_h, lines) = (0.0, 0.0, lines + 1);
                        }
                        line_w += if line_w > 0.0 { gap } else { 0.0 } + w;
                        line_h = line_h.max(h);
                    }
                    if line_w > 0.0 || sizes.is_empty() {
                        width = width.max(line_w);
                        height += line_h;
                        lines += 1;
                    }
                    Size {
                        w: width,
                        h: height + s.cross_gap() * (lines as f32 - 1.0).max(0.0),
                    }
                }
                Direction::Row => Size {
                    w: sizes.iter().map(|x| outer(x).0).sum::<f32>()
                        + gap * sizes.len().saturating_sub(1) as f32,
                    h: sizes.iter().map(|x| outer(x).1).fold(0.0, f32::max),
                },
                Direction::Column => Size {
                    w: sizes.iter().map(|x| outer(x).0).fold(0.0, f32::max),
                    h: sizes.iter().map(|x| outer(x).1).sum::<f32>()
                        + gap * sizes.len().saturating_sub(1) as f32,
                },
            }
        }
    };
    let base_w = avail.unwrap_or(0.0);
    let explicit_h = resolve(s.height, 0.0).filter(|_| matches!(s.height, Dim::Px(_)));
    let mut w = explicit_w.unwrap_or(content.w + inset.horizontal());
    let mut h = explicit_h.unwrap_or(content.h + inset.vertical());
    if let Some(ratio) = s.aspect_ratio.filter(|r| *r > 0.0) {
        match (explicit_w.or(avail.filter(|_| definite)), explicit_h) {
            (Some(known), None) => h = known / ratio,
            (None, Some(known)) => w = known * ratio,
            _ => {}
        }
    }
    Size {
        w: clamp(w, s.min_width, s.max_width, base_w),
        h: clamp(h, s.min_height, s.max_height, 0.0),
    }
}

/// Lay `n` out in the border box `rect`.
pub fn place<'a>(n: &'a Node, rect: Rect, m: &dyn Metrics) -> Placed<'a> {
    let s = &n.style;
    let pad = inset(n);
    let mut inner = Rect {
        x: rect.x + pad.left,
        y: rect.y + pad.top,
        w: (rect.w - pad.horizontal()).max(0.0),
        h: (rect.h - pad.vertical()).max(0.0),
    };
    let viewport = inner;
    let row = s.direction == Direction::Row;
    let mut flow: Vec<&Node> = n.children.iter().filter(|c| !c.style.absolute).collect();
    flow.sort_by_key(|c| c.style.order);
    // A scroll container lays its children out at their full length.
    let scroll = s.overflow == Overflow::Scroll;
    let mut content = None;
    if scroll && row {
        inner.w = inner.w.max(content_width(n, m));
        content = Some(inner.w);
    } else if scroll {
        let h = match &n.kind {
            Kind::List(l) => l.total,
            _ => content_height(n, inner.w, m),
        };
        inner.h = inner.h.max(h);
        content = Some(inner.h);
    }

    // Each child's border box: the main size is flexed below, the cross
    // size follows from it.
    struct Item<'a> {
        node: &'a Node,
        mg: Edges,
        auto: [bool; 4],
        align: Align,
        w: f32,
        h: f32,
    }
    let ratio = |cs: &crate::node::Style, w: f32, h: f32| match cs.aspect_ratio {
        Some(r) if r > 0.0 && cs.height == Dim::Auto => (w, w / r),
        Some(r) if r > 0.0 && cs.width == Dim::Auto => (h * r, h),
        _ => (w, h),
    };
    let mut items: Vec<Item> = flow
        .iter()
        .copied()
        .map(|c| {
            let (cs, mg) = (&c.style, c.style.margin);
            let align = cs.align_self.unwrap_or(s.align);
            let room_w = (inner.w - mg.horizontal()).max(0.0);
            let (w, h);
            if row {
                w = clamp(
                    resolve(cs.width, inner.w).unwrap_or_else(|| natural(c, Some(room_w), m).w),
                    cs.min_width,
                    cs.max_width,
                    inner.w,
                );
                h = 0.0;
            } else {
                let auto_w = cs.margin_auto[1] || cs.margin_auto[3];
                w = clamp(
                    resolve(cs.width, inner.w).unwrap_or(match align {
                        Align::Stretch if !auto_w => room_w,
                        _ => natural(c, Some(room_w), m).w,
                    }),
                    cs.min_width,
                    cs.max_width,
                    inner.w,
                );
                let nat = clamp(
                    resolve(cs.height, inner.h).unwrap_or_else(|| natural_at(c, w, m).h),
                    cs.min_height,
                    cs.max_height,
                    inner.h,
                );
                (_, h) = ratio(cs, w, nat);
                return Item {
                    node: c,
                    mg,
                    auto: cs.margin_auto,
                    align,
                    w,
                    h,
                };
            }
            Item {
                node: c,
                mg,
                auto: cs.margin_auto,
                align,
                w,
                h,
            }
        })
        .collect();

    let main_of = |i: &Item| if row { i.w } else { i.h };
    let margin_main = |i: &Item| {
        if row {
            i.mg.horizontal()
        } else {
            i.mg.vertical()
        }
    };
    let room = if row { inner.w } else { inner.h };
    let (main_gap, cross_gap) = (s.main_gap(), s.cross_gap());

    // The lines: one, or for a wrapping row as many as the room needs.
    let mut lines: Vec<std::ops::Range<usize>> = Vec::new();
    if s.flex_wrap && row {
        let (mut start, mut used) = (0, 0.0);
        for (k, i) in items.iter().enumerate() {
            let need = i.w + i.mg.horizontal();
            if k > start && used + main_gap + need > room {
                lines.push(start..k);
                (start, used) = (k, need);
            } else {
                used += if k > start { main_gap } else { 0.0 } + need;
            }
        }
        lines.push(start..items.len());
    } else {
        lines.push(0..items.len());
    }

    let mut children: Vec<Placed> = Vec::with_capacity(items.len());
    let mut cross_cursor = if row { inner.y } else { inner.x };
    for line in lines {
        // Flex along the main axis.
        let slice = &mut items[line.clone()];
        let count = slice.len();
        let gaps = main_gap * count.saturating_sub(1) as f32;
        let used: f32 = slice.iter().map(|i| main_of(i) + margin_main(i)).sum();
        let free = room - used - gaps;
        let grow: f32 = slice.iter().map(|i| i.node.style.grow).sum();
        let weight = |i: &Item| {
            let cs = &i.node.style;
            let auto = if row { cs.width } else { cs.height } == Dim::Auto;
            cs.shrink.unwrap_or(if auto { 1.0 } else { 0.0 }) * main_of(i)
        };
        let shrink: f32 = slice.iter().map(weight).sum();
        let deltas: Vec<f32> = slice
            .iter()
            .map(|i| {
                if free > 0.0 && grow > 0.0 {
                    free * i.node.style.grow / grow
                } else if free < 0.0 && shrink > 0.0 {
                    free * weight(i) / shrink
                } else {
                    0.0
                }
            })
            .collect();
        for (i, d) in slice.iter_mut().zip(deltas) {
            let cs = &i.node.style;
            if row {
                i.w = clamp(i.w + d, cs.min_width, cs.max_width, inner.w);
            } else {
                i.h = clamp(i.h + d, cs.min_height, cs.max_height, inner.h);
            }
        }
        // A row's heights are known only now that its widths are final.
        let wrapped = s.flex_wrap && row;
        if row {
            let natural_h: Vec<f32> = slice
                .iter()
                .map(|i| {
                    let cs = &i.node.style;
                    let nat = clamp(
                        resolve(cs.height, inner.h).unwrap_or_else(|| natural_at(i.node, i.w, m).h),
                        cs.min_height,
                        cs.max_height,
                        inner.h,
                    );
                    ratio(cs, i.w, nat).1
                })
                .collect();
            // The line is as tall as its tallest, or as the box when alone.
            let line_h = if wrapped {
                slice
                    .iter()
                    .zip(&natural_h)
                    .map(|(i, h)| h + i.mg.vertical())
                    .fold(0.0, f32::max)
            } else {
                inner.h
            };
            for (i, nat) in slice.iter_mut().zip(natural_h) {
                let cs = &i.node.style;
                let stretches = i.align == Align::Stretch
                    && cs.height == Dim::Auto
                    && cs.aspect_ratio.is_none()
                    && !(i.auto[0] || i.auto[2]);
                i.h = if stretches {
                    clamp(
                        (line_h - i.mg.vertical()).max(0.0),
                        cs.min_height,
                        cs.max_height,
                        inner.h,
                    )
                } else {
                    nat
                };
            }
        }
        let cross_size = if row {
            if wrapped {
                slice
                    .iter()
                    .map(|i| i.h + i.mg.vertical())
                    .fold(0.0, f32::max)
            } else {
                inner.h
            }
        } else {
            inner.w
        };

        // Along the main axis: auto margins take the free space, else
        // `justify` places it.
        let used: f32 = slice.iter().map(|i| main_of(i) + margin_main(i)).sum();
        let left = (room - used - gaps).max(0.0);
        let autos: usize = slice
            .iter()
            .map(|i| {
                let (a, b) = if row {
                    (i.auto[3], i.auto[1])
                } else {
                    (i.auto[0], i.auto[2])
                };
                usize::from(a) + usize::from(b)
            })
            .sum();
        let (offset, extra) = if autos > 0 {
            (0.0, 0.0)
        } else {
            match s.justify {
                Justify::Start => (0.0, 0.0),
                Justify::Center => (left / 2.0, 0.0),
                Justify::End => (left, 0.0),
                Justify::Between if count > 1 => (0.0, left / (count - 1) as f32),
                Justify::Between => (0.0, 0.0),
            }
        };
        let auto_share = if autos > 0 { left / autos as f32 } else { 0.0 };
        let mut cursor = if row { inner.x } else { inner.y } + offset;
        for i in slice.iter() {
            let (a_start, a_end) = if row {
                (i.auto[3], i.auto[1])
            } else {
                (i.auto[0], i.auto[2])
            };
            let (start, end) = if row {
                (i.mg.left, i.mg.right)
            } else {
                (i.mg.top, i.mg.bottom)
            };
            let start = start + if a_start { auto_share } else { 0.0 };
            let end = end + if a_end { auto_share } else { 0.0 };
            let (c_start, c_end, size, c_auto) = if row {
                (i.mg.top, i.mg.bottom, i.h, (i.auto[0], i.auto[2]))
            } else {
                (i.mg.left, i.mg.right, i.w, (i.auto[3], i.auto[1]))
            };
            let free_cross = (cross_size - size - c_start - c_end).max(0.0);
            let cross = match c_auto {
                (true, true) => cross_cursor + c_start + free_cross / 2.0,
                (true, false) => cross_cursor + c_start + free_cross,
                (false, true) => cross_cursor + c_start,
                (false, false) => match i.align {
                    Align::Stretch | Align::Start => cross_cursor + c_start,
                    Align::Center => cross_cursor + c_start + free_cross / 2.0,
                    Align::End => cross_cursor + c_start + free_cross,
                },
            };
            let main = cursor + start;
            let r = if row {
                Rect {
                    x: main,
                    y: cross,
                    w: i.w,
                    h: i.h,
                }
            } else {
                Rect {
                    x: cross,
                    y: main,
                    w: i.w,
                    h: i.h,
                }
            };
            children.push(place(i.node, r, m));
            cursor = main + main_of(i) + end + main_gap + extra;
        }
        cross_cursor += cross_size + cross_gap;
    }
    // Out of the flow, over the rest.
    let mut over: Vec<&Node> = n.children.iter().filter(|c| c.style.absolute).collect();
    over.sort_by_key(|c| c.style.z);
    // Behind the flow (a negative `z`) or over it.
    let mut behind = 0;
    for c in over {
        let placed = place(c, anchored(c, rect, m), m);
        if c.style.z < 0 {
            children.insert(behind, placed);
            behind += 1;
        } else {
            children.push(placed);
        }
    }
    Placed {
        node: n,
        rect,
        inner: if scroll {
            // The room the children scroll in, not their length.
            Rect {
                w: if row { viewport.w } else { inner.w },
                h: if row { inner.h } else { viewport.h },
                ..inner
            }
        } else {
            inner
        },
        content,
        children,
    }
}

/// Where an absolute node goes in its parent's border box `p`: `left` and
/// `right` (or `top` and `bottom`) together give its size; one alone puts
/// its edge there; neither puts it at the start.
fn anchored(c: &Node, p: Rect, m: &dyn Metrics) -> Rect {
    let s = &c.style;
    let at = |d: Dim, whole: f32| resolve(d, whole);
    let (l, r) = (at(s.left, p.w), at(s.right, p.w));
    let (t, b) = (at(s.top, p.h), at(s.bottom, p.h));
    let want_w = resolve(s.width, p.w).or(match (l, r) {
        (Some(l), Some(r)) => Some((p.w - l - r).max(0.0)),
        _ => None,
    });
    let w = clamp(
        want_w.unwrap_or_else(|| natural(c, Some(p.w - l.unwrap_or(0.0) - r.unwrap_or(0.0)), m).w),
        s.min_width,
        s.max_width,
        p.w,
    );
    let want_h = resolve(s.height, p.h).or(match (t, b) {
        (Some(t), Some(b)) => Some((p.h - t - b).max(0.0)),
        _ => None,
    });
    let h = clamp(
        want_h.unwrap_or_else(|| natural_at(c, w, m).h),
        s.min_height,
        s.max_height,
        p.h,
    );
    let x = match (l, r) {
        (Some(l), _) => p.x + l,
        (None, Some(r)) => p.x + p.w - r - w,
        _ => p.x,
    };
    let y = match (t, b) {
        (Some(t), _) => p.y + t,
        (None, Some(b)) => p.y + p.h - b - h,
        _ => p.y,
    };
    Rect { x, y, w, h }
}

/// The height a column's children take at width `w`.
fn content_height(n: &Node, w: f32, m: &dyn Metrics) -> f32 {
    let s = &n.style;
    let heights: f32 = n
        .children
        .iter()
        .filter(|c| !c.style.absolute)
        .map(|c| {
            let (cs, mg) = (&c.style, c.style.margin);
            let room = (w - mg.horizontal()).max(0.0);
            let cw = clamp(
                resolve(cs.width, w).unwrap_or(match s.align {
                    Align::Stretch => room,
                    _ => natural(c, Some(room), m).w,
                }),
                cs.min_width,
                cs.max_width,
                w,
            );
            let ch = clamp(
                resolve(cs.height, 0.0)
                    .filter(|_| matches!(cs.height, Dim::Px(_)))
                    .unwrap_or_else(|| natural_at(c, cw, m).h),
                cs.min_height,
                cs.max_height,
                0.0,
            );
            ch + mg.vertical()
        })
        .sum();
    let flowing = n.children.iter().filter(|c| !c.style.absolute).count();
    heights + s.main_gap() * flowing.saturating_sub(1) as f32
}

/// The width a row's children take side by side.
fn content_width(n: &Node, m: &dyn Metrics) -> f32 {
    let s = &n.style;
    let flowing: Vec<&Node> = n.children.iter().filter(|c| !c.style.absolute).collect();
    let widths: f32 = flowing
        .iter()
        .map(|c| {
            let cs = &c.style;
            let w = resolve(cs.width, 0.0)
                .filter(|_| matches!(cs.width, Dim::Px(_)))
                .unwrap_or_else(|| natural(c, None, m).w);
            clamp(w, cs.min_width, cs.max_width, 0.0) + cs.margin.horizontal()
        })
        .sum();
    widths + s.main_gap() * flowing.len().saturating_sub(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Colors, Rgba, Sink, TextStyle, parse};
    use mlua::Lua;

    /// Ten wide a character, twenty tall a line, wrapping on characters.
    struct Fixed;

    impl Metrics for Fixed {
        fn text(&self, t: &Text, max: Option<f32>) -> Size {
            let chars: usize = t.spans.iter().map(|s| s.text.chars().count()).sum();
            let w = chars as f32 * 10.0;
            match max {
                Some(m) if w > m && m >= 10.0 => {
                    let per = (m / 10.0).floor();
                    Size {
                        w: per * 10.0,
                        h: (chars as f32 / per).ceil() * 20.0,
                    }
                }
                _ => Size { w, h: 20.0 },
            }
        }

        fn text_input(&self, _: &TextInput, max: Option<f32>) -> Size {
            Size {
                w: max.unwrap_or(100.0),
                h: 20.0,
            }
        }

        fn input(&self, _: &Input, max: Option<f32>) -> Size {
            Size {
                w: max.unwrap_or(100.0),
                h: 20.0,
            }
        }
    }

    struct NoGroups;

    impl Colors for NoGroups {
        fn group(&self, _: &str, _: bool) -> Option<Rgba> {
            None
        }

        fn style(&self, _: &str) -> crate::node::GroupStyle {
            Default::default()
        }
    }

    fn tree(src: &str) -> Node {
        let lua = Lua::new();
        let t: mlua::Table = lua.load(format!("return {src}")).eval().unwrap();
        parse(
            &t,
            &NoGroups,
            &TextStyle::default(),
            &Sink::new(lua.create_table().unwrap(), 0),
        )
        .unwrap()
    }

    fn rects(p: &Placed) -> Vec<(f32, f32, f32, f32)> {
        p.children
            .iter()
            .map(|c| (c.rect.x, c.rect.y, c.rect.w, c.rect.h))
            .collect()
    }

    fn lay(n: &Node, w: f32, h: f32) -> Placed<'_> {
        place(
            n,
            Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            },
            &Fixed,
        )
    }

    #[test]
    fn a_row_shares_what_is_left_by_grow() {
        let n =
            tree("{ direction = 'row', gap = 10, { width = 50 }, { grow = 1 }, { width = 50 } }");
        assert_eq!(
            rects(&lay(&n, 300.0, 40.0)),
            vec![
                (0.0, 0.0, 50.0, 40.0),
                (60.0, 0.0, 180.0, 40.0),
                (250.0, 0.0, 50.0, 40.0)
            ]
        );
    }

    #[test]
    fn a_column_stretches_across_and_stacks_down() {
        let n = tree("{ padding = 5, { text = 'hello' }, { height = 30 } }");
        assert_eq!(
            rects(&lay(&n, 100.0, 100.0)),
            vec![(5.0, 5.0, 90.0, 20.0), (5.0, 25.0, 90.0, 30.0)]
        );
    }

    #[test]
    fn justify_between_and_center_align() {
        let n = tree(
            "{ direction = 'row', justify = 'between', align = 'center', { width = 20, height = 10 }, { width = 20, height = 10 } }",
        );
        assert_eq!(
            rects(&lay(&n, 100.0, 50.0)),
            vec![(0.0, 20.0, 20.0, 10.0), (80.0, 20.0, 20.0, 10.0)]
        );
    }

    #[test]
    fn auto_sizes_shrink_and_text_wraps_to_what_is_left() {
        let n = tree("{ direction = 'row', { text = 'aaaaaaaaaa' }, { width = 40 } }");
        let p = lay(&n, 100.0, 100.0);
        // 100 wide: the 40 stays, the text gets 60 and wraps to two lines.
        assert_eq!(p.children[1].rect.w, 40.0);
        assert_eq!(p.children[0].rect.w, 60.0);
    }

    #[test]
    fn percentages_margins_and_min_max() {
        let n = tree(
            "{ direction = 'row', { width = '50%', margin = 4 }, { grow = 1, max_width = 20 } }",
        );
        let r = rects(&lay(&n, 200.0, 50.0));
        assert_eq!(r[0], (4.0, 4.0, 100.0, 42.0));
        assert_eq!(r[1].2, 20.0);
    }

    #[test]
    fn a_scroll_container_lays_children_out_at_full_height() {
        let n = tree("{ overflow = 'scroll', { height = 80 }, { height = 80 } }");
        let p = lay(&n, 100.0, 100.0);
        assert_eq!(p.content, Some(160.0));
        assert_eq!(p.inner.h, 100.0);
        assert_eq!(
            rects(&p),
            vec![(0.0, 0.0, 100.0, 80.0), (0.0, 80.0, 100.0, 80.0)]
        );
    }

    #[test]
    fn absolute_nodes_leave_the_flow_and_sit_by_their_edges() {
        let n = tree(
            "{ { height = 30 }, { position = 'absolute', right = 10, bottom = 5, width = 40, height = 20 }, { height = 30 } }",
        );
        let p = lay(&n, 100.0, 100.0);
        // The two flow children stack; the absolute one is last, bottom right.
        assert_eq!(
            rects(&p),
            vec![
                (0.0, 0.0, 100.0, 30.0),
                (0.0, 30.0, 100.0, 30.0),
                (50.0, 75.0, 40.0, 20.0)
            ]
        );
    }

    #[test]
    fn a_bubble_is_as_tall_as_its_text_and_padding() {
        let n = tree(
            "{ margin = { 16, 0, 6, 0 }, direction = 'row', justify = 'end', align = 'start', { max_width = '78%', padding = { 10, 14, 10, 14 }, { text = 'hello' } } }",
        );
        let p = lay(&n, 1000.0, 700.0);
        // 20 of text and 20 of padding; the wrapper has no height of its own.
        assert_eq!(p.children[0].rect.h, 40.0, "{:?}", p.children[0].rect);
    }

    #[test]
    fn a_wrapping_row_goes_on_to_another_line() {
        let n = tree(
            "{ direction = 'row', flex_wrap = true, column_gap = 10, row_gap = 6, align = 'start', { width = 40, height = 20 }, { width = 40, height = 20 }, { width = 40, height = 20 } }",
        );
        assert_eq!(
            rects(&lay(&n, 100.0, 100.0)),
            vec![
                (0.0, 0.0, 40.0, 20.0),
                (50.0, 0.0, 40.0, 20.0),
                (0.0, 26.0, 40.0, 20.0)
            ]
        );
        // Its height at that width is two lines and the space between.
        let z = natural(&n, Some(100.0), &Fixed);
        assert_eq!(z.h, 46.0);
    }

    #[test]
    fn align_self_order_and_auto_margins() {
        let n = tree(
            "{ direction = 'row', align = 'start', { width = 20, height = 10, order = 2 }, { width = 20, height = 10, align_self = 'end', order = 1 }, { width = 20, height = 10, margin = { 0, 0, 0, 'auto' }, order = 3 } }",
        );
        // Second first (order 1), then the first; the third is pushed to the end.
        assert_eq!(
            rects(&lay(&n, 100.0, 40.0)),
            vec![
                (0.0, 30.0, 20.0, 10.0),
                (20.0, 0.0, 20.0, 10.0),
                (80.0, 0.0, 20.0, 10.0)
            ]
        );
    }

    #[test]
    fn aspect_ratio_gives_the_missing_side() {
        let n = tree("{ { aspect_ratio = 2 } }");
        assert_eq!(rects(&lay(&n, 100.0, 100.0)), vec![(0.0, 0.0, 100.0, 50.0)]);
    }

    #[test]
    fn units_em_vw_and_calc() {
        let lua = Lua::new();
        let t: mlua::Table = lua
            .load("return { direction = 'row', size = 10, { width = 'calc(50% - 20px)' }, { width = '2em' }, { width = '10vw' } }")
            .eval()
            .unwrap();
        let sink = Sink::new(lua.create_table().unwrap(), 0).with_viewport(1000.0, 500.0);
        let n = parse(&t, &NoGroups, &TextStyle::default(), &sink).unwrap();
        let widths: Vec<f32> = lay(&n, 400.0, 50.0)
            .children
            .iter()
            .map(|c| c.rect.w)
            .collect();
        assert_eq!(widths, vec![180.0, 20.0, 100.0]);
    }

    #[test]
    fn a_row_that_scrolls_sideways_is_as_long_as_its_children() {
        let n = tree(
            "{ direction = 'row', overflow = 'scroll', gap = 10, { width = 300 }, { width = 300 } }",
        );
        let p = lay(&n, 200.0, 50.0);
        assert_eq!(p.content, Some(610.0));
        assert_eq!(p.inner.w, 200.0);
        assert_eq!(rects(&p)[1].0, 310.0);
    }

    #[test]
    fn natural_size_wraps_text_at_the_width_on_offer() {
        let n = tree("{ padding = 10, { text = 'aaaaaaaaaa' } }");
        let z = natural(&n, Some(70.0), &Fixed);
        // 70 - 20 padding = 50 for text: 5 per line, 2 lines.
        assert_eq!((z.w, z.h), (70.0, 60.0));
    }
}
