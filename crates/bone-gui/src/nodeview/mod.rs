//! The node view: `bone.gui.view(fn)` registers a function that returns the
//! window as a tree of nodes (`node.rs`); the window is that tree, laid out
//! by `flex.rs` and painted here. Without one, `bone.gui.fallback`'s view
//! says so.
//!
//! Each frame: the wheel scrolls what was under the pointer last frame; Lua
//! builds the tree when something changed (else the last one is used); it
//! is laid out, scroll offsets applied, the items of lists in view made
//! (the chat's kept while they have not changed), painted, the keyboard
//! focus settled, and the handlers that fired are called. Lua's windows
//! (`bone.ui.popup`) are placed here too, for Lua to draw as nodes.
//!
//! The pieces live beside this file: [`ops`] (the `bone.gui` ops),
//! [`popups`] (cell-sized windows), [`list`] (virtual lists), [`focus`]
//! (keyboard and wheel), [`paint`] (drawing) and [`shapes`].

mod edit;
mod focus;
mod list;
mod ops;
mod paint;
mod popups;
mod shapes;

pub(crate) use list::{ChatMove, ListState};
pub(crate) use popups::cell_size;
pub(crate) use ops::{dispatch, window_spec};

use crate::node::Sink;
use list::{Windows, set_totals};
use std::collections::HashMap;
use std::rc::Rc;

use bone_ui::lua::Host;
use mlua::Value;

use crate::backend::input::{Event, FrameInput, Modifiers};
use crate::backend::interact::Pointer;
use crate::backend::text::{Family, Job, RunStyle, Shaper};
use crate::backend::{FrameOutput, Rect as Area, Shape, pos};

use crate::Gui;
use crate::flex::{self, Metrics, Placed, Rect, Size};
use crate::node::{
    self, Colors, GroupStyle, Input, Kind, Node, Overflow, Rgba, Text, TextStyle,
};
use crate::view::color;
use bone_ui::style::Modifier;

/// A scroll container's state, kept by its `id`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scroll {
    pub offset: f32,
    pub max: f32,
    pub viewport: f32,
    /// Where Lua asked to go (`bone.gui.scroll`), taken next frame.
    pub target: Option<Target>,
    /// This frame went where Lua asked: a list does not follow its end over it.
    pub held: bool,
    /// The pointer is holding this container's scrollbar. Content growth must
    /// not pull a thumb back to the end while it is being dragged.
    pub dragging: bool,
    /// Where the scrollbar was taken: the pointer along it, the offset,
    /// the max and the track's free room then.
    pub grab: Option<[f32; 4]>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Target {
    To(f32),
    End,
}

/// What was under the pointer last frame, for the wheel.
#[derive(Clone, Debug)]
pub(crate) struct Hit {
    pub(in crate::nodeview) rect: Area,
    pub(in crate::nodeview) scroll: Option<String>,
    /// A row, which the sideways wheel moves.
    pub(in crate::nodeview) sideways: bool,
    pub(in crate::nodeview) on_scroll: Option<u64>,
}

/// A node the keyboard can reach, in the order they were drawn.
#[derive(Clone, Debug)]
pub(crate) struct Focusable {
    pub key: String,
    pub on_key: Option<u64>,
    pub click: Option<u64>,
    /// The `block_input` box it is in (a modal), numbered as drawn.
    pub scope: Option<usize>,
    pub autofocus: bool,
    pub key_guard: f32,
}

/// A text input's text, and the value Lua last gave it.
pub(crate) struct InputBuf {
    pub text: String,
    /// The selection's other end and the cursor, in chars.
    pub anchor: usize,
    pub cursor: usize,
    /// `text` the last time it changed, for measuring rows without copying.
    measure: Rc<str>,
    pub(in crate::nodeview) pushed: Option<String>,
}

impl Default for InputBuf {
    fn default() -> Self {
        InputBuf {
            text: String::new(),
            anchor: 0,
            cursor: 0,
            measure: Rc::from(""),
            pushed: None,
        }
    }
}

pub(in crate::nodeview) fn remember_text(buf: &mut InputBuf, text: String) {
    if buf.measure.as_ref() != text.as_str() {
        buf.measure = Rc::from(text.as_str());
    }
    buf.text = text;
}

/// A value on its way: `[red, green, blue, alpha]` or `[opacity, 0, 0, 0]`.
pub(in crate::nodeview) struct Tween {
    pub(in crate::nodeview) from: [f32; 4],
    pub(in crate::nodeview) to: [f32; 4],
    pub(in crate::nodeview) at: std::time::Instant,
}

/// What belongs to one Lua state: its functions and what was made from
/// them. A reload swaps it out whole, so the old state's view keeps working
/// until the new configuration is taken, and none of it outlives its state.
#[derive(Default)]
pub struct Bound {
    /// The view (`bone.gui.view`) and the one drawn when it fails or is
    /// unset (`bone.gui.fallback`): stored callbacks.
    pub(crate) func: Option<u64>,
    pub(crate) fallback: Option<u64>,
    /// The last error the view was reported for.
    pub(crate) error: Option<String>,
    /// What Lua's windows and panels draw as nodes (their spec's `node`),
    /// by `popup:<id>` or `panel:<id>`: a stored callback.
    pub(crate) window_nodes: HashMap<String, u64>,
    /// Families Lua registered (`bone.gui.font`): name and file.
    pub(crate) extra_fonts: Vec<(String, String)>,
    pub(crate) canvases: crate::canvas::Canvases,
    tree: Option<Node>,
    /// The functions of the tree and the items made (see `node::Sink`), and
    /// the table before the last rebuild, which kept chat items' are copied
    /// from.
    pub(crate) sink: Option<Sink>,
    pub(in crate::nodeview) prev_sink: Option<mlua::Table>,
}

/// The fonts last applied: the monospace file, its size, the proportional
/// faces, and the families Lua registered.
pub type FontWant = (String, f32, [String; 3], Vec<(String, String)>);

/// What the view keeps between frames, through Lua reloads too (scroll
/// offsets, measured list heights, the focus). Lua builds the tree only when
/// something changed (`dirty`); between those the last tree is laid out
/// again, so a long transcript costs Lua nothing while it sits still.
/// Whether Lua's `mouse` handlers took the press, drag or release under
/// way from the nodes.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseTaken {
    #[default]
    No,
    /// At the press: the nodes never see the button go down.
    Whole,
    /// After the nodes saw the press: they see no more motion, then the release
    /// where the pointer last was for it, and the nodes hear none of it.
    Rest,
}

#[derive(Default)]
pub struct ViewState {
    pub(crate) bound: Bound,
    /// Lua should build the tree again.
    pub dirty: bool,
    pub(crate) scroll: HashMap<String, Scroll>,
    pub(crate) lists: HashMap<String, ListState>,
    pub(crate) hits: Vec<Hit>,
    /// A character cell's width and height (logical pixels) last frame:
    /// the unit Lua's windows are sized in and `resize` counts.
    pub cell: Option<(f32, f32)>,
    /// The window's width in columns, as last drawn, and its size in
    /// columns and rows as `resize` last reported it.
    pub screen_cols: u16,
    pub screen_size: (u16, u16),
    /// The font file and size applied last (`bone.o.font`, `font_size`).
    pub fonts: Option<FontWant>,
    /// Whether the proportional font has a bold and an italic face
    /// (`sans-bold`, `sans-italic`); without them bold is a brighter color.
    pub faces: (bool, bool),
    pub hover: std::collections::HashSet<String>,
    /// Nodes the primary button was down on last frame, and the one with
    /// the focus (the last clicked).
    pub pressed: std::collections::HashSet<String>,
    pub focused: Option<String>,
    /// The focus came from the keyboard, so it is shown (a ring).
    pub(crate) focus_visible: bool,
    /// The nodes the keyboard can reach, as last drawn.
    pub(crate) focusables: Vec<Focusable>,
    /// The topmost `block_input` box: the focus stays in it.
    pub(crate) trap: Option<usize>,
    /// The `autofocus` nodes on screen, which took the focus as they came.
    pub(in crate::nodeview) autofocused: std::collections::HashSet<String>,
    /// The `on_key` of each text input, by id: it sees the keys the input
    /// does not type with first.
    pub(crate) input_keys: HashMap<String, u64>,
    /// Keys are ignored until then (`key_guard`).
    pub(crate) guard_until: Option<std::time::Instant>,
    /// The text inputs' text, by id.
    pub(crate) inputs: HashMap<String, InputBuf>,
    /// Values being faded (`transition`), by node and property.
    pub(in crate::nodeview) tweens: HashMap<String, Tween>,
    /// The text input that has the keyboard, if one does (not the prompt).
    pub input_focus: Option<String>,
    /// Asked to take the keyboard (`bone.gui.focus`).
    pub(in crate::nodeview) focus_request: Option<String>,
    /// Inputs that have taken the keyboard on appearing (`focus = true`).
    pub(in crate::nodeview) focused_once: std::collections::HashSet<String>,
    pub size: (f32, f32),
    /// The window's area, and where each node with an id was: windows
    /// anchor to `chat` and `prompt`, and `bone.gui.rect` reads any.
    pub(crate) area: Option<Area>,
    pub(crate) leaves: Vec<(String, Area)>,
    /// The colorscheme and options last built with.
    rev: u64,
    /// When `bone.gui.invalidate(ms)` wants the next build.
    pub(crate) wake: Option<std::time::Instant>,
    /// The button held for the `mouse` event and the cell it was last at.
    pub(crate) mouse_held: Option<(&'static str, (u16, u16))>,
    /// Whether a `mouse` handler took the gesture under way, and where
    /// the nodes last saw the pointer (see `route_pointer`).
    pub(crate) mouse_taken: MouseTaken,
    pub(crate) mouse_seen: Option<crate::backend::Pos>,
    /// The pointer as the nodes hear it.
    pub(crate) pointer: Pointer,
    /// The monospace cell of the fonts applied last, before a frame has
    /// measured one.
    pub(crate) mono_cell: Option<(f32, f32)>,
    /// Text being composed by the input method, in the field with the
    /// keyboard.
    pub(crate) preedit: String,
    /// The text fields' editing state (the prompt's under `""`): where the
    /// pointer pressed, and how far the field is scrolled.
    pub(crate) edits: HashMap<String, crate::nodeview::edit::EditState>,
    /// A text node's selection: its key, and the char range; and the text
    /// it selects.
    pub(crate) text_selection: Option<(String, usize, usize)>,
    pub(crate) selection_text: Option<String>,
    /// When the pointer came to rest over the node with a tooltip.
    pub(crate) tooltip_since: Option<(String, std::time::Instant)>,
    /// Wheel travel not scrolled yet that is short of a `wheelup` or
    /// `wheeldown` key, in points.
    pub(crate) wheel_rest: f32,
}

impl ViewState {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// The node with the keyboard focus, if one has it.
    pub(crate) fn focused_node(&self) -> Option<&Focusable> {
        let id = self.focused.as_ref()?;
        self.focusables.iter().find(|f| &f.key == id)
    }
    /// The table the tree's functions are in.
    pub(crate) fn sink_table(&self) -> Option<mlua::Table> {
        self.bound.sink.as_ref().map(|s| s.table.clone())
    }

    /// The Lua state is being replaced: take what belongs to it (to put
    /// back if the new one is refused). Made nodes hold the old state's
    /// callbacks, so they go with it; measured heights stay, so the chat
    /// does not snap to the top.
    pub(crate) fn unbind(&mut self) -> Bound {
        for st in self.lists.values_mut() {
            st.drop_nodes();
        }
        self.dirty = true;
        std::mem::take(&mut self.bound)
    }

    /// The chat items in view, as text, a line to each piece of it.
    pub(crate) fn transcript(&self) -> String {
        let Some(st) = self.lists.get("chat") else {
            return String::new();
        };
        let mut index: Vec<usize> = st.nodes.keys().copied().collect();
        index.sort_unstable();
        let mut lines = Vec::new();
        for i in index {
            if let Some(n) = st.nodes.get(&i) {
                node_text(n, &mut lines);
            }
        }
        lines.join("\n")
    }
}

#[derive(Clone)]
pub(in crate::nodeview) enum Arg {
    Text(String),
    Bool(bool),
    Pos {
        x: f32,
        y: f32,
    },
    /// `on_scroll(dy, { dx, ctrl, alt, shift, super })`.
    Scroll {
        dy: f32,
        dx: f32,
        mods: Modifiers,
    },
}

/// What a frame is painted with: its input, the window's text shaper, and
/// what it leaves for the window.
pub(crate) struct Fx<'a> {
    pub input: &'a FrameInput,
    pub shaper: &'a dyn Shaper,
    pub out: &'a mut FrameOutput,
    /// The keys and text left for the text field with the keyboard.
    pub typed: Vec<Event>,
    /// How opaque what is painted now is.
    pub opacity: f32,
}

impl Fx<'_> {
    /// Paint `shape`, cut to `clip`, at the opacity painted now.
    pub fn paint(&mut self, shape: Shape, clip: Area) {
        self.out.draw.push(shape, clip, self.opacity);
    }
}

/// Whether painting `p` looks the node up by a string (handlers, focus,
/// a tooltip, a role, or something that animates or is drawn into).
pub(in crate::nodeview) fn needs_key(p: &Placed) -> bool {
    let s = &p.node.style;
    let h = &p.node.handlers;
    h.any()
        || s.cursor.is_some()
        || s.hover.is_some()
        || s.group_hover.is_some()
        || s.active.is_some()
        || s.focus.is_some()
        || s.focusable == Some(true)
        || s.tooltip.is_some()
        || s.role.is_some()
        || s.transition.is_some()
        || s.block_input
        || s.autofocus
        || matches!(
            p.node.kind,
            Kind::Canvas(_) | Kind::TextInput(_) | Kind::Input(_)
        )
        || matches!(&p.node.kind, Kind::Text(t) if !t.selectable)
}

impl Colors for Gui {
    fn group(&self, name: &str, bg: bool) -> Option<Rgba> {
        let s = self.lua.theme.hl(name);
        if bg { s.bg } else { s.fg }.and_then(color)
    }

    fn style(&self, name: &str) -> GroupStyle {
        let s = self.lua.theme.hl(name);
        let m = s.add_modifier;
        GroupStyle {
            fg: s.fg.and_then(color),
            bold: m.contains(Modifier::BOLD),
            italic: m.contains(Modifier::ITALIC),
            underline: m.contains(Modifier::UNDERLINED),
            dim: m.contains(Modifier::DIM),
        }
    }
}

/// Text measured with the window's fonts.
pub(in crate::nodeview) struct Fonts<'a> {
    pub(in crate::nodeview) shaper: &'a dyn Shaper,
    /// Whether the proportional font has a bold and an italic face.
    faces: (bool, bool),
    /// The prompt's text, which an input node is as tall as.
    draft: Rc<str>,
    /// The text of the text inputs, which a multiline one is as tall as.
    texts: HashMap<String, Rc<str>>,
    /// The families Lua registered.
    registered: Vec<String>,
}

/// `c` a step toward white (bold without a bold face).
fn brighter(c: Rgba) -> Rgba {
    let up = |v: u8| (f32::from(v) + (255.0 - f32::from(v)) * 0.35).round() as u8;
    Rgba(up(c.0), up(c.1), up(c.2), c.3)
}

impl Fonts<'_> {
    pub(in crate::nodeview) fn family(&self, s: &TextStyle) -> Family {
        let (has_bold, has_italic) = self.faces;
        if let Some(name) = s
            .family
            .as_deref()
            .filter(|n| self.registered.iter().any(|r| r == n))
        {
            Family::Named(name.into())
        } else if s.mono {
            Family::Mono
        } else if s.bold && has_bold {
            Family::SansBold
        } else if s.italic && has_italic {
            Family::SansItalic
        } else if s.medium {
            Family::SansMedium
        } else {
            Family::Sans
        }
    }

    /// How span style `st` is drawn.
    pub(in crate::nodeview) fn run_style(&self, st: &TextStyle) -> RunStyle {
        let family = self.family(st);
        let mut color = st.color;
        if st.bold && !st.mono && !self.faces.0 {
            // Without a bold face, bold is a brighter color.
            color = brighter(color);
        }
        RunStyle {
            italic: st.italic && family != Family::SansItalic,
            strike: st.strike,
            line_height: st.line_height,
            letter_spacing: st.letter_spacing,
            ..RunStyle::plain(family, st.size, color)
        }
    }

    pub(in crate::nodeview) fn job(&self, t: &Text, width: Option<f32>) -> Job {
        let mut job = Job {
            text: String::new(),
            runs: Vec::new(),
            wrap: width.map(|w| w.max(1.0)),
            ellipsis: t.ellipsis && width.is_some(),
            align: t.align,
            box_width: None,
        };
        for s in &t.spans {
            let mut style = self.run_style(&s.style);
            if s.dim {
                style.color.3 = (f32::from(style.color.3) * 0.6).round() as u8;
            }
            style.strike |= s.strike;
            style.underline = s.underline;
            style.background = s.bg;
            job.append(&s.text, style);
        }
        job
    }

    /// The prompt's or a field's text style.
    pub(in crate::nodeview) fn field_style(&self, st: &TextStyle) -> RunStyle {
        RunStyle::plain(self.family(st), st.size, st.color)
    }

    fn rows(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> (usize, f32) {
        let st = self.field_style(style);
        let line = self.shaper.row_height(&st.family, st.size);
        let job = Job::simple(format!("{text} "), st, max_width.map(|w| w.max(1.0)));
        (self.shaper.shape(job).rows.len().max(1), line)
    }
}

impl Metrics for Fonts<'_> {
    fn text(&self, t: &Text, max_width: Option<f32>) -> Size {
        let _span = crate::profile::Span::start("text-measure");
        let g = self.shaper.shape(self.job(t, max_width));
        // A pixel of slack: a box as wide as its text must not wrap it when
        // measured again at that width.
        Size {
            w: g.size.x.ceil() + 1.0,
            h: g.size.y,
        }
    }

    fn text_input(&self, t: &node::TextInput, max_width: Option<f32>) -> Size {
        let (rows, line) = if t.multiline {
            let text = if t.read_only {
                t.value.as_deref().unwrap_or("")
            } else {
                self.texts.get(&t.id).map_or("", |s| s.as_ref())
            };
            let (rows, line) = self.rows(text, &t.text, max_width);
            (rows.clamp(t.min_rows.max(1), t.max_rows.max(t.min_rows).max(1)), line)
        } else {
            let st = self.field_style(&t.text);
            (1, self.shaper.row_height(&st.family, st.size))
        };
        Size {
            w: max_width.unwrap_or(0.0),
            h: rows as f32 * line,
        }
    }

    fn input(&self, i: &Input, max_width: Option<f32>) -> Size {
        let (rows, line) = self.rows(&self.draft, &i.text, max_width);
        let rows = rows.clamp(i.min_rows.max(1), i.max_rows.max(i.min_rows).max(1));
        Size {
            w: max_width.unwrap_or(0.0),
            h: rows as f32 * line,
        }
    }
}

/// Offsets applied: the children of a scroll container move up by its
/// offset, which follows the end when it was there.
fn apply_scroll(
    p: &mut Placed,
    state: &mut HashMap<String, Scroll>,
    used: &mut std::collections::HashSet<String>,
) {
    if let (Overflow::Scroll, Some(content), Some(id)) =
        (p.node.style.overflow, p.content, &p.node.id)
    {
        used.insert(id.clone());
        let s = state.entry(id.clone()).or_default();
        // A row scrolls sideways, a column up and down.
        let sideways = p.node.style.direction == crate::node::Direction::Row;
        let viewport = if sideways { p.inner.w } else { p.inner.h };
        let max = (content - viewport).max(0.0);
        let at_end = s.offset >= s.max - 1.0;
        let target = s.target.take();
        s.held = matches!(target, Some(Target::To(_)));
        s.offset = match target {
            Some(Target::To(y)) => y,
            Some(Target::End) => max,
            None if p.node.style.follow && at_end && !s.dragging => max,
            None => s.offset,
        }
        .clamp(0.0, max);
        (s.max, s.viewport) = (max, viewport);
        let by = -s.offset;
        let (dx, dy) = if sideways { (by, 0.0) } else { (0.0, by) };
        for c in &mut p.children {
            shift(c, dx, dy);
        }
    }
    for c in &mut p.children {
        apply_scroll(c, state, used);
    }
}

/// The text in a node, a line to each text node.
pub(crate) fn node_text(n: &Node, out: &mut Vec<String>) {
    if let Kind::Text(t) = &n.kind {
        let s: String = t.spans.iter().map(|s| s.text.as_str()).collect();
        if !s.trim().is_empty() {
            out.push(s);
        }
    }
    for c in &n.children {
        node_text(c, out);
    }
}

fn shift(p: &mut Placed, dx: f32, dy: f32) {
    p.rect.x += dx;
    p.rect.y += dy;
    p.inner.x += dx;
    p.inner.y += dy;
    for c in &mut p.children {
        shift(c, dx, dy);
    }
}

/// What a frame's painting gathers.
#[derive(Default)]
pub(in crate::nodeview) struct Frame {
    pub(in crate::nodeview) events: Vec<(u64, Arg)>,
    pub(in crate::nodeview) hits: Vec<Hit>,
    pub(in crate::nodeview) hover: std::collections::HashSet<String>,
    /// Whether the pointer is over the nearest node that follows it around
    /// the one being painted (`group_hover`).
    pub(in crate::nodeview) group_hover: bool,
    pub(in crate::nodeview) pressed: std::collections::HashSet<String>,
    /// The tweens used this frame; the others are dropped.
    pub(in crate::nodeview) tweened: std::collections::HashSet<String>,
    /// The text input with the keyboard.
    pub(in crate::nodeview) input_focus: Option<String>,
    /// A press landed in a text input (or the prompt) this frame.
    pub(in crate::nodeview) input_pressed: bool,
    /// Where each node with an id is (windows anchor to `chat` and
    /// `prompt`).
    pub(in crate::nodeview) leaves: Vec<(String, Area)>,
    pub(in crate::nodeview) focusables: Vec<Focusable>,
    pub(in crate::nodeview) input_keys: HashMap<String, u64>,
    /// The `block_input` boxes being drawn, innermost last, and how many
    /// there have been; the last one drawn is the trap.
    pub(in crate::nodeview) scopes: Vec<usize>,
    pub(in crate::nodeview) scope_count: usize,
    pub(in crate::nodeview) trap: Option<usize>,
    /// The innermost focusable node under a press.
    pub(in crate::nodeview) press_focus: Option<String>,
    /// The tooltip of the node under the pointer, and where.
    pub(in crate::nodeview) tooltip: Option<(String, String)>,
    /// The text nodes painted, by key: their text and where it was laid
    /// out, for selecting it with the pointer.
    pub(in crate::nodeview) texts: Vec<(String, crate::backend::Pos, std::sync::Arc<crate::backend::Shaped>)>,
}

impl Gui {
    /// Draw the window as Lua's node tree; false when there is none (no
    /// view or fallback set, or both failed).
    pub(crate) fn view_ui(&mut self, fx: &mut Fx) -> bool {
        let _view = crate::profile::Span::start("view");
        // No view set: the fallback says so (see `bone.gui.fallback`).
        let Some(cb) = self.view.bound.func.or(self.view.bound.fallback) else {
            return false;
        };
        let no_view = self.view.bound.func.is_none();
        let area = fx.input.screen;
        let (w, h) = (area.width(), area.height());
        let vs = &mut self.view;
        if vs.size != (w, h) {
            vs.size = (w, h);
            vs.dirty = true;
        }
        let vs = &mut self.view;
        if vs.rev != self.lua.opts_rev + self.lua.views_rev {
            vs.rev = self.lua.opts_rev + self.lua.views_rev;
            vs.dirty = true;
        }
        let now = std::time::Instant::now();
        if vs.wake.is_some_and(|at| at <= now) {
            vs.wake = None;
            vs.dirty = true;
        }
        if let Some(at) = vs.wake {
            fx.out.repaint_after(at.saturating_duration_since(now));
        }
        // `bone.ui.refresh_in` and `bone.chat.refresh_in`: a window or an
        // item that changes with the clock asks to be made again. One
        // coming due leaves the other deadline alone.
        for at in [&mut self.lua.ui_expiry, &mut self.lua.chat_expiry] {
            let Some(t) = *at else { continue };
            if t <= now {
                *at = None;
                self.view.dirty = true;
            } else {
                fx.out.repaint_after(t - now);
            }
        }
        // Typing, clicks and the like may change what Lua draws.
        if fx.input.events.iter().any(|e| {
            !matches!(
                e,
                Event::PointerMoved(_) | Event::Wheel { .. } | Event::PointerGone
            )
        }) {
            self.view.dirty = true;
        }
        self.wheel(fx);

        let base = TextStyle {
            color: self
                .group("Normal", false)
                .unwrap_or(TextStyle::default().color),
            ..TextStyle::default()
        };
        let rebuilt = self.view.dirty || self.view.bound.tree.is_none();
        // A new tree gets a new table of functions; the old one goes with it.
        let sink = match (&self.view.bound.sink, self.lua.state.as_ref()) {
            (Some(s), _) if !rebuilt => s.clone(),
            (old, Some(lua)) => match lua.create_table() {
                Ok(t) => Sink::new(t, old.as_ref().map_or(0, |s| s.next.get())).with_viewport(w, h),
                Err(_) => return false,
            },
            _ => return false,
        };
        if rebuilt {
            self.view.bound.prev_sink = (self.view.bound.sink.take()).map(|s| s.table);
            self.view.bound.canvases.clear();
            // What Lua highlights in the prompt may have changed too.
            self.prompt.marks_for = None;
        }
        self.view.bound.sink = Some(sink.clone());
        let _build = crate::profile::Span::start(if rebuilt { "build" } else { "build-skip" });
        let mut tree = match self.view.bound.tree.take() {
            Some(t) if !rebuilt => t,
            _ => {
                self.view.dirty = false;
                let first = if no_view {
                    Err("no view is set: bone.gui.view(fn)".to_owned())
                } else {
                    self.build_tree(cb, w, h, None, &base, &sink)
                };
                match first {
                    Ok(tree) => {
                        self.view.bound.error = None;
                        tree
                    }
                    Err(e) => {
                        // Said once, not every frame.
                        if !no_view && self.view.bound.error.as_deref() != Some(&e) {
                            self.error(format!("gui view: {e}"));
                            self.view.bound.error = Some(e.clone());
                        }
                        // The fallback, a view of Lua's that says so.
                        let fallback = match self.view.bound.fallback {
                            Some(fb) => self.build_tree(fb, w, h, Some(&e), &base, &sink),
                            None => return false,
                        };
                        match fallback {
                            Ok(tree) => tree,
                            Err(_) => return false,
                        }
                    }
                }
            }
        };
        drop(_build);

        let mut lists = std::mem::take(&mut self.view.lists);
        let mut scroll = std::mem::take(&mut self.view.scroll);
        let mut used_scroll = std::collections::HashSet::new();
        set_totals(&mut tree, &mut lists);
        if self.draft_rc.as_ref() != self.draft.as_str() {
            self.draft_rc = Rc::from(self.draft.as_str());
        }
        let fonts = Fonts {
            shaper: fx.shaper,
            faces: self.view.faces,
            draft: Rc::clone(&self.draft_rc),
            registered: (self.view.bound.extra_fonts.iter())
                .map(|(n, _)| n.clone())
                .collect(),
            texts: (self.view.inputs.iter())
                .map(|(k, b)| (k.clone(), Rc::clone(&b.measure)))
                .collect(),
        };
        let mut frame = Frame::default();
        let mut windows = Windows::new();
        {
            let _place = crate::profile::Span::start("place");
            let mut placed = flex::place(
                &tree,
                Rect {
                    x: area.min.x,
                    y: area.min.y,
                    w,
                    h,
                },
                &fonts,
            );
            apply_scroll(&mut placed, &mut scroll, &mut used_scroll);
            drop(_place);
            let _realize = crate::profile::Span::start("realize");
            let again = self.realize(
                &placed,
                &fonts,
                &base,
                &sink,
                rebuilt,
                &mut lists,
                &mut scroll,
                &mut windows,
            );
            if again {
                fx.out.repaint();
            }
            drop(_realize);
            let _item_place = crate::profile::Span::start("item-place");
            let placed_lists: HashMap<String, (usize, Vec<Placed>)> = windows
                .iter()
                .map(|(id, win)| {
                    let items = win
                        .nodes
                        .iter()
                        .zip(&win.rects)
                        .map(|(n, r)| {
                            // What scrolls inside an item (a table, a code
                            // block) scrolls as it does in the window.
                            let mut item = flex::place(n, *r, &fonts);
                            apply_scroll(&mut item, &mut scroll, &mut used_scroll);
                            item
                        })
                        .collect();
                    (id.clone(), (win.first, items))
                })
                .collect();
            drop(_item_place);
            // A chat keeps its position when it is switched away from,
            // but a markdown block's sideways offset is one of thousands:
            // those live only while their item is in the virtualized
            // window, as tweens do.
            scroll.retain(|k, _| used_scroll.contains(k) || !k.starts_with("md:"));
            self.view.scroll = scroll;
            let _paint = crate::profile::Span::start("paint");
            self.paint(
                fx,
                &fonts,
                &placed,
                area,
                0,
                None,
                &placed_lists,
                &mut frame,
                None,
            );
            drop(_paint);
        }
        self.select_text(fx, &frame.texts);
        self.tooltip(fx, frame.tooltip.take());
        self.window_units(fx, area, std::mem::take(&mut frame.leaves));
        // The items in view are kept for the next frame, the rest dropped.
        for (id, win) in windows {
            if let Some(st) = lists.get_mut(&id) {
                st.nodes = (win.first..).zip(win.nodes).collect();
            }
        }
        self.view.lists = lists;
        self.view.bound.tree = Some(tree);
        self.view.hover = frame.hover;
        self.view.pressed = frame.pressed;
        let used = std::mem::take(&mut frame.tweened);
        self.view.tweens.retain(|k, _| used.contains(k));
        self.view.hits = frame.hits;
        self.view.input_keys = std::mem::take(&mut frame.input_keys);
        // A text input that is not drawn any more has not the keyboard, nor
        // has one when a press lands anywhere else (clicking away blurs it,
        // as on the web): the keyboard goes where the press put it, else
        // back to the prompt.
        if let Some(id) = &self.view.input_focus
            && (frame.input_focus.as_ref() != Some(id)
                || (self.view.pointer.primary_pressed() && !frame.input_pressed))
        {
            self.view.input_focus = None;
            // Lua's view may follow the focus (`bone.gui.focused`).
            self.view.invalidate();
        }
        self.settle_focus(fx, frame.focusables, frame.trap, frame.press_focus);
        if !frame.events.is_empty() {
            self.view.dirty = true;
        }
        self.call_handlers(frame.events);
        true
    }

    /// Call view function `cb` for a `w` × `h` window and parse what it
    /// returns; `error` is what the view before it failed with (a fallback
    /// view shows it).
    fn build_tree(
        &mut self,
        cb: u64,
        w: f32,
        h: f32,
        error: Option<&str>,
        base: &TextStyle,
        sink: &Sink,
    ) -> Result<Node, String> {
        let (cw, ch) = (self.view.cell)
            .or(self.view.mono_cell)
            .unwrap_or((8.0, 16.0));
        let root = Host::call_callback(self, cb, "gui view", |lua| {
            let ctx = lua.create_table()?;
            ctx.set("width", w)?;
            ctx.set("height", h)?;
            let cell = lua.create_table()?;
            cell.set("w", cw)?;
            cell.set("h", ch)?;
            ctx.set("cell", cell)?;
            if let Some(e) = error {
                ctx.set("error", e)?;
            }
            Ok(Value::Table(ctx))
        });
        match root {
            Some(Value::Table(t)) => node::parse(&t, &*self, base, sink),
            Some(other) => Err(format!(
                "the view returned a {}, not a node table",
                other.type_name()
            )),
            // The error was reported as it was raised: say what it was.
            None => Err(self
                .lua
                .lua_errors
                .last()
                .cloned()
                .unwrap_or_else(|| "the view raised an error".into())),
        }
    }

    /// There is no view (none set, or it failed): say so, in plain text.
    pub(crate) fn no_view_ui(&mut self, fx: &mut Fx) {
        let why = match &self.view.bound.error {
            Some(e) => format!("The view failed: {e}"),
            None => "No view: set one with bone.gui.view(fn).".to_owned(),
        };
        let color = self.group("Normal", false).unwrap_or(TextStyle::default().color);
        let job = Job::simple(why, RunStyle::plain(Family::Sans, 14.0, color), None);
        let text = fx.shaper.shape(job);
        let screen = fx.input.screen;
        fx.paint(
            Shape::Text {
                at: pos(screen.min.x + 24.0, screen.min.y + 24.0),
                text,
                color: None,
            },
            screen,
        );
    }

    /// The scroll actions (page up, scroll top...) move the node view's
    /// scroll container with the id `chat`.
    pub(crate) fn scroll_view_chat(&mut self, b: bone_ui::keymap::Builtin) {
        use bone_ui::keymap::Builtin::*;
        // One line of the chat, for the scroll keys (`bone.o.scroll_line`).
        let line_px = self.number_option("scroll_line", 20.0).max(1.0);
        let Some(s) = self.view.scroll.get_mut("chat") else {
            return;
        };
        let line = line_px;
        s.offset = match b {
            ScrollUp => s.offset - 3.0 * line,
            ScrollDown => s.offset + 3.0 * line,
            PageUp => s.offset - s.viewport * 0.9,
            PageDown => s.offset + s.viewport * 0.9,
            ScrollTop => 0.0,
            _ => s.max,
        }
        .clamp(0.0, s.max);
        s.target = None;
        self.view.invalidate();
    }
}
