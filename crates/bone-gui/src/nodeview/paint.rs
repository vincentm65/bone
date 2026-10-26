//! Paint a placed tree: boxes, text, inputs, images, scrollbars and fades.

use std::collections::HashMap;

use bone_ui::lua::Host;
use eframe::egui::{
    self, Color32, FontFamily, FontId, Key, KeyboardShortcut, Modifiers, Sense, Stroke, StrokeKind,
    TextEdit,
};

use crate::Gui;
use crate::flex::Placed;
use crate::node::{self, Colors, Input, Kind, Overflow, Rgba, ScrollbarShow, TextAlign};

use super::shapes::{cursor_icon, draw_border, gradient_shape, to_corners};
use super::{Arg, Focusable, Fonts, Frame, Hit, Tween, needs_key, node_text, remember_text, rgba};

impl Gui {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint(
        &mut self,
        ui: &mut egui::Ui,
        fonts: &Fonts,
        p: &Placed,
        clip: egui::Rect,
        path: u64,
        listed: Option<&str>,
        lists: &HashMap<String, (usize, Vec<Placed>)>,
        frame: &mut Frame,
        tint: Option<Rgba>,
    ) {
        let r = egui::Rect::from_min_size(
            egui::pos2(p.rect.x, p.rect.y),
            egui::vec2(p.rect.w, p.rect.h),
        );
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
        let visible = r.intersect(ui.painter().clip_rect()).intersect(clip);
        if s.block_input && visible.is_positive() {
            ui.interact(
                visible,
                egui::Id::new(("block", &key)),
                Sense::click_and_drag(),
            );
        }
        if let Some(id) = p.node.id.as_deref() {
            frame.leaves.push((id.to_owned(), r));
        }
        let (mut hovered, mut pressed, mut sensed) = (false, false, false);
        let styled = s.hover.is_some() || s.active.is_some() || s.focus.is_some();
        // Asks for the focus: a press gives it, not only tab. A text input
        // has egui's own focus instead, and its `on_key` beside it.
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
        // What a screen reader is told it is: a button when it is clicked,
        // unless it says.
        let role = match s.role {
            Some(node::Role::None) => None,
            Some(r) => Some(r),
            None if h.clicks() => Some(node::Role::Button),
            None => None,
        };
        let silent = s.role == Some(node::Role::None);
        let mut named = silent;
        if (h.any()
            || s.cursor.is_some()
            || styled
            || s.tooltip.is_some()
            || wants_focus
            || role.is_some())
            && visible.is_positive()
        {
            let sense = if h.drag.is_some() {
                Sense::click_and_drag()
            } else if h.clicks() || wants_focus || s.active.is_some() {
                Sense::click()
            } else {
                Sense::hover()
            };
            let mut resp = ui.interact(visible, egui::Id::new(("node", &key)), sense);
            sensed = true;
            if let Some(role) = role {
                let label = (s.label.clone())
                    .or_else(|| {
                        let mut words = Vec::new();
                        node_text(p.node, &mut words);
                        (!words.is_empty()).then(|| words.join(" ").trim().to_owned())
                    })
                    .or_else(|| s.tooltip.clone())
                    .unwrap_or_default();
                let typ = widget_type(role);
                let selected = s.selected;
                resp.widget_info(|| match selected {
                    Some(on) => egui::WidgetInfo::selected(typ, true, on, &label),
                    None => egui::WidgetInfo::labeled(typ, true, &label),
                });
                named = true;
            }
            // A screen reader follows the keyboard focus.
            if focused && !resp.has_focus() {
                resp.request_focus();
            }
            // Over a child that takes the pointer is still over this one,
            // as last frame found (see after the children).
            hovered = resp.hovered()
                || (self.view.hover.contains(&key) && ui.rect_contains_pointer(visible));
            // A `mouse` handler took the gesture (`route_pointer`): the node
            // hears none of it.
            let quiet = self.view.mouse_taken != super::MouseTaken::No;
            pressed = resp.is_pointer_button_down_on() && !quiet;
            if hovered {
                if let Some(icon) = s.cursor.as_deref().and_then(cursor_icon) {
                    ui.ctx().set_cursor_icon(icon);
                }
                frame.hover.insert(key.clone());
                // The innermost node under a press that wants it has the
                // focus (see `settle_focus`).
                if wants_focus && ui.input(|i| i.pointer.primary_pressed()) {
                    frame.press_focus = Some(key.clone());
                }
            }
            let pos =
                ui.input(|i| i.pointer.interact_pos())
                    .map_or(Arg::Pos { x: 0.0, y: 0.0 }, |q| Arg::Pos {
                        x: q.x - r.min.x,
                        y: q.y - r.min.y,
                    });
            let was = self.view.pressed.contains(&key) && !quiet;
            let clicked = resp.clicked() && !quiet;
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
            // frames). egui has already ended capture then, so `dragged()`
            // is false. Deliver the final position before `on_release`.
            let released_drag = was
                && ui.input(|i| i.pointer.primary_released() && i.pointer.is_decidedly_dragging());
            if let (Some(f), true) = (h.drag, (resp.dragged() && !quiet) || released_drag) {
                let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
                if let (true, Some(q)) = (moved, ui.input(|i| i.pointer.interact_pos())) {
                    frame.events.push((f, Arg::Pos { x: q.x, y: q.y }));
                }
            }
            if !pressed
                && (was || clicked)
                && let Some(f) = h.release
            {
                frame.events.push((f, pos.clone()));
            }
            let at = resp
                .interact_pointer_pos()
                .map_or((0.0, 0.0), |q| (q.x - r.min.x, q.y - r.min.y));
            if let (Some(f), true) = (h.click, clicked) {
                frame.events.push((f, Arg::Pos { x: at.0, y: at.1 }));
            }
            if let (Some(f), true) = (h.double_click, resp.double_clicked() && !quiet) {
                frame.events.push((f, Arg::Pos { x: at.0, y: at.1 }));
            }
            if let (Some(f), true) = (h.secondary, resp.secondary_clicked() && !quiet) {
                frame.events.push((f, Arg::Pos { x: at.0, y: at.1 }));
            }
            if let (Some(f), true) = (h.middle, resp.middle_clicked() && !quiet) {
                frame.events.push((f, Arg::Pos { x: at.0, y: at.1 }));
            }
            if let Some(tip) = &s.tooltip {
                resp = resp.on_hover_text(tip.as_str());
            }
            let _ = resp;
        }
        if named {
            frame.named += 1;
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
        // `transition`: what changed is faded in, from where it was.
        let mut bg_rgba = bg;
        if let Some(tr) = s.transition {
            bg_rgba = self.fade_color(ui.ctx(), frame, &key, "bg", bg, &tr);
            border_color = self.fade_color(ui.ctx(), frame, &key, "border", border_color, &tr);
            let o = self.fade(
                ui.ctx(),
                frame,
                &key,
                "opacity",
                [opacity.unwrap_or(1.0), 0.0, 0.0, 0.0],
                &tr,
            );
            opacity = Some(o[0]);
        }
        let bg = bg_rgba;
        let previous_opacity = ui.opacity();
        if let Some(o) = opacity {
            ui.set_opacity(previous_opacity * o.clamp(0.0, 1.0));
        }

        let corners = to_corners(s.radius);
        // Under the box, and not cut by its own clip.
        for sh in shadows {
            let shadow = egui::epaint::Shadow {
                offset: [sh.dx as i8, sh.dy as i8],
                blur: sh.blur.clamp(0.0, 255.0) as u8,
                spread: sh.spread.clamp(0.0, 255.0) as u8,
                color: rgba(sh.color),
            };
            ui.painter()
                .with_clip_rect(outer_clip)
                .add(shadow.as_shape(r, corners));
        }
        let painter = ui.painter().with_clip_rect(clip);
        if let Some(g) = gradient {
            painter.add(gradient_shape(r, s.radius, g));
        } else if let Some(bg) = bg {
            painter.rect_filled(r, corners, rgba(bg));
        }
        if s.border.top > 0.0
            || s.border.right > 0.0
            || s.border.bottom > 0.0
            || s.border.left > 0.0
        {
            let c = border_color.map_or(Color32::GRAY, rgba);
            draw_border(&painter, r, corners, s.border, s.border_style, c);
        }
        if let Some(o) = s.outline {
            ui.painter().with_clip_rect(outer_clip).rect_stroke(
                r.expand(o.offset),
                corners,
                Stroke::new(o.width, rgba(o.color)),
                StrokeKind::Outside,
            );
        }
        // Reached with the keyboard and no `focus` look of its own: a ring.
        if focused && self.view.focus_visible && s.focus.is_none() {
            let c = (self.group("FocusRing", false))
                .or_else(|| self.group("Accent", false))
                .unwrap_or(Rgba(0xc8, 0xdc, 0xf5, 0xff));
            ui.painter().with_clip_rect(outer_clip).rect_stroke(
                r.expand(2.0),
                corners,
                Stroke::new(2.0, rgba(c)),
                StrokeKind::Outside,
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
        let inner = egui::Rect::from_min_max(
            r.min + egui::vec2(pad.left + s.border.left, pad.top + s.border.top),
            r.max - egui::vec2(pad.right + s.border.right, pad.bottom + s.border.bottom),
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
                let galley = fonts.ctx.fonts_mut(|f| f.layout_job(fonts.job(t, width)));
                drop(_span);
                // With an alignment, egui puts the galley's edge, center or
                // end at the position.
                let x = match t.align {
                    TextAlign::Left => inner.min.x,
                    TextAlign::Center => inner.center().x,
                    TextAlign::Right => inner.max.x,
                };
                if t.selectable
                    && t.wrap
                    && !t.ellipsis
                    && t.align == TextAlign::Left
                    && clip.intersects(inner)
                {
                    // Native text: dragging selects across the labels in
                    // view, and ctrl+c copies.
                    let job = fonts.job(t, width);
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(inner)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    child.set_clip_rect(clip);
                    child.add(egui::Label::new(job).selectable(true));
                } else {
                    // Painted text is invisible to screen readers: give it
                    // a label (hover only, so clicks go where they did).
                    let words: String = t.spans.iter().map(|s| s.text.as_str()).collect();
                    if frame.named == 0
                        && words.chars().any(char::is_alphanumeric)
                        && visible.is_positive()
                    {
                        let resp =
                            ui.interact(visible, egui::Id::new(("text", &key)), Sense::hover());
                        let words = words.trim().to_owned();
                        resp.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &words)
                        });
                    }
                    let at = egui::pos2(x, inner.min.y);
                    if let Some(sh) = t.shadow {
                        painter.galley_with_override_text_color(
                            at + egui::vec2(sh.dx, sh.dy),
                            galley.clone(),
                            rgba(sh.color),
                        );
                    }
                    match tint {
                        Some(c) => painter.galley_with_override_text_color(at, galley, rgba(c)),
                        None => painter.galley(at, galley, Color32::WHITE),
                    }
                }
            }
            Kind::Text(_) => {}
            Kind::Input(i) => self.input_ui(ui, fonts, i, inner, clip),
            Kind::TextInput(t) => self.text_input_ui(ui, t, inner, clip, frame),
            Kind::Canvas(c) => {
                if visible.is_positive() && self.canvas_ui(ui, &painter, r, &key, c.draw, c.fps) {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs_f32(1.0 / c.fps));
                }
            }
            Kind::Image(i) => {
                if visible.is_positive() {
                    self.image_ui(ui, &painter, inner, corners, i);
                }
            }
            Kind::Box => {
                for (n, c) in p.children.iter().enumerate() {
                    let child = path
                        .wrapping_mul(0x517cc1b727220a95)
                        .wrapping_add(n as u64 + 1);
                    self.paint(ui, fonts, c, clip, child, None, lists, frame, tint);
                }
                // The bar is over what scrolls under it.
                if let (Overflow::Scroll, Some(id)) = (s.overflow, &p.node.id) {
                    self.scrollbar(ui, &painter, id, p);
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
                            // reuse a transition/egui id for another chat's
                            // item at the same index.
                            let key = format!("{id}:{}/{}", l.key, first + n);
                            self.paint(ui, fonts, c, clip, at, Some(&key), lists, frame, tint);
                        }
                    }
                    self.scrollbar(ui, &painter, id, p);
                }
            }
        }
        ui.set_opacity(previous_opacity);
        // A child under the pointer keeps its parents hovered, as in CSS:
        // otherwise a button that shows on a row's hover hides as soon as
        // the pointer reaches it.
        if sensed && !hovered && frame.hover.len() > hover_before {
            hovered = true;
            frame.hover.insert(key.clone());
            ui.ctx().request_repaint();
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
        if named {
            frame.named -= 1;
        }
    }

    /// `value` faded toward from where it was: the tween for `prop` of node
    /// `key`. The first time there is nothing to fade from.
    fn fade(
        &mut self,
        ctx: &egui::Context,
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
            ctx.request_repaint();
        }
        mix(tween, k)
    }

    /// A color (none is clear) faded the same way.
    fn fade_color(
        &mut self,
        ctx: &egui::Context,
        frame: &mut Frame,
        key: &str,
        prop: &str,
        color: Option<Rgba>,
        tr: &node::Transition,
    ) -> Option<Rgba> {
        // Appearing or going away is a fade of the alpha, in the color
        // that was there.
        let id = format!("{key}#{prop}");
        let last = self.view.tweens.get(&id).map(|t| t.to);
        let target = match (color, last) {
            (Some(c), _) => [c.0 as f32, c.1 as f32, c.2 as f32, c.3 as f32],
            (None, Some(l)) => [l[0], l[1], l[2], 0.0],
            (None, None) => return None,
        };
        let v = self.fade(ctx, frame, key, prop, target, tr);
        (v[3] > 0.5).then(|| Rgba(v[0] as u8, v[1] as u8, v[2] as u8, v[3].round() as u8))
    }

    /// A thin bar on the right edge when the content is longer. Drag it, or
    /// press on the track to jump there and drag on from it.
    fn scrollbar(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, id: &str, p: &Placed) {
        let style = p.node.style.scrollbar;
        let Some(s) = self.view.scroll.get(id).filter(|s| s.max > 0.0) else {
            return;
        };
        let rect = egui::Rect::from_min_size(
            egui::pos2(p.rect.x, p.rect.y),
            egui::vec2(p.rect.w, p.rect.h),
        );
        let drag_id = egui::Id::new(("scrollbar", id));
        let dragging = ui.data(|d| d.get_temp::<bool>(drag_id)).unwrap_or(false);
        match style.show {
            ScrollbarShow::Hidden => return,
            ScrollbarShow::Hover => {
                if !dragging
                    && !ui
                        .input(|i| i.pointer.hover_pos())
                        .is_some_and(|q| rect.contains(q))
                {
                    return;
                }
            }
            ScrollbarShow::Auto => {}
        }
        let view = egui::Rect::from_min_size(
            egui::pos2(p.inner.x, p.inner.y),
            egui::vec2(p.inner.w, p.inner.h),
        );
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
                egui::Rect::from_min_size(
                    egui::pos2(start, view.max.y - style.width - style.inset),
                    egui::vec2(len, style.width),
                )
            } else {
                egui::Rect::from_min_size(
                    egui::pos2(rect.max.x - style.width - style.inset, start),
                    egui::vec2(style.width, len),
                )
            }
        };
        let mut offset = s.offset;
        // The grab is easier than the bar is thin: a track at least this wide.
        let grab = style.width.max(12.0);
        let track = if sideways {
            egui::Rect::from_min_max(egui::pos2(view.min.x, view.max.y - grab), view.max)
        } else {
            egui::Rect::from_min_max(
                egui::pos2(rect.max.x - grab - style.inset, view.min.y),
                egui::pos2(rect.max.x - style.inset, view.max.y),
            )
        };
        let resp = ui.interact(track, drag_id, Sense::click_and_drag());
        let along = |q: egui::Pos2| if sideways { q.x } else { q.y };
        let mut held = dragging;
        if (resp.drag_started() || (resp.is_pointer_button_down_on() && !dragging))
            && let Some(q) = resp.interact_pointer_pos()
        {
            let start = thumb_at(offset);
            // On the thumb, it stays under the pointer where it was
            // taken; on the track, its middle goes there.
            let hold = if (start..=start + len).contains(&along(q)) {
                along(q) - start
            } else {
                len / 2.0
            };
            ui.data_mut(|d| d.insert_temp(drag_id.with("hold"), hold));
            held = true;
        }
        if held {
            if let Some(q) = ui.input(|i| i.pointer.interact_pos()) {
                let hold = ui
                    .data(|d| d.get_temp::<f32>(drag_id.with("hold")))
                    .unwrap_or(len / 2.0);
                offset = ((along(q) - hold - lo) / room * max).clamp(0.0, max);
            }
            if !ui.input(|i| i.pointer.primary_down()) {
                held = false;
            }
        }
        ui.data_mut(|d| d.insert_temp(drag_id, held));
        if offset != s.offset {
            if let Some(s) = self.view.scroll.get_mut(id) {
                s.offset = offset;
                s.target = None;
            }
            ui.ctx().request_repaint();
        }
        painter.rect_filled(bar_at(thumb_at(offset)), style.radius, rgba(style.color));
    }

    /// A text field of its own in `area`: its text is kept here by id, Lua
    /// hears of each change and of enter.
    fn text_input_ui(
        &mut self,
        ui: &mut egui::Ui,
        t: &node::TextInput,
        area: egui::Rect,
        clip: egui::Rect,
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
            remember_text(buf, v.clone());
        }
        let mut text = std::mem::take(&mut buf.text);
        let font = FontId::new(
            t.text.size,
            if t.text.mono {
                FontFamily::Monospace
            } else {
                FontFamily::Proportional
            },
        );
        let hint = (self.group("Placeholder", false)).map_or(Color32::from_gray(0x66), rgba);
        let id = egui::Id::new(("text_input", &key));
        let rows = (area.height() / ui.ctx().fonts_mut(|f| f.row_height(&font)))
            .floor()
            .max(1.0) as usize;
        let edit = if t.multiline {
            TextEdit::multiline(&mut text).desired_rows(rows)
        } else {
            TextEdit::singleline(&mut text)
        };
        let edit = edit
            .id(id)
            .font(font)
            .frame(egui::Frame::NONE)
            .desired_width(area.width())
            .password(t.password)
            .interactive(!t.read_only)
            .text_color(rgba(t.text.color))
            .hint_text(egui::RichText::new(&t.placeholder).color(hint));
        let resp = ui.put(area, edit);
        let vs = &mut self.view;
        if vs.focus_request.as_deref() == Some(&key)
            || (t.focus && vs.focused_once.insert(key.clone()))
        {
            // One thing has the keyboard: a focused node would take egui's
            // focus back every frame (for screen readers).
            (vs.focus_request, vs.focused) = (None, None);
            // Asking again would reset the keys it keeps for itself (the
            // arrows) for a frame, in which egui moves the focus with them.
            if !resp.has_focus() {
                resp.request_focus();
            }
            if t.select {
                let mut state =
                    egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
                let all = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(text.chars().count()),
                );
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), id);
            }
        }
        if resp.has_focus() {
            frame.input_focus = Some(key.clone());
        }
        // A press on it is not one on the node around it.
        if resp.contains_pointer() && ui.input(|i| i.pointer.primary_pressed()) {
            frame.press_focus = None;
        }
        if let (true, Some(f)) = (resp.changed(), t.on_change) {
            frame.events.push((f, Arg::Text(text.clone())));
        }
        // Enter sends a one-line field; ctrl+enter a many-line one.
        let enter =
            ui.input(|i| i.key_pressed(egui::Key::Enter) && (!t.multiline || i.modifiers.command));
        let sent = enter && (resp.lost_focus() || (t.multiline && resp.has_focus()));
        if let (true, Some(f)) = (sent, t.on_submit) {
            frame.events.push((f, Arg::Text(text.clone())));
        }
        if let Some(buf) = self.view.inputs.get_mut(&key) {
            remember_text(buf, text);
        }
    }

    /// A picture in `area`, as its `fit` says; the pixels are read the first
    /// time it is asked for.
    pub(crate) fn image_ui(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        area: egui::Rect,
        corners: egui::CornerRadius,
        i: &node::Image,
    ) {
        use crate::node::Fit;
        use crate::preview::Pixels;
        match self.pixels(ui.ctx(), &i.source) {
            Pixels::Loading => {}
            Pixels::Failed(e) => {
                painter.text(
                    area.center(),
                    egui::Align2::CENTER_CENTER,
                    e,
                    FontId::proportional(12.0),
                    Color32::from_gray(0x99),
                );
            }
            Pixels::Ready(tex) => {
                let size = tex.size_vec2();
                let scale_to = |target: egui::Vec2, cover: bool| {
                    let k = (target.x / size.x, target.y / size.y);
                    let k = if cover { k.0.max(k.1) } else { k.0.min(k.1) };
                    size * k
                };
                let whole = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                let (rect, uv) = match i.fit {
                    Fit::Fill => (area, whole),
                    Fit::Contain => (
                        egui::Rect::from_center_size(area.center(), scale_to(area.size(), false)),
                        whole,
                    ),
                    Fit::None => (egui::Rect::from_center_size(area.center(), size), whole),
                    Fit::Cover => {
                        // The part of the picture the box shows.
                        let drawn = scale_to(area.size(), true);
                        let (kx, ky) = (area.width() / drawn.x, area.height() / drawn.y);
                        let uv =
                            egui::Rect::from_center_size(egui::pos2(0.5, 0.5), egui::vec2(kx, ky));
                        (area, uv)
                    }
                };
                painter.add(
                    egui::epaint::RectShape::filled(rect, corners, Color32::WHITE)
                        .with_texture(tex.id(), uv),
                );
            }
        }
    }

    /// The prompt's text field in `area`: egui's native editor on the
    /// shared prompt, so the cursor, selection, IME, history and
    /// attachments are the shared ones.
    fn input_ui(
        &mut self,
        ui: &mut egui::Ui,
        fonts: &Fonts,
        i: &Input,
        area: egui::Rect,
        clip: egui::Rect,
    ) {
        if !clip.intersects(area) {
            return;
        }
        let font = fonts.font(i);
        let hint = (self.group("Placeholder", false)).map_or(Color32::from_gray(0x66), rgba);
        self.sync_cursor(ui.ctx(), true);
        let rows = (area.height() / fonts.ctx.fonts_mut(|f| f.row_height(&font)))
            .floor()
            .max(1.0) as usize;
        // `bone.ui.prompt_highlight`: styles over ranges of the text, and
        // ghost text after the cursor.
        let cols = self
            .view
            .cell
            .map_or(80, |(w, _)| (area.width() / w) as u16);
        let marks = self.prompt_marks(cols);
        let base = egui::TextFormat {
            font_id: font.clone(),
            color: rgba(i.text.color),
            ..Default::default()
        };
        let styles: Vec<(usize, usize, usize, egui::TextFormat)> = (marks.ranges.iter())
            .map(|m| (m.row, m.from, m.to, self.mark_format(&m.hl, &base)))
            .collect();
        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            let job = highlighted(buf.as_str(), &base, &styles, wrap);
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let edit = TextEdit::multiline(&mut self.draft)
            .id(crate::prompt::edit_id())
            .font(font.clone())
            .frame(egui::Frame::NONE)
            .desired_width(area.width())
            .desired_rows(rows)
            .text_color(rgba(i.text.color))
            .hint_text(egui::RichText::new(&i.placeholder).color(hint))
            // Enter sends; Shift+Enter is the newline.
            .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter));
        let edit = if styles.is_empty() {
            edit
        } else {
            edit.layouter(&mut layouter)
        };
        let layout = egui::Layout::centered_and_justified(egui::Direction::TopDown);
        let out = ui
            .scope_builder(egui::UiBuilder::new().max_rect(area).layout(layout), |ui| {
                edit.show(ui)
            })
            .inner;
        let response = out.response.response.clone();
        // Ghost text when the cursor is at the end of its line.
        if let (Some(ghost), Some(range)) = (marks.ghost.as_deref(), out.cursor_range)
            && response.has_focus()
            && !ghost.is_empty()
            && range.is_empty()
            && self.cursor_at_line_end(range.primary.index.into())
        {
            let at = out.galley.pos_from_cursor(range.primary);
            let color = (self.group(&marks.ghost_hl, false)).map_or(hint, rgba);
            let line = ghost.lines().next().unwrap_or_default();
            ui.painter().with_clip_rect(out.text_clip_rect).text(
                out.galley_pos + at.min.to_vec2(),
                egui::Align2::LEFT_TOP,
                line,
                font,
                color,
            );
        }
        // A click on the prompt gives it the keyboard back from a panel.
        if response.clicked() && self.focused_panel().is_some() {
            let _ = bone_ui::lua::panels::focus_panel(self, None);
        }
        self.sync_cursor(ui.ctx(), false);
        self.prompt_changed();
        // Typed this frame: the highlights follow on the next.
        if !self.marks_fresh() {
            ui.ctx().request_repaint();
        }
        // A node with the keyboard focus has the keys, not the prompt.
        if self.view.focused_node().is_some() {
            response.surrender_focus();
        } else if !response.has_focus() && !ui.ctx().memory(|m| m.focused().is_some()) {
            response.request_focus();
        }
    }
}

impl Gui {
    /// How a `prompt_highlight` range in group `hl` is drawn over `base`.
    fn mark_format(&self, hl: &str, base: &egui::TextFormat) -> egui::TextFormat {
        let s = self.style(hl);
        let color = s.fg.map_or(base.color, rgba);
        egui::TextFormat {
            color,
            background: self.group(hl, true).map_or(Color32::TRANSPARENT, rgba),
            italics: s.italic,
            underline: if s.underline {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            },
            ..base.clone()
        }
    }

    /// Whether char offset `at` of the prompt ends its line.
    fn cursor_at_line_end(&self, at: usize) -> bool {
        self.draft.chars().nth(at).is_none_or(|c| c == '\n')
    }
}

/// `text` laid out in `base`, with `marks` (row, chars from..to, format)
/// over it; a later mark wins where two meet.
fn highlighted(
    text: &str,
    base: &egui::TextFormat,
    marks: &[(usize, usize, usize, egui::TextFormat)],
    wrap: f32,
) -> egui::text::LayoutJob {
    // The format of each char: a mark's index, or none for the base.
    let mut by_char: Vec<Option<usize>> = vec![None; text.chars().count()];
    let mut starts = vec![0];
    let mut lens = Vec::new();
    for line in text.split('\n') {
        let len = line.chars().count();
        lens.push(len);
        starts.push(starts.last().copied().unwrap_or(0) + len + 1);
    }
    for (k, (row, from, to, _)) in marks.iter().enumerate() {
        let (Some(&start), Some(&len)) = (starts.get(*row), lens.get(*row)) else {
            continue;
        };
        let (a, b) = ((*from).min(len), (*to).min(len));
        if a < b {
            by_char[start + a..start + b].fill(Some(k));
        }
    }
    let mut job = egui::text::LayoutJob::default();
    let format = |m: Option<usize>| m.map_or(base, |k| &marks[k].3).clone();
    let mut run = (0, by_char.first().copied().flatten());
    for (i, (byte, _)) in text.char_indices().enumerate() {
        if by_char[i] != run.1 {
            job.append(&text[run.0..byte], 0.0, format(run.1));
            run = (byte, by_char[i]);
        }
    }
    job.append(&text[run.0..], 0.0, format(run.1));
    job.wrap.max_width = wrap;
    job
}

fn widget_type(r: node::Role) -> egui::WidgetType {
    use egui::WidgetType as W;
    match r {
        node::Role::Button => W::Button,
        node::Role::Link => W::Link,
        node::Role::Checkbox => W::Checkbox,
        node::Role::Radio => W::RadioButton,
        node::Role::Option => W::SelectableLabel,
        node::Role::Image => W::Image,
        node::Role::Dialog => W::Window,
        node::Role::Text | node::Role::None => W::Label,
    }
}
