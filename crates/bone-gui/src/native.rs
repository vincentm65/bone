//! The window, on GPUI. A frame is drawn only when something asks for one:
//! input, a message from the core, or a deadline a frame set (an animation,
//! a timer, a fade); in between nothing is drawn. Each frame runs the GUI on
//! the input gathered since the last, and paints its draw list.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bone_proto::Connection;
use gpui::{
    App, AppContext, Application, Bounds, ClipboardItem, ContentMask, Context, CursorStyle,
    ElementInputHandler, Entity, EntityInputHandler, ExternalPaths, FocusHandle, Focusable,
    Hitbox, HitboxBehavior, InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent,
    MouseUpEvent, NavigationDirection, ParentElement, PathBuilder, Pixels, Render, RenderImage,
    ScrollDelta, ScrollWheelEvent, Styled, Subscription, Task, TitlebarOptions, UTF16Selection,
    Window, WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowHandle, WindowOptions,
    canvas, div, point, px, size,
};

use crate::backend::draw::{DrawList, Shape};
use crate::backend::gpui::{FontNames, GpuiShaper, Laid, hsla};
use crate::backend::input::{Button, Event, FrameInput, Key, Modifiers, WheelUnit};
use crate::backend::text::{FontSet, FontsApplied, Shaper};
use crate::backend::{Backend, Cursor, Rect, WindowCmd, pos, vec2};
use crate::node::{Corners, Gradient, Rgba};
use crate::node::WindowControl;
use crate::{Gui, GuiOptions, Titlebar};

/// The window's side of a frame: GPUI's text system and fonts.
struct GpuiBackend {
    shaper: GpuiShaper,
    /// Font files added so far, by path, and the family each is.
    loaded: HashMap<String, Option<String>>,
}

impl Backend for GpuiBackend {
    fn shaper(&self) -> &dyn Shaper {
        &self.shaper
    }

    fn set_fonts(&mut self, set: &FontSet) -> FontsApplied {
        let mut errors = Vec::new();
        let text = self.shaper.text.clone();
        let mut load = |path: &str| -> Option<String> {
            if let Some(f) = self.loaded.get(path) {
                return f.clone();
            }
            let family = crate::fonts::read(path).and_then(|bytes| {
                let name = family_name(&bytes)?;
                text.add_fonts(vec![bytes.into()]).ok()?;
                Some(name)
            });
            self.loaded.insert(path.to_owned(), family.clone());
            family
        };
        let mut names = FontNames::default();
        match set.mono.as_deref() {
            Some(p) => match load(p) {
                Some(f) => names.mono = f.into(),
                None => errors.push(p.to_owned()),
            },
            None => {
                if let Some(f) = load(crate::fonts::MONO) {
                    names.mono = f.into();
                }
            }
        }
        // The built-in regular face brings its medium weight along.
        if set.sans[0].as_deref() == Some(crate::fonts::SANS[0]) {
            load(crate::fonts::SANS_MEDIUM);
        }
        let mut faces = [false; 3];
        for (i, path) in set.sans.iter().enumerate() {
            let Some(path) = path else { continue };
            match load(path) {
                Some(f) => {
                    faces[i] = true;
                    match i {
                        0 => names.sans = f.into(),
                        1 => names.bold = Some(f.into()),
                        _ => names.italic = Some(f.into()),
                    }
                }
                None => errors.push(path.clone()),
            }
        }
        // Symbol fallbacks are found by the text system itself.
        for path in &set.symbols {
            load(path);
        }
        for (name, path) in &set.families {
            match load(path) {
                Some(f) => {
                    names.named.insert(name.clone(), f.into());
                }
                None => errors.push(path.clone()),
            }
        }
        self.shaper.fonts = names;
        FontsApplied {
            faces: (faces[1], faces[2]),
            errors,
        }
    }
}

/// The family name a font file gives itself.
fn family_name(bytes: &[u8]) -> Option<String> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    let names = face.names();
    let find = |id: u16| {
        names
            .into_iter()
            .filter(|n| n.name_id == id && n.is_unicode())
            .find_map(|n| n.to_string())
    };
    find(ttf_parser::name_id::TYPOGRAPHIC_FAMILY).or_else(|| find(ttf_parser::name_id::FAMILY))
}

/// The window's one view: the GUI, and the input gathered for its next
/// frame.
struct Root {
    gui: Gui,
    backend: GpuiBackend,
    focus: FocusHandle,
    events: Vec<Event>,
    mods: Modifiers,
    title: String,
    /// Where text is typed, for the input method.
    ime: Option<Rect>,
    /// The frame a deadline asked for.
    timer: Option<(Instant, Task<()>)>,
    /// Pictures uploaded, by id, and when each was last drawn.
    images: HashMap<u64, (Arc<RenderImage>, Instant)>,
    /// The title bar's parts the system acts on, from the last frame, in
    /// paint order: outermost (the drag area) first.
    controls: Vec<(WindowControl, Rect)>,
    _subscriptions: Vec<Subscription>,
    _messages: Task<()>,
}

impl Root {
    fn new(mut gui: Gui, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        // A message from the core asks for a frame.
        let notify = Arc::new(tokio::sync::Notify::new());
        let n = notify.clone();
        gui.set_waker(Box::new(move || n.notify_one()));
        let messages = cx.spawn_in(window, async move |this, cx| {
            loop {
                notify.notified().await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        });
        let quit = cx.on_app_quit(|root: &mut Root, _cx| {
            bone_ui::lua::plugins::run_shutdown(&mut root.gui, None);
            async {}
        });
        let active = cx.observe_window_activation(window, |_, _, cx| cx.notify());
        gui.renderer = Some("gpui".into());
        Root {
            gui,
            backend: GpuiBackend {
                shaper: GpuiShaper {
                    text: window.text_system().clone(),
                    fonts: FontNames::default(),
                },
                loaded: HashMap::new(),
            },
            focus,
            events: Vec::new(),
            mods: Modifiers::NONE,
            title: String::new(),
            ime: None,
            timer: None,
            images: HashMap::new(),
            controls: Vec::new(),
            _subscriptions: vec![quit, active],
            _messages: messages,
        }
    }

    fn key_down(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let m = mods(&e.keystroke.modifiers);
        let k = &e.keystroke;
        self.mods = m;
        // ctrl+v and shift+insert paste the clipboard's text.
        let paste = (k.key == "v" && m.command() && !m.alt && !m.shift)
            || (k.key == "insert" && m.shift && !m.ctrl);
        if paste && let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
            self.push(Event::Paste(t.replace("\r\n", "\n")), cx);
            return;
        }
        if let Some(key) = key_of(&k.key) {
            self.push(
                Event::Key {
                    key,
                    pressed: true,
                    repeat: e.is_held,
                    mods: m,
                },
                cx,
            );
        }
    }

    fn push(&mut self, e: Event, cx: &mut Context<Self>) {
        self.events.push(e);
        cx.notify();
    }

    /// Run a frame in `bounds` and paint it.
    fn frame(&mut self, bounds: Bounds<Pixels>, hitbox: &Hitbox, window: &mut Window, cx: &mut Context<Self>) {
        let screen = Rect::from_xywh(
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
        );
        let mut input = FrameInput::new(screen);
        input.events = std::mem::take(&mut self.events);
        input.scale = window.scale_factor();
        input.focused = window.is_window_active();
        input.fullscreen = window.is_fullscreen();
        input.maximized = window.is_maximized();
        input.mods = self.mods;
        self.backend.shaper.text = window.text_system().clone();
        let out = self.gui.frame(input, &mut self.backend);
        self.paint(&out.draw, window);
        window.set_cursor_style(cursor(out.cursor), hitbox);
        self.ime = out.ime;
        if let Some(t) = out.copy {
            cx.write_to_clipboard(ClipboardItem::new_string(t));
        }
        if let Some(t) = out.title
            && t != self.title
        {
            window.set_window_title(&t);
            self.title = t;
        }
        for cmd in out.window {
            match cmd {
                WindowCmd::Size(s) => window.resize(size(px(s.x), px(s.y))),
                WindowCmd::Fullscreen(on) => {
                    if on != window.is_fullscreen() {
                        window.toggle_fullscreen();
                    }
                }
                WindowCmd::Maximized(on) => {
                    if on != window.is_maximized() {
                        window.zoom_window();
                    }
                }
                WindowCmd::Minimized(true) => window.minimize_window(),
                WindowCmd::Minimized(false) => {}
                WindowCmd::TitlebarDoubleClick => window.titlebar_double_click(),
            }
        }
        // The title bar's parts the system acts on (Windows: dragging, snap
        // layouts, the caption buttons), placed by the next frame's prepaint.
        // When they move (the first frame, a resize), that frame comes at
        // once, so the system's hit test never waits on the pointer.
        if self.gui.titlebar == Some(Titlebar::Windows) && self.controls != out.controls {
            self.controls = out.controls.clone();
            window.request_animation_frame();
        }
        if out.close {
            cx.quit();
            return;
        }
        // The next frame: at once, at a deadline, or on input.
        if let Some(after) = out.repaint_after {
            // A window in the background animates slowly.
            let after = if window.is_window_active() {
                after
            } else {
                after.max(Duration::from_millis(500))
            };
            if after.is_zero() {
                window.request_animation_frame();
            } else {
                let at = Instant::now() + after;
                if self.timer.as_ref().is_none_or(|(t, _)| at < *t || *t <= Instant::now()) {
                    let task = cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor().timer(after).await;
                        let _ = this.update(cx, |_, cx| cx.notify());
                    });
                    self.timer = Some((at, task));
                }
            }
        }
        // Pictures not drawn for a while are let go.
        let now = Instant::now();
        let stale: Vec<u64> = (self.images.iter())
            .filter(|(_, (_, used))| now.duration_since(*used) > Duration::from_secs(30))
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some((img, _)) = self.images.remove(&id) {
                let _ = window.drop_image(img);
            }
        }
    }

    fn paint(&mut self, list: &DrawList, window: &mut Window) {
        let now = Instant::now();
        for item in &list.items {
            let mask = ContentMask {
                bounds: bounds(item.clip),
            };
            let o = item.opacity;
            window.with_content_mask(Some(mask), |window| match &item.shape {
                Shape::Rect {
                    rect,
                    corners: c,
                    fill,
                } => window.paint_quad(gpui::fill(bounds(*rect), hsla(faded(*fill, o))).corner_radii(radii(*c))),
                Shape::Stroke {
                    rect,
                    corners: c,
                    width,
                    color,
                    outside,
                } => {
                    let (r, c) = if *outside {
                        (rect.expand(*width), grow(*c, *width))
                    } else {
                        (*rect, *c)
                    };
                    window.paint_quad(gpui::quad(
                        bounds(r),
                        radii(c),
                        gpui::transparent_black(),
                        px(*width),
                        hsla(faded(*color, o)),
                        gpui::BorderStyle::Solid,
                    ));
                }
                Shape::Shadow {
                    rect,
                    corners: c,
                    offset,
                    blur,
                    spread,
                    color,
                } => window.paint_shadows(
                    bounds(*rect),
                    radii(*c),
                    &[gpui::BoxShadow {
                        color: hsla(faded(*color, o)),
                        offset: point(px(offset.x), px(offset.y)),
                        blur_radius: px(*blur),
                        spread_radius: px(*spread),
                    }],
                ),
                Shape::Gradient {
                    rect,
                    corners: c,
                    gradient,
                } => paint_gradient(window, *rect, *c, gradient, o),
                Shape::Text { at, text, color } => {
                    let Some(laid) = text.handle.downcast_ref::<Laid>() else {
                        return;
                    };
                    // A span's background (a code span's chip) is a pill a
                    // little wider than its glyphs and a little shorter
                    // than its line, with soft corners.
                    for d in laid.decos.iter().filter(|d| d.under) {
                        let r = d.rect.translate(at.to_vec2());
                        let trim = if r.height() > 14.0 { 2.0 } else { 0.0 };
                        let r = crate::backend::Rect::from_xywh(
                            r.min.x - 3.0,
                            r.min.y + trim,
                            r.width() + 6.0,
                            r.height() - 2.0 * trim,
                        );
                        window.paint_quad(
                            gpui::fill(bounds(r), hsla(faded(d.color, o))).corner_radii(px(4.0)),
                        );
                    }
                    for g in &laid.glyphs {
                        let origin = point(px(at.x + g.x), px(at.y + g.y));
                        let c = hsla(faded(color.unwrap_or(g.color), o));
                        let _ = if g.emoji {
                            window.paint_emoji(origin, g.font, g.id, px(g.size))
                        } else {
                            window.paint_glyph(origin, g.font, g.id, px(g.size), c)
                        };
                    }
                    for d in laid.decos.iter().filter(|d| !d.under) {
                        let r = d.rect.translate(at.to_vec2());
                        let c = color.unwrap_or(d.color);
                        window.paint_quad(gpui::fill(bounds(r), hsla(faded(c, o))));
                    }
                }
                Shape::Image {
                    rect,
                    uv,
                    corners: c,
                    picture,
                } => {
                    let (img, used) = self.images.entry(picture.id).or_insert_with(|| {
                        // GPUI's pictures are BGRA.
                        let mut bgra = picture.rgba.clone();
                        for px in bgra.as_chunks_mut::<4>().0 {
                            px.swap(0, 2);
                        }
                        let buf = image::RgbaImage::from_raw(picture.width, picture.height, bgra)
                            .unwrap_or_default();
                        let frame = image::Frame::new(buf);
                        (Arc::new(RenderImage::new(smallvec::smallvec![frame])), now)
                    });
                    *used = now;
                    // Only the `uv` part shows: the whole picture is drawn
                    // larger, cut to the box.
                    let (uw, uh) = (uv.width().max(1e-6), uv.height().max(1e-6));
                    let full = Rect::from_xywh(
                        rect.min.x - uv.min.x / uw * rect.width(),
                        rect.min.y - uv.min.y / uh * rect.height(),
                        rect.width() / uw,
                        rect.height() / uh,
                    );
                    let mask = ContentMask {
                        bounds: bounds(rect.intersect(item.clip)),
                    };
                    let img = img.clone();
                    window.with_content_mask(Some(mask), |window| {
                        let _ = window.paint_image(bounds(full), radii(*c), img, 0, false);
                    });
                }
                Shape::Circle {
                    center,
                    radius,
                    fill,
                    stroke,
                } => {
                    let r = Rect::from_center_size(*center, vec2(radius * 2.0, radius * 2.0));
                    let c = Corners {
                        tl: *radius,
                        tr: *radius,
                        br: *radius,
                        bl: *radius,
                    };
                    let (w, sc) = stroke.unwrap_or((0.0, Rgba(0, 0, 0, 0)));
                    window.paint_quad(gpui::quad(
                        bounds(r),
                        radii(c),
                        hsla(faded(fill.unwrap_or(Rgba(0, 0, 0, 0)), o)),
                        px(w),
                        hsla(faded(sc, o)),
                        gpui::BorderStyle::Solid,
                    ));
                }
                Shape::Path {
                    points,
                    closed,
                    stroke,
                    fill,
                } => {
                    if points.len() < 2 {
                        return;
                    }
                    let build = |mut b: PathBuilder, close: bool| {
                        b.move_to(point(px(points[0].x), px(points[0].y)));
                        for q in &points[1..] {
                            b.line_to(point(px(q.x), px(q.y)));
                        }
                        if close {
                            b.close();
                        }
                        b.build().ok()
                    };
                    if let (Some(f), true) = (fill, points.len() >= 3)
                        && let Some(path) = build(PathBuilder::fill(), true)
                    {
                        window.paint_path(path, hsla(faded(*f, o)));
                    }
                    if let Some((w, c)) = stroke
                        && let Some(path) = build(PathBuilder::stroke(px(*w)), *closed)
                    {
                        window.paint_path(path, hsla(faded(*c, o)));
                    }
                }
            });
        }
    }
}

impl Focusable for Root {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Root {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let prepaint = this.clone();
        let focus = self.focus.clone();
        div()
            .size_full()
            .track_focus(&self.focus)
            // Keys go along the focus: they are heard on the focused box.
            .on_key_down(cx.listener(|root, e: &KeyDownEvent, _window, cx| root.key_down(e, cx)))
            .on_key_up(cx.listener(|root, e: &KeyUpEvent, _window, cx| {
                let m = mods(&e.keystroke.modifiers);
                if let Some(key) = key_of(&e.keystroke.key) {
                    root.mods = m;
                    root.push(
                        Event::Key {
                            key,
                            pressed: false,
                            repeat: false,
                            mods: m,
                        },
                        cx,
                    );
                }
            }))
            .on_modifiers_changed(cx.listener(|root, e: &ModifiersChangedEvent, _window, cx| {
                let m = mods(&e.modifiers);
                if root.mods != m {
                    root.mods = m;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|root, paths: &ExternalPaths, _window, cx| {
                for p in paths.paths() {
                    root.events.push(Event::DroppedFile(p.clone()));
                }
                cx.notify();
            }))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                        let controls = control_hitboxes(&prepaint.read(cx).controls, bounds, window);
                        (hitbox, controls)
                    },
                    move |bounds, (hitbox, controls), window, cx| {
                        for (area, hit) in controls {
                            window.insert_window_control_hitbox(area, hit);
                        }
                        listen(&this, window);
                        window.handle_input(&focus, ElementInputHandler::new(bounds, this.clone()), cx);
                        this.update(cx, |root, cx| root.frame(bounds, &hitbox, window, cx));
                    },
                )
                .size_full(),
            )
    }
}

/// This frame's listeners: the keyboard and the pointer, into the input
/// of the next frame.
fn listen(this: &Entity<Root>, window: &mut Window) {
    use gpui::DispatchPhase::Bubble;
    let r = this.clone();
    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _window, cx| {
        if phase == Bubble {
            let p = pos(f32::from(e.position.x), f32::from(e.position.y));
            r.update(cx, |root, cx| root.push(Event::PointerMoved(p), cx));
        }
    });
    let r = this.clone();
    window.on_mouse_event(move |e: &MouseExitEvent, phase, _window, cx| {
        if phase == Bubble {
            let _ = e;
            r.update(cx, |root, cx| root.push(Event::PointerGone, cx));
        }
    });
    let r = this.clone();
    window.on_mouse_event(move |e: &MouseDownEvent, phase, _window, cx| {
        if phase == Bubble {
            let ev = Event::PointerButton {
                pos: pos(f32::from(e.position.x), f32::from(e.position.y)),
                button: button(e.button),
                pressed: true,
                mods: mods(&e.modifiers),
            };
            r.update(cx, |root, cx| root.push(ev, cx));
        }
    });
    let r = this.clone();
    window.on_mouse_event(move |e: &MouseUpEvent, phase, _window, cx| {
        if phase == Bubble {
            let ev = Event::PointerButton {
                pos: pos(f32::from(e.position.x), f32::from(e.position.y)),
                button: button(e.button),
                pressed: false,
                mods: mods(&e.modifiers),
            };
            r.update(cx, |root, cx| root.push(ev, cx));
        }
    });
    let r = this.clone();
    window.on_mouse_event(move |e: &ScrollWheelEvent, phase, _window, cx| {
        if phase != Bubble {
            return;
        }
        let (delta, unit) = match e.delta {
            ScrollDelta::Pixels(p) => (vec2(f32::from(p.x), f32::from(p.y)), WheelUnit::Point),
            ScrollDelta::Lines(p) => (vec2(p.x, p.y), WheelUnit::Line),
        };
        let ev = Event::Wheel {
            delta,
            unit,
            mods: mods(&e.modifiers),
        };
        let p = pos(f32::from(e.position.x), f32::from(e.position.y));
        r.update(cx, |root, cx| {
            root.events.push(Event::PointerMoved(p));
            root.push(ev, cx)
        });
    });
}

/// Typing and the input method's text come through here.
impl EntityInputHandler for Root {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<std::ops::Range<usize>> {
        let n = self.gui.view.preedit.encode_utf16().count();
        (n > 0).then_some(0..n)
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.push(Event::Preedit(String::new()), cx);
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.events.push(Event::Preedit(String::new()));
        self.push(Event::Text(text.to_owned()), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.push(Event::Preedit(new_text.to_owned()), cx);
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.ime.map(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

fn bounds(r: Rect) -> Bounds<Pixels> {
    Bounds::new(
        point(px(r.min.x), px(r.min.y)),
        size(px(r.width().max(0.0)), px(r.height().max(0.0))),
    )
}

fn radii(c: Corners) -> gpui::Corners<Pixels> {
    gpui::Corners {
        top_left: px(c.tl),
        top_right: px(c.tr),
        bottom_right: px(c.br),
        bottom_left: px(c.bl),
    }
}

fn grow(c: Corners, by: f32) -> Corners {
    let g = |v: f32| if v > 0.0 { v + by } else { 0.0 };
    Corners {
        tl: g(c.tl),
        tr: g(c.tr),
        br: g(c.br),
        bl: g(c.bl),
    }
}

fn faded(c: Rgba, opacity: f32) -> Rgba {
    crate::backend::draw::faded(c, opacity)
}

/// A gradient: GPUI blends two stops; more are slabs of two along an
/// upright or level box, else the first and last.
fn paint_gradient(window: &mut Window, r: Rect, c: Corners, g: &Gradient, o: f32) {
    let stop = |(at, color): (f32, Rgba)| gpui::linear_color_stop(hsla(faded(color, o)), at);
    let (Some(first), Some(last)) = (g.stops.first(), g.stops.last()) else {
        return;
    };
    let square = c == Corners::default();
    let angle = g.angle.rem_euclid(360.0);
    let upright = [0.0, 90.0, 180.0, 270.0].contains(&angle);
    if g.stops.len() <= 2 || !square || !upright {
        let bg = gpui::linear_gradient(angle, stop(*first), stop(*last));
        window.paint_quad(gpui::fill(bounds(r), bg).corner_radii(radii(c)));
        return;
    }
    for w in g.stops.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b.0 <= a.0 {
            continue;
        }
        // The slab between the two stops, along the gradient.
        let slab = match angle as u32 {
            180 => Rect::from_xywh(r.min.x, r.min.y + r.height() * a.0, r.width(), r.height() * (b.0 - a.0)),
            0 => Rect::from_xywh(r.min.x, r.max.y - r.height() * b.0, r.width(), r.height() * (b.0 - a.0)),
            90 => Rect::from_xywh(r.min.x + r.width() * a.0, r.min.y, r.width() * (b.0 - a.0), r.height()),
            _ => Rect::from_xywh(r.max.x - r.width() * b.0, r.min.y, r.width() * (b.0 - a.0), r.height()),
        };
        let bg = gpui::linear_gradient(angle, stop((0.0, a.1)), stop((1.0, b.1)));
        window.paint_quad(gpui::fill(bounds(slab), bg));
    }
}

fn mods(m: &gpui::Modifiers) -> Modifiers {
    Modifiers {
        ctrl: m.control,
        alt: m.alt,
        shift: m.shift,
        logo: m.platform,
    }
}

fn button(b: MouseButton) -> Button {
    match b {
        MouseButton::Left => Button::Primary,
        MouseButton::Right => Button::Secondary,
        MouseButton::Middle => Button::Middle,
        MouseButton::Navigate(NavigationDirection::Back) => Button::Back,
        MouseButton::Navigate(NavigationDirection::Forward) => Button::Forward,
    }
}

/// GPUI's name of a key as the GUI's key.
fn key_of(k: &str) -> Option<Key> {
    Some(match k {
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "up" => Key::ArrowUp,
        "escape" => Key::Escape,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "enter" => Key::Enter,
        "space" => Key::Space,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        k => {
            if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                Key::F(n)
            } else {
                let mut chars = k.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Key::Char(c.to_ascii_lowercase()),
                    _ => return None,
                }
            }
        }
    })
}

fn cursor(c: Cursor) -> CursorStyle {
    use CursorStyle as S;
    match c {
        Cursor::Default | Cursor::Wait | Cursor::Progress | Cursor::Help | Cursor::ZoomIn | Cursor::ZoomOut => S::Arrow,
        Cursor::None => S::None,
        Cursor::PointingHand => S::PointingHand,
        Cursor::Text => S::IBeam,
        Cursor::VerticalText => S::IBeamCursorForVerticalLayout,
        Cursor::Grab => S::OpenHand,
        Cursor::Grabbing | Cursor::Move | Cursor::AllScroll => S::ClosedHand,
        Cursor::NotAllowed | Cursor::NoDrop => S::OperationNotAllowed,
        Cursor::Crosshair | Cursor::Cell => S::Crosshair,
        Cursor::Copy => S::DragCopy,
        Cursor::Alias => S::DragLink,
        Cursor::ResizeHorizontal => S::ResizeLeftRight,
        Cursor::ResizeVertical => S::ResizeUpDown,
        Cursor::ResizeNeSw => S::ResizeUpRightDownLeft,
        Cursor::ResizeNwSe => S::ResizeUpLeftDownRight,
        Cursor::ResizeColumn => S::ResizeColumn,
        Cursor::ResizeRow => S::ResizeRow,
    }
}

/// The top edge left to the system's resize when the window is not
/// maximized, in logical pixels (Windows' sizing frame, `SM_CYFRAME`).
const RESIZE_EDGE: f32 = 4.0;

/// The hitboxes of the title bar's parts the system acts on, placed before
/// the paint, as the last frame drew them (`parts`, in paint order).
///
/// Each one blocks the mouse from what is under it, the root's focus
/// tracking above all: GPUI moves the focus on a press and marks the press
/// handled (`prevent_default`), and Windows' backend then keeps a press on
/// the bar, the caption buttons or a resize edge from the system, which
/// left the window unable to move, resize, minimize or close. The pointer
/// events still reach the GUI (its listeners are the window's), so the
/// buttons' hover keeps working.
fn control_hitboxes(
    parts: &[(WindowControl, Rect)],
    bounds: Bounds<Pixels>,
    window: &mut Window,
) -> Vec<(WindowControlArea, Hitbox)> {
    if parts.is_empty() {
        return Vec::new();
    }
    let edge = if window.is_maximized() || window.is_fullscreen() { 0.0 } else { RESIZE_EDGE };
    if edge > 0.0 {
        // The top edge stays the system's (resize), and is no press of ours.
        let strip = Rect::from_xywh(
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.size.width),
            edge,
        );
        window.insert_hitbox(self::bounds(strip), HitboxBehavior::BlockMouse);
    }
    let top = f32::from(bounds.origin.y) + edge;
    // In paint order, so the innermost (a caption button in the bar) is on
    // top and is the one the system finds.
    (parts.iter())
        .map(|(area, r)| {
            let mut r = *r;
            r.min.y = r.min.y.max(top);
            let hit = window.insert_hitbox(self::bounds(r), HitboxBehavior::BlockMouse);
            (control_area(*area), hit)
        })
        .collect()
}

fn control_area(c: WindowControl) -> WindowControlArea {
    match c {
        WindowControl::Drag => WindowControlArea::Drag,
        WindowControl::Min => WindowControlArea::Min,
        WindowControl::Max => WindowControlArea::Max,
        WindowControl::Close => WindowControlArea::Close,
    }
}

/// The traffic lights' place on macOS, from the window's top left: in the
/// middle of the title bar Lua draws (`bone.nodes.titlebar`, 34px tall).
const TRAFFIC_LIGHTS: (f32, f32) = (13.0, 10.0);

/// Open the GUI on `conn` and run it until the window closes. Call from
/// the main thread, inside a multi-threaded Tokio runtime.
pub async fn run(conn: Connection, opts: GuiOptions) -> Result<(), String> {
    run_with(conn, opts, false).await
}

/// [`run`], but `resident` serves the control socket later `bone --gui`
/// launches use to open their window in this process.
pub async fn run_with(conn: Connection, opts: GuiOptions, resident: bool) -> Result<(), String> {
    let gui = Gui::connect(conn, opts).await?;
    gui.watch_config();
    tokio::task::block_in_place(move || open(gui, resident))
}

fn open(gui: Gui, resident: bool) -> Result<(), String> {
    let (unlink, asks) = start_resident(resident).unzip();
    let _unlink = unlink;
    Application::new().run(move |cx: &mut App| {
        if let Err(e) = present(cx, gui) {
            eprintln!("bone: cannot open a window: {e}");
            cx.quit();
            return;
        }
        if let Some(mut rx) = asks {
            cx.spawn(async move |cx| {
                while let Some(reply) = rx.recv().await {
                    let msg = cx.update(show_window);
                    let _ = reply.send(msg.unwrap_or_else(|_| "err gone\n".into()));
                }
            })
            .detach();
        }
        cx.on_window_closed(move |cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        #[cfg(target_os = "macos")]
        dock_icon();
        cx.activate(true);
    });
    Ok(())
}

/// A window's size, from Lua, or the default.
fn window_size(gui: &Gui) -> (f32, f32) {
    (
        gui.window_info["width"]
            .as_f64()
            .filter(|w| *w > 0.0)
            .unwrap_or(960.0) as f32,
        gui.window_info["height"]
            .as_f64()
            .filter(|h| *h > 0.0)
            .unwrap_or(720.0) as f32,
    )
}

/// Closing the last window would end the process and drop the GPU.
/// Bring this process's window onto the workspace the user is on.
fn reveal_window() {
    let pid = std::process::id();
    let target = format!("+0,pid:{pid}");
    let focus = format!("pid:{pid}");
    hyprctl(&["dispatch", "movetoworkspace", &target]);
    hyprctl(&["dispatch", "focuswindow", &focus]);
}

fn hyprctl(args: &[&str]) {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let _ = std::process::Command::new("hyprctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Open `gui` in a new window.
fn present(cx: &mut App, mut gui: Gui) -> Result<WindowHandle<Root>, String> {
    let (w, h) = window_size(&gui);
    // See-through only if asked for when the window opens: an opaque
    // window draws faster, and works without a compositor.
    let transparent = gui.opacity() < 1.0;
    gui.transparent = transparent;
    // The window draws its own title bar where the system lets it, unless
    // the `titlebar` option asks for the system's.
    let titlebar = Titlebar::native().filter(|_| gui.bool_option("titlebar", true));
    gui.titlebar = titlebar;
    let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("New chat".into()),
            appears_transparent: titlebar.is_some(),
            traffic_light_position: (titlebar == Some(Titlebar::Mac))
                .then(|| point(px(TRAFFIC_LIGHTS.0), px(TRAFFIC_LIGHTS.1))),
        }),
        window_background: if transparent {
            WindowBackgroundAppearance::Transparent
        } else {
            WindowBackgroundAppearance::Opaque
        },
        app_id: Some("bone".into()),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| cx.new(|cx| Root::new(gui, window, cx)))
        .map_err(|e| e.to_string())
}

/// Focus the window already open. Without one this process is on its way
/// out, and the refused asker becomes the GUI itself.
fn show_window(cx: &mut App) -> String {
    let Some(handle) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
        return "err closing\n".into();
    };
    reveal_window();
    let _ = handle.update(cx, |_view, window, _| window.activate_window());
    cx.activate(true);
    "ok\n".into()
}

/// Another `bone --gui`'s ask to open the window here: the reply line it
/// is owed ("ok" or "err …").
type Ask = std::sync::mpsc::Sender<String>;

/// Removes the control socket when this process leaves.
struct Unlink(PathBuf);

impl Drop for Unlink {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Where a running GUI listens for `open` and `quit`.
fn gui_socket_path() -> PathBuf {
    let mut path = bone_proto::transport::default_socket_path();
    path.set_file_name("gui.sock");
    path
}

/// Ask the GUI that is already running to show its window. False when none
/// is, which is also when this process should become that GUI.
pub fn request_window() -> bool {
    #[cfg(unix)]
    {
        let path = gui_socket_path();
        // A refused connect may be a dead GUI's socket or a live one with
        // a full backlog; bind_gui_socket tells them apart, so a stale
        // file is not removed here.
        let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&path) else {
            return false;
        };
        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
        // Longer than the server's 5s wait for its event loop to answer.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(8)));
        if stream.write_all(b"open\n").is_err() {
            return false;
        }
        let mut buf = [0u8; 64];
        let n = stream.read(&mut buf).unwrap_or(0);
        return buf[..n].starts_with(b"ok");
    }
    #[cfg(not(unix))]
    false
}

#[cfg(unix)]
fn bind_gui_socket() -> Option<std::os::unix::net::UnixListener> {
    let path = gui_socket_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // A dead GUI's socket is replaced at once. A live holder that would
    // not show a window is a GUI on its way out: give it a moment to
    // leave (unlinking its socket) before giving up on being resident.
    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        match std::os::unix::net::UnixListener::bind(&path) {
            Ok(listener) => return Some(listener),
            Err(_) if std::os::unix::net::UnixStream::connect(&path).is_err() => {
                let _ = std::fs::remove_file(&path);
                return std::os::unix::net::UnixListener::bind(&path).ok();
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}

/// The control socket, when this process is the one later launches talk to.
fn start_resident(
    resident: bool,
) -> Option<(Unlink, tokio::sync::mpsc::UnboundedReceiver<Ask>)> {
    if !resident {
        return None;
    }
    #[cfg(not(unix))]
    {
        return None;
    }
    #[cfg(unix)]
    {
        let listener = bind_gui_socket()?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || serve_gui_socket(listener, tx));
        Some((Unlink(gui_socket_path()), rx))
    }
}

#[cfg(unix)]
fn serve_gui_socket(
    listener: std::os::unix::net::UnixListener,
    tx: tokio::sync::mpsc::UnboundedSender<Ask>,
) {
    for conn in listener.incoming() {
        let Ok(mut conn) = conn else { continue };
        let tx = tx.clone();
        // A thread each: one stalled connector must not hold up the next.
        std::thread::spawn(move || {
            let _ = conn.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = conn.set_write_timeout(Some(Duration::from_secs(3)));
            let mut buf = [0u8; 16];
            let n = conn.read(&mut buf).unwrap_or(0);
            if std::str::from_utf8(&buf[..n]).unwrap_or("").trim() != "open" {
                let _ = conn.write_all(b"err unknown\n");
                return;
            }
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            if tx.send(reply_tx).is_err() {
                let _ = conn.write_all(b"err gone\n");
                return;
            }
            let reply = reply_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| "err timeout\n".into());
            let _ = conn.write_all(reply.as_bytes());
        });
    }
}


/// The app's icon in the Dock and the app switcher. A bare binary has
/// none of its own (an app bundle's `Info.plist` would give it one).
#[cfg(target_os = "macos")]
fn dock_icon() {
    use cocoa::appkit::{NSApp, NSApplication, NSImage};
    use cocoa::base::nil;
    use cocoa::foundation::NSData;
    const PNG: &[u8] = include_bytes!("../assets/icon-512.png");
    // SAFETY: on the main thread, after the application exists (in the
    // callback of `Application::run`); the image is retained by AppKit.
    unsafe {
        let data = NSData::dataWithBytes_length_(nil, PNG.as_ptr().cast(), PNG.len() as u64);
        let image = NSImage::initWithData_(NSImage::alloc(nil), data);
        if image != nil {
            NSApp().setApplicationIconImage_(image);
        }
    }
}
