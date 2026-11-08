//! Paint a placed tree: boxes, text, inputs, images, scrollbars and fades.

use std::collections::HashMap;

use bone_ui::lua::Host;

use crate::Gui;
use crate::backend::interact::Sense;
use crate::backend::text::Job;
use crate::backend::{Button, Rect, Shape, pos, vec2};
use crate::flex::Placed;
use crate::node::{self, Colors, Corners, Input, Kind, Overflow, Rgba, ScrollbarShow, TextAlign};

use super::edit::{Field, Look};
use super::shapes::{cursor_icon, draw_border};
use super::{Arg, Focusable, Fonts, Frame, Fx, Hit, Tween, needs_key};

/// The text selection's color when the scheme sets none.
const SELECTION: Rgba = Rgba(0x2a, 0x3f, 0x55, 0xff);

impl Gui {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint(
        &mut self,
        fx: &mut Fx,
        fonts: &Fonts,
        p: &Placed,
        clip: Rect,
        path: u64,
        listed: Option<&str>,
        lists: &HashMap<String, (usize, Vec<Placed>)>,
        frame: &mut Frame,
        tint: Option<Rgba>,
    ) {
        let r = Rect::from(p.rect);
        let s = &p.node.style;
        let h = &p.node.handlers;
        // A string key only for a node something will look up again. The
        // rest of the tree is addressed by the path hash.
        let key = if let Some(id) = listed {
            id.to_owned()
        } else if let Some(id) = &p.node.id {
            id.clone()
        } else if needs_key(p) {
            format!("@{path:x}")
        } else {
            String::new()
        };
        let outer_clip = clip;
        let clip = if s.clips() { clip.intersect(r) } else { clip };
        if s.block_input {
            frame.scope_count += 1;
            frame.scopes.push(frame.scope_count);
            frame.trap = Some(frame.scope_count);
        }

        // Pointer: registered before the children, so they sit above it,
        // and before the paint, which follows the hover.
        let visible = r.intersect(clip);
        if s.block_input && visible.is_positive() {
            self.view
                .pointer
                .sense(&format!("block:{key}"), visible, Sense::Drag);
        }
        if let Some(id) = p.node.id.as_deref() {
            frame.leaves.push((id.to_owned(), r));
        }
        if let Some(area) = s.window_control
            && visible.is_positive()
        {
            fx.out.controls.push((area, visible));
        }
        let (mut hovered, mut pressed, mut sensed) = (false, false, false);
        let styled = s.hover.is_some() || s.active.is_some() || s.focus.is_some();
        // Asks for the focus: a press gives it, not only tab. A text input
        // has the keyboard its own way, and its `on_key` beside it.
        let input = matches!(p.node.kind, Kind::TextInput(_));
        if let (true, Some(f)) = (input, h.key) {
            frame.input_keys.insert(key.clone(), f);
        }
        let wants_focus =
            !input && (s.focusable == Some(true) || h.key.is_some() || s.focus.is_some());
        let focusable = !input && s.focusable.unwrap_or(wants_focus || h.click.is_some());
        if focusable && visible.is_positive() {
            frame.focusables.push(Focusable {
                key: key.clone(),
                on_key: h.key,
                click: h.click,
                scope: frame.scopes.last().copied(),
                autofocus: s.autofocus,
                key_guard: s.key_guard,
            });
        }
        let focused = self.view.focused.as_deref() == Some(&key);
        if (h.any() || s.cursor.is_some() || styled || s.tooltip.is_some() || wants_focus)
            && visible.is_positive()
        {
            let sense = if h.drag.is_some() {
                Sense::Drag
            } else if h.clicks() || wants_focus || s.active.is_some() {
                Sense::Click
            } else {
                Sense::Hover
            };
            let resp = self.view.pointer.sense(&format!("node:{key}"), visible, sense);
            sensed = true;
            let pointer = &self.view.pointer;
            // Over a child that takes the pointer is still over this one,
            // as last frame found (see after the children).
            hovered = resp.hovered
                || (self.view.hover.contains(&key) && resp.contains_pointer);
            // A `mouse` handler took the gesture (`route_pointer`): the node
            // hears none of it.
            let quiet = self.view.mouse_taken != super::MouseTaken::No;
            pressed = resp.pressed_on && pointer.is_down(Button::Primary) && !quiet;
            if hovered {
                if let Some(icon) = s.cursor.as_deref().and_then(cursor_icon) {
                    fx.out.cursor = icon;
                }
                frame.hover.insert(key.clone());
                // The innermost node under a press that wants it has the
                // focus (see `settle_focus`).
                if wants_focus && pointer.primary_pressed() {
                    frame.press_focus = Some(key.clone());
                }
                if let Some(tip) = &s.tooltip {
                    frame.tooltip = Some((key.clone(), tip.clone()));
                }
            }
            let at_pointer = |q: Option<crate::backend::Pos>| {
                q.map_or(Arg::Pos { x: 0.0, y: 0.0 }, |q| Arg::Pos {
                    x: q.x - r.min.x,
                    y: q.y - r.min.y,
                })
            };
            let pos = at_pointer(pointer.pos);
            let was = self.view.pressed.contains(&key) && !quiet;
            let clicked = resp.clicked && !quiet;
            if pressed {
                frame.pressed.insert(key.clone());
                if let (Some(f), false) = (h.press, was) {
                    frame.events.push((f, pos.clone()));
                }
            } else if was || clicked {
                // Down and up within one frame are both still heard.
                if let (Some(f), false) = (h.press, was) {
                    frame.events.push((f, pos.clone()));
                }
            }
            // Motion and release can arrive together (a quick drag between
            // frames): the final position comes before `on_release`.
            if let (Some(f), true) = (h.drag, (resp.dragged || resp.drag_released) && !quiet)
                && pointer.delta != crate::backend::Vec2::ZERO
                && let Some(q) = pointer.pos
            {
                frame.events.push((f, Arg::Pos { x: q.x, y: q.y }));
            }
            if !pressed
                && (was || clicked)
                && let Some(f) = h.release
            {
                frame.events.push((f, pos.clone()));
            }
            let at = at_pointer(resp.pointer);
            if let (Some(f), true) = (h.click, clicked) {
                frame.events.push((f, at.clone()));
            }
            if let (Some(f), true) = (h.double_click, resp.double_clicked && !quiet) {
                frame.events.push((f, at.clone()));
            }
            if let (Some(f), true) = (h.secondary, resp.secondary_clicked && !quiet) {
                frame.events.push((f, at.clone()));
            }
            if let (Some(f), true) = (h.middle, resp.middle_clicked && !quiet) {
                frame.events.push((f, at));
            }
        }

        let hover_before = frame.hover.len();

        // The style now: the node's own, then what hover, focus and press
        // change, in that order.
        let mut bg = s.bg;
        let mut gradient = s.gradient.as_ref();
        let mut border_color = s.border_color;
        let mut shadows = &s.shadows;
        let mut opacity = s.opacity;
        let mut tint = tint;
        for state in [
            hovered.then_some(&s.hover),
            focused.then_some(&s.focus),
            pressed.then_some(&s.active),
        ]
        .into_iter()
        .flatten()
        .flatten()
        {
            if state.bg.is_some() || state.gradient.is_some() {
                (bg, gradient) = (state.bg, state.gradient.as_ref());
            }
            border_color = state.border_color.or(border_color);
            shadows = state.shadows.as_ref().unwrap_or(shadows);
            opacity = state.opacity.or(opacity);
            tint = state.color.or(tint);
        }
        // A border with a color of its own that fades to nothing is not drawn,
        // rather than drawn in the default gray.
        let own_border = border_color.is_some();
        // `transition`: what changed is faded in, from where it was.
        let mut bg_rgba = bg;
        if let Some(tr) = s.transition {
            bg_rgba = self.fade_color(fx, frame, &key, "bg", bg, &tr);
            border_color = self.fade_color(fx, frame, &key, "border", border_color, &tr);
            let o = self.fade(
                fx,
                frame,
                &key,
                "opacity",
                [opacity.unwrap_or(1.0), 0.0, 0.0, 0.0],
                &tr,
            );
            opacity = Some(o[0]);
        }
        let bg = bg_rgba;
        let previous_opacity = fx.opacity;
        if let Some(o) = opacity {
            fx.opacity = previous_opacity * o.clamp(0.0, 1.0);
        }

        let corners = s.radius;
        // Under the box, and not cut by its own clip.
        for sh in shadows {
            fx.paint(
                Shape::Shadow {
                    rect: r,
                    corners,
                    offset: vec2(sh.dx, sh.dy),
                    blur: sh.blur.max(0.0),
                    spread: sh.spread.max(0.0),
                    color: sh.color,
                },
                outer_clip,
            );
        }
        if let Some(g) = gradient {
            fx.paint(
                Shape::Gradient {
                    rect: r,
                    corners,
                    gradient: g.clone(),
                },
                clip,
            );
        } else if let Some(bg) = bg {
            fx.paint(
                Shape::Rect {
                    rect: r,
                    corners,
                    fill: bg,
                },
                clip,
            );
        }
        if (s.border.top > 0.0
            || s.border.right > 0.0
            || s.border.bottom > 0.0
            || s.border.left > 0.0)
            && (border_color.is_some() || !own_border)
        {
            let c = border_color.unwrap_or(Rgba(0xa0, 0xa0, 0xa0, 0xff));
            draw_border(fx, clip, r, corners, s.border, s.border_style, c);
        }
        if let Some(o) = s.outline {
            fx.paint(
                Shape::Stroke {
                    rect: r.expand(o.offset),
                    corners,
                    width: o.width,
                    color: o.color,
                    outside: true,
                },
                outer_clip,
            );
        }
        // Reached with the keyboard and no `focus` look of its own: a ring.
        if focused && self.view.focus_visible && s.focus.is_none() {
            let c = (self.group("FocusRing", false))
                .or_else(|| self.group("Accent", false))
                .unwrap_or(Rgba(0xc8, 0xdc, 0xf5, 0xff));
            fx.paint(
                Shape::Stroke {
                    rect: r.expand(2.0),
                    corners,
                    width: 2.0,
                    color: c,
                    outside: true,
                },
                outer_clip,
            );
        }
        if (s.overflow == Overflow::Scroll || h.scroll.is_some()) && visible.is_positive() {
            frame.hits.push(Hit {
                rect: visible,
                scroll: (s.overflow == Overflow::Scroll)
                    .then(|| p.node.id.clone())
                    .flatten(),
                sideways: s.direction == crate::node::Direction::Row,
                on_scroll: h.scroll,
            });
        }

        let pad = &s.padding;
        let inner = Rect::from_min_max(
            r.min + vec2(pad.left + s.border.left, pad.top + s.border.top),
            r.max - vec2(pad.right + s.border.right, pad.bottom + s.border.bottom),
        );
        match &p.node.kind {
            // Text out of the clip is not shaped: a long item has most of its
            // text out of view.
            Kind::Text(t)
                if clip.intersects(
                    inner.expand(t.shadow.map_or(0.0, |s| s.dx.abs().max(s.dy.abs()))),
                ) =>
            {
                let width = (t.wrap || t.ellipsis).then_some(inner.width());
                let _span = crate::profile::Span::start("text-paint");
                let mut job = fonts.job(t, width);
                if t.align != TextAlign::Left {
                    job.box_width = Some(inner.width());
                }
                let galley = fonts.shaper.shape(job);
                drop(_span);
                // Text wider than its box stays in it: a row too narrow for
                // its pieces cuts them, rather than painting one over the
                // next. The height is left alone: a text taller than its
                // box keeps the lines it has.
                let text_clip = if galley.size.x > inner.width() + 0.5 {
                    Rect::from_min_max(
                        pos(inner.min.x.max(clip.min.x), clip.min.y),
                        pos(inner.max.x.min(clip.max.x), clip.max.y),
                    )
                } else {
                    clip
                };
                let at = inner.min;
                if t.selectable && !t.ellipsis && clip.intersects(inner) {
                    let k = if key.is_empty() {
                        format!("@{path:x}")
                    } else {
                        key.clone()
                    };
                    if let Some((sel, a, b)) = &self.view.text_selection
                        && *sel == k
                    {
                        let color = self.group("TextSelection", true).unwrap_or(SELECTION);
                        for rr in galley.range_rects(*a, *b) {
                            fx.paint(
                                Shape::Rect {
                                    rect: rr.translate(at.to_vec2()),
                                    corners: Corners::default(),
                                    fill: color,
                                },
                                text_clip,
                            );
                        }
                    }
                    frame.texts.push((k, at, galley.clone()));
                }
                if let Some(sh) = t.shadow {
                    fx.paint(
                        Shape::Text {
                            at: at + vec2(sh.dx, sh.dy),
                            text: galley.clone(),
                            color: Some(sh.color),
                        },
                        text_clip,
                    );
                }
                fx.paint(
                    Shape::Text {
                        at,
                        text: galley,
                        color: tint,
                    },
                    text_clip,
                );
            }
            Kind::Text(_) => {}
            Kind::Input(i) => self.input_ui(fx, fonts, i, inner, clip, frame),
            Kind::TextInput(t) => self.text_input_ui(fx, fonts, t, inner, clip, frame),
            Kind::Canvas(c) => {
                if visible.is_positive() && self.canvas_ui(fx, clip, r, &key, c.draw, c.fps) {
                    fx.out
                        .repaint_after(std::time::Duration::from_secs_f32(1.0 / c.fps));
                }
            }
            Kind::Image(i) => {
                if visible.is_positive() {
                    self.image_ui(fx, clip, inner, corners, i);
                }
            }
            Kind::Box => {
                for (n, c) in p.children.iter().enumerate() {
                    let child = path
                        .wrapping_mul(0x517cc1b727220a95)
                        .wrapping_add(n as u64 + 1);
                    self.paint(fx, fonts, c, clip, child, None, lists, frame, tint);
                }
                // The bar is over what scrolls under it.
                if let (Overflow::Scroll, Some(id)) = (s.overflow, &p.node.id) {
                    self.scrollbar(fx, clip, id, p);
                }
            }
            Kind::List(l) => {
                if let Some(id) = &p.node.id {
                    // Keyed by the item's place in the list, not in the
                    // window: scrolling must not renumber what is in view.
                    if let Some((first, items)) = lists.get(id) {
                        for (n, c) in items.iter().enumerate() {
                            // The list's path, so item 0 is not the root's
                            // path and two lists do not share item ids.
                            let at = path
                                .wrapping_mul(0x517cc1b727220a95)
                                .wrapping_add((first + n) as u64 + 1);
                            // Include the list key so a rebuilt chat cannot
                            // reuse a transition's key for another chat's
                            // item at the same index.
                            let key = format!("{id}:{}/{}", l.key, first + n);
                            self.paint(fx, fonts, c, clip, at, Some(&key), lists, frame, tint);
                        }
                    }
                    self.scrollbar(fx, clip, id, p);
                }
            }
        }
        fx.opacity = previous_opacity;
        // A child under the pointer keeps its parents hovered, as in CSS:
        // otherwise a button that shows on a row's hover hides as soon as
        // the pointer reaches it.
        if sensed && !hovered && frame.hover.len() > hover_before {
            hovered = true;
            frame.hover.insert(key.clone());
            fx.out.repaint();
        }
        if let (Some(f), true) = (h.hover, sensed) {
            let was = self.view.hover.contains(&key);
            if hovered != was {
                frame.events.push((f, Arg::Bool(hovered)));
            }
        }
        if s.block_input {
            frame.scopes.pop();
        }
    }

    /// `value` faded toward from where it was: the tween for `prop` of node
    /// `key`. The first time there is nothing to fade from.
    fn fade(
        &mut self,
        fx: &mut Fx,
        frame: &mut Frame,
        key: &str,
        prop: &str,
        value: [f32; 4],
        tr: &node::Transition,
    ) -> [f32; 4] {
        let id = format!("{key}#{prop}");
        frame.tweened.insert(id.clone());
        let now = std::time::Instant::now();
        let tween = self.view.tweens.entry(id).or_insert(Tween {
            from: value,
            to: value,
            at: now,
        });
        let progress = |t: &Tween| {
            let span = (tr.duration_ms / 1000.0).max(0.001);
            tr.easing.at(now.duration_since(t.at).as_secs_f32() / span)
        };
        let mix =
            |t: &Tween, k: f32| std::array::from_fn(|i| t.from[i] + (t.to[i] - t.from[i]) * k);
        if tween.to != value {
            // From wherever it had got to.
            tween.from = mix(tween, progress(tween));
            tween.to = value;
            tween.at = now;
        }
        let k = progress(tween);
        if k < 1.0 {
            // The display's next frame; a fade is short.
            fx.out.repaint_after(std::time::Duration::from_millis(16));
        }
        mix(tween, k)
    }

    /// A color (none is clear) faded the same way.
    fn fade_color(
        &mut self,
        fx: &mut Fx,
        frame: &mut Frame,
        key: &str,
        prop: &str,
        color: Option<Rgba>,
        tr: &node::Transition,
    ) -> Option<Rgba> {
        // Faded premultiplied: each channel weighted by its alpha. Straight
        // channels would pass through the color of a faint one at half
        // strength (a hover's white at 4% to a solid gray goes pale gray at
        // 50%: a flash). Appearing or going away is then a fade of the
        // alpha, in the color that was there.
        let id = format!("{key}#{prop}");
        let seen = self.view.tweens.contains_key(&id);
        let target = match (color, seen) {
            (Some(c), _) => {
                let a = c.3 as f32 / 255.0;
                [c.0 as f32 * a, c.1 as f32 * a, c.2 as f32 * a, c.3 as f32]
            }
            (None, true) => [0.0; 4],
            (None, false) => return None,
        };
        let v = self.fade(fx, frame, key, prop, target, tr);
        if v[3] <= 0.5 {
            return None;
        }
        let a = v[3] / 255.0;
        let channel = |x: f32| (x / a).round().clamp(0.0, 255.0) as u8;
        Some(Rgba(channel(v[0]), channel(v[1]), channel(v[2]), v[3].round() as u8))
    }

    /// A thin bar on the right edge when the content is longer. Drag it, or
    /// press on the track to jump there and drag on from it.
    fn scrollbar(&mut self, fx: &mut Fx, clip: Rect, id: &str, p: &Placed) {
        let style = p.node.style.scrollbar;
        let Some(s) = self.view.scroll.get(id).filter(|s| s.max > 0.0) else {
            return;
        };
        let rect = Rect::from(p.rect);
        let dragging = s.grab.is_some();
        match style.show {
            ScrollbarShow::Hidden => return,
            ScrollbarShow::Hover => {
                if !dragging && !self.view.pointer.pos.is_some_and(|q| rect.contains(q)) {
                    return;
                }
            }
            ScrollbarShow::Auto => {}
        }
        let view = Rect::from(p.inner);
        let sideways = p.node.style.direction == crate::node::Direction::Row;
        let (max, viewport) = (s.max, s.viewport);
        let total = viewport + max;
        // Along the bar's axis: where the track is, and how long the thumb.
        let (lo, span) = if sideways {
            (view.min.x, view.width())
        } else {
            (view.min.y, view.height())
        };
        let len = (span * viewport / total).max(24.0);
        let room = (span - len).max(1.0);
        let thumb_at = |offset: f32| lo + room * (offset / max);
        let bar_at = |start: f32| {
            if sideways {
                Rect::from_min_size(
                    pos(start, view.max.y - style.width - style.inset),
                    vec2(len, style.width),
                )
            } else {
                Rect::from_min_size(
                    pos(rect.max.x - style.width - style.inset, start),
                    vec2(style.width, len),
                )
            }
        };
        let mut offset = s.offset;
        // The grab is easier than the bar is thin: a track at least this wide.
        let grab = style.width.max(12.0);
        let track = if sideways {
            Rect::from_min_max(pos(view.min.x, view.max.y - grab), view.max)
        } else {
            Rect::from_min_max(
                pos(rect.max.x - grab - style.inset, view.min.y),
                pos(rect.max.x - style.inset, view.max.y),
            )
        };
        let resp = (self.view.pointer).sense(&format!("scrollbar:{id}"), track.intersect(clip), Sense::Drag);
        let along = |q: crate::backend::Pos| if sideways { q.x } else { q.y };
        let mut held = s.grab;
        if resp.press_started
            && let Some(q) = resp.pointer
        {
            let start = thumb_at(offset);
            // On the thumb, it stays under the pointer where it was
            // taken; on the track, its middle goes there.
            let hold = if (start..=start + len).contains(&along(q)) {
                along(q) - start
            } else {
                len / 2.0
            };
            // Keep the drag in pointer-delta space. A list can be rebuilt
            // while the pointer is down; remapping the unchanged pointer
            // position through a new max would make the thumb jump.
            let offset_at_pointer = ((along(q) - hold - lo) / room * max).clamp(0.0, max);
            held = Some([along(q), offset_at_pointer, max, room]);
            offset = offset_at_pointer;
        }
        if let Some([start_pos, start_offset, start_max, start_room]) = held {
            if let Some(q) = self.view.pointer.pos {
                offset = (start_offset + (along(q) - start_pos) / start_room.max(1.0) * start_max)
                    .clamp(0.0, max);
            }
            if !self.view.pointer.is_down(Button::Primary) {
                held = None;
            }
        }
        let changed = self.view.scroll.get(id).is_some_and(|s| offset != s.offset);
        if let Some(s) = self.view.scroll.get_mut(id) {
            if changed {
                s.offset = offset;
                s.target = None;
            }
            s.grab = held;
            s.dragging = held.is_some();
        }
        if changed {
            fx.out.repaint();
        }
        let corners = Corners {
            tl: style.radius,
            tr: style.radius,
            br: style.radius,
            bl: style.radius,
        };
        fx.paint(
            Shape::Rect {
                rect: bar_at(thumb_at(offset)),
                corners,
                fill: style.color,
            },
            clip,
        );
    }

    /// A text field of its own in `area`: its text is kept here by id, Lua
    /// hears of each change and of enter.
    fn text_input_ui(
        &mut self,
        fx: &mut Fx,
        fonts: &Fonts,
        t: &node::TextInput,
        area: Rect,
        clip: Rect,
        frame: &mut Frame,
    ) {
        if !clip.intersects(area) {
            return;
        }
        let key = t.id.clone();
        let vs = &mut self.view;
        let buf = vs.inputs.entry(key.clone()).or_default();
        // Lua pushed a new value.
        if let Some(v) = t.value.as_ref().filter(|v| buf.pushed.as_ref() != Some(*v)) {
            buf.pushed = Some(v.clone());
            super::remember_text(buf, v.clone());
            buf.anchor = buf.anchor.min(v.chars().count());
            buf.cursor = v.chars().count();
        }
        // Asked to take the keyboard, or appearing with `focus`.
        if vs.focus_request.as_deref() == Some(&key)
            || (t.focus && vs.focused_once.insert(key.clone()))
        {
            // One thing has the keyboard.
            (vs.focus_request, vs.focused) = (None, None);
            vs.input_focus = Some(key.clone());
            if t.select {
                let n = buf.text.chars().count();
                (buf.anchor, buf.cursor) = (0, n);
            }
        }
        let has_keys = vs.input_focus.as_deref() == Some(&key);
        let mut field = Field::new(std::mem::take(&mut buf.text), buf.anchor, buf.cursor);
        let look = Look {
            style: fonts.field_style(&t.text),
            placeholder: &t.placeholder,
            placeholder_color: self
                .group("Placeholder", false)
                .unwrap_or(Rgba(0x66, 0x66, 0x66, 0xff)),
            selection: self.group("TextSelection", true).unwrap_or(SELECTION),
            cursor: (self.group("Cursor", false))
                .or_else(|| self.group("Normal", false))
                .unwrap_or(t.text.color),
            password: t.password,
            multiline: t.multiline,
            read_only: t.read_only,
            enter_newline: true,
            marks: Vec::new(),
            ghost: None,
        };
        let out = self.field_ui(fx, &key, area, clip, &mut field, has_keys, &look);
        let vs = &mut self.view;
        if out.pressed {
            // A press on it is not one on the node around it.
            frame.press_focus = None;
            frame.input_pressed = true;
            vs.input_focus = Some(key.clone());
            vs.focused = None;
        }
        let mut has_keys = vs.input_focus.as_deref() == Some(&key);
        if out.escaped || (out.submitted && !t.multiline) {
            vs.input_focus = None;
            has_keys = false;
        }
        if let Some(forward) = out.tabbed {
            vs.input_focus = None;
            has_keys = false;
            vs.move_focus(forward);
        }
        if has_keys {
            frame.input_focus = Some(key.clone());
        }
        if let (true, Some(f)) = (out.changed, t.on_change) {
            frame.events.push((f, Arg::Text(field.text.clone())));
        }
        if let (true, Some(f)) = (out.submitted, t.on_submit) {
            frame.events.push((f, Arg::Text(field.text.clone())));
        }
        if let Some(buf) = self.view.inputs.get_mut(&key) {
            (buf.anchor, buf.cursor) = (field.anchor, field.cursor);
            super::remember_text(buf, field.text);
        }
    }

    /// A picture in `area`, as its `fit` says; the pixels are read the first
    /// time it is asked for.
    pub(crate) fn image_ui(
        &self,
        fx: &mut Fx,
        clip: Rect,
        area: Rect,
        corners: Corners,
        i: &node::Image,
    ) {
        use crate::node::Fit;
        use crate::preview::Pixels;
        match self.pixels(&i.source) {
            Pixels::Loading => {}
            Pixels::Failed(e) => {
                let st = crate::backend::RunStyle::plain(
                    crate::backend::Family::Sans,
                    12.0,
                    Rgba(0x99, 0x99, 0x99, 0xff),
                );
                let text = fx.shaper.shape(Job::simple(e, st, None));
                let at = area.center() - text.size * 0.5;
                fx.paint(
                    Shape::Text {
                        at,
                        text,
                        color: None,
                    },
                    clip,
                );
            }
            Pixels::Ready(picture) => {
                let size = picture.size();
                let scale_to = |target: crate::backend::Vec2, cover: bool| {
                    let k = (target.x / size.x, target.y / size.y);
                    let k = if cover { k.0.max(k.1) } else { k.0.min(k.1) };
                    size * k
                };
                let whole = Rect::from_min_max(pos(0.0, 0.0), pos(1.0, 1.0));
                let (rect, uv) = match i.fit {
                    Fit::Fill => (area, whole),
                    Fit::Contain => (
                        Rect::from_center_size(area.center(), scale_to(area.size(), false)),
                        whole,
                    ),
                    Fit::None => (Rect::from_center_size(area.center(), size), whole),
                    Fit::Cover => {
                        // The part of the picture the box shows.
                        let drawn = scale_to(area.size(), true);
                        let (kx, ky) = (area.width() / drawn.x, area.height() / drawn.y);
                        let uv = Rect::from_center_size(pos(0.5, 0.5), vec2(kx, ky));
                        (area, uv)
                    }
                };
                fx.paint(
                    Shape::Image {
                        rect,
                        uv,
                        corners,
                        picture,
                    },
                    clip,
                );
            }
        }
    }

    /// The prompt's text field in `area`: the shared prompt's text, cursor
    /// and selection, edited here, with Lua's highlights and ghost text.
    fn input_ui(
        &mut self,
        fx: &mut Fx,
        fonts: &Fonts,
        i: &Input,
        area: Rect,
        clip: Rect,
        frame: &mut Frame,
    ) {
        if !clip.intersects(area) {
            return;
        }
        let style = fonts.field_style(&i.text);
        let hint = (self.group("Placeholder", false)).unwrap_or(Rgba(0x66, 0x66, 0x66, 0xff));
        // `bone.ui.prompt_highlight`: styles over ranges of the text, and
        // ghost text after the cursor.
        let cols = self
            .view
            .cell
            .map_or(80, |(w, _)| (area.width() / w) as u16);
        let marks = self.prompt_marks(cols);
        // Rows and columns as the text's char offsets.
        let mut starts = vec![0];
        for line in self.draft.split('\n') {
            starts.push(starts.last().copied().unwrap_or(0) + line.chars().count() + 1);
        }
        let styled = (marks.ranges.iter())
            .filter_map(|m| {
                let start = *starts.get(m.row)?;
                let len = starts.get(m.row + 1).map_or(0, |e| e - start - 1);
                let (a, b) = (m.from.min(len), m.to.min(len));
                Some((start + a, start + b, self.mark_style(&m.hl, &style)))
            })
            .collect();
        let ghost = (marks.ghost.clone()).map(|g| {
            let color = self.group(&marks.ghost_hl, false).unwrap_or(hint);
            (g, color)
        });
        let look = Look {
            style: style.clone(),
            placeholder: &i.placeholder,
            placeholder_color: hint,
            selection: self.group("TextSelection", true).unwrap_or(SELECTION),
            cursor: (self.group("Cursor", false))
                .or_else(|| self.group("Normal", false))
                .unwrap_or(i.text.color),
            password: false,
            multiline: true,
            read_only: false,
            enter_newline: false,
            marks: styled,
            ghost,
        };
        // The prompt has the keyboard when no node and no text input does.
        let has_keys = self.view.focused_node().is_none() && self.view.input_focus.is_none();
        let mut field = Field::new(
            std::mem::take(&mut self.draft),
            self.prompt.anchor,
            self.prompt.cursor,
        );
        let out = self.field_ui(fx, "", area, clip, &mut field, has_keys, &look);
        self.draft = field.text;
        (self.prompt.anchor, self.prompt.cursor) = (field.anchor, field.cursor);
        if out.pressed {
            frame.press_focus = None;
            frame.input_pressed = true;
            self.view.input_focus = None;
        }
        // A click on the prompt gives it the keyboard back from a panel.
        if out.clicked && self.focused_panel().is_some() {
            let _ = bone_ui::lua::panels::focus_panel(self, None);
        }
        self.prompt_changed();
        // Typed this frame: the highlights follow on the next.
        if !self.marks_fresh() {
            fx.out.repaint();
        }
    }
}

impl Gui {
    /// How a `prompt_highlight` range in group `hl` is drawn over `base`.
    fn mark_style(
        &self,
        hl: &str,
        base: &crate::backend::RunStyle,
    ) -> crate::backend::RunStyle {
        let s = self.style(hl);
        crate::backend::RunStyle {
            color: s.fg.unwrap_or(base.color),
            background: self.group(hl, true),
            italic: s.italic,
            underline: s.underline,
            ..base.clone()
        }
    }

    /// The text nodes selected with the pointer: a press on one starts a
    /// selection in it, a drag extends it, a double click takes a word, and
    /// a press anywhere else ends it.
    pub(in crate::nodeview) fn select_text(
        &mut self,
        fx: &mut Fx,
        texts: &[(String, crate::backend::Pos, std::sync::Arc<crate::backend::Shaped>)],
    ) {
        let pointer = &self.view.pointer;
        let Some(p) = pointer.pos else { return };
        let under = texts
            .iter()
            .rev()
            .find(|(_, at, t)| t.rect_at(*at).contains(p));
        if pointer.primary_pressed() {
            self.view.text_selection = None;
            self.view.selection_text = None;
            if let Some((k, at, t)) = under
                && self.view.mouse_taken == super::MouseTaken::No
            {
                let i = t.index_at(pos(p.x - at.x, p.y - at.y));
                self.view.text_selection = Some((k.clone(), i, i));
                self.view.edits.entry(format!("text:{k}")).or_default().selecting = true;
            }
            return;
        }
        let Some((k, a, _)) = self.view.text_selection.clone() else {
            return;
        };
        let Some((_, at, t)) = texts.iter().find(|(key, ..)| *key == k) else {
            return;
        };
        let down = pointer.is_down(Button::Primary);
        let state = self.view.edits.entry(format!("text:{k}")).or_default();
        if state.selecting {
            let i = t.index_at(pos(p.x - at.x, p.y - at.y));
            if !down {
                state.selecting = false;
            }
            let (lo, hi) = (a.min(i), a.max(i));
            self.view.text_selection = Some((k.clone(), a, i));
            self.view.selection_text =
                (lo < hi).then(|| t.text.chars().skip(lo).take(hi - lo).collect());
            fx.out.repaint();
        }
    }

    /// The text selected in a text node, if any.
    pub(crate) fn selected_text(&self) -> Option<String> {
        self.view.text_selection.as_ref()?;
        self.view.selection_text.clone()
    }

    /// A tooltip by the pointer once it has rested on its node a moment.
    pub(in crate::nodeview) fn tooltip(&mut self, fx: &mut Fx, tip: Option<(String, String)>) {
        const DELAY: std::time::Duration = std::time::Duration::from_millis(500);
        let Some((key, text)) = tip else {
            self.view.tooltip_since = None;
            return;
        };
        let now = std::time::Instant::now();
        let since = match &self.view.tooltip_since {
            Some((k, at)) if *k == key && self.view.pointer.delta == crate::backend::Vec2::ZERO => *at,
            Some((k, at)) if *k == key => *at,
            _ => {
                self.view.tooltip_since = Some((key, now));
                now
            }
        };
        if now.duration_since(since) < DELAY {
            fx.out.repaint_after(DELAY - now.duration_since(since));
            return;
        }
        let Some(p) = self.view.pointer.pos else { return };
        let fg = self.group("Normal", false).unwrap_or(Rgba(0xee, 0xee, 0xee, 0xff));
        let bg = (self.group("Panel", true))
            .or_else(|| self.group("Normal", true))
            .unwrap_or(Rgba(0x26, 0x26, 0x26, 0xff));
        let rule = self
            .group("WinSeparator", false)
            .unwrap_or(Rgba(0x2c, 0x2c, 0x2c, 0xff));
        let st = crate::backend::RunStyle::plain(crate::backend::Family::Sans, 13.0, fg);
        let shaped = fx.shaper.shape(Job::simple(text, st, Some(320.0)));
        let screen = fx.input.screen;
        let size = shaped.size + vec2(16.0, 10.0);
        let mut at = p + vec2(12.0, 18.0);
        at.x = at.x.min(screen.max.x - size.x).max(screen.min.x);
        if at.y + size.y > screen.max.y {
            at.y = p.y - size.y - 6.0;
        }
        let card = Rect::from_min_size(at, size);
        let r = Corners {
            tl: 6.0,
            tr: 6.0,
            br: 6.0,
            bl: 6.0,
        };
        let opacity = std::mem::replace(&mut fx.opacity, 1.0);
        fx.paint(
            Shape::Shadow {
                rect: card,
                corners: r,
                offset: vec2(0.0, 4.0),
                blur: 12.0,
                spread: 0.0,
                color: Rgba(0, 0, 0, 90),
            },
            screen,
        );
        fx.paint(
            Shape::Rect {
                rect: card,
                corners: r,
                fill: bg,
            },
            screen,
        );
        fx.paint(
            Shape::Stroke {
                rect: card,
                corners: r,
                width: 1.0,
                color: rule,
                outside: false,
            },
            screen,
        );
        fx.paint(
            Shape::Text {
                at: at + vec2(8.0, 5.0),
                text: shaped,
                color: None,
            },
            screen,
        );
        fx.opacity = opacity;
    }
}
