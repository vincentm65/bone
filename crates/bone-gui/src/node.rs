//! The GUI's own view: a tree of nodes that Lua returns (`bone.gui.view`),
//! with CSS-like styles. This module parses the Lua table into a typed
//! tree and resolves colors; `flex.rs` lays it out and `nodeview` draws it.
//! See `docs/plans/gui-native-view.md` for the fields.

use mlua::{Function, Table, Value};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Dim {
    #[default]
    Auto,
    Px(f32),
    /// A fraction (0.5 for `"50%"`) of the parent's inner size.
    Pct(f32),
    /// `calc(50% - 20px)`: a fraction of the parent's inner size plus pixels.
    Mix {
        pct: f32,
        px: f32,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }

    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
}

/// Corner radii, CSS order: top-left, top-right, bottom-right, bottom-left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Corners {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum BorderStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

/// A line outside the border box, drawing over what is around: `outline =
/// { width, color, offset }`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outline {
    pub width: f32,
    pub color: Rgba,
    pub offset: f32,
}

/// A linear gradient: `gradient = { angle = 180, stops = { "#fff", "#000" } }`
/// (the angle is CSS's: 180 runs to the bottom; a stop is a color or
/// `{ position 0–1, color }`).
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub angle: f32,
    pub stops: Vec<(f32, Rgba)>,
}

/// What a node changes while the pointer is over it (`hover`), pressing it
/// (`active`) or it has the focus (`focus`): the paint, and the color of
/// the text in it.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub bg: Option<Rgba>,
    pub gradient: Option<Gradient>,
    pub border_color: Option<Rgba>,
    pub color: Option<Rgba>,
    pub opacity: Option<f32>,
    pub shadows: Option<Vec<Shadow>>,
}

/// `transition = 150` (ms) or `{ duration = 150, easing = "ease-out" }`: what
/// the node's background, border color and opacity change to (by Lua, or
/// by `hover` and the other states) is faded to over that long.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    pub duration_ms: f32,
    pub easing: Easing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Easing {
    Linear,
    EaseIn,
    #[default]
    EaseOut,
    EaseInOut,
}

impl Easing {
    /// The progress `t` (0 to 1) eased.
    pub fn at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t * t,
            Easing::EaseOut => 1.0 - (1.0 - t).powi(3),
            Easing::EaseInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
        }
    }
}

/// What a node is to a screen reader (`role`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Button,
    Link,
    Checkbox,
    Radio,
    /// One of a set to choose from (a list entry, a tab): with `selected`.
    Option,
    Image,
    Dialog,
    Text,
    /// Nothing: neither it nor the text in it is announced.
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ScrollbarShow {
    /// While the content is longer than the box.
    #[default]
    Auto,
    /// Only while the pointer is over the box.
    Hover,
    Hidden,
}

/// `scrollbar = { width, color, radius, inset, show }` on a scroll box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scrollbar {
    pub width: f32,
    pub color: Rgba,
    pub radius: f32,
    pub inset: f32,
    pub show: ScrollbarShow,
}

impl Default for Scrollbar {
    fn default() -> Self {
        Scrollbar {
            width: 5.0,
            color: Rgba(255, 255, 255, 40),
            radius: 3.0,
            inset: 2.0,
            show: ScrollbarShow::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Direction {
    Row,
    #[default]
    Column,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Align {
    #[default]
    Stretch,
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    Between,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Overflow {
    #[default]
    Visible,
    Hidden,
    /// Clipped, scrolls vertically (a column), with the offset kept by `id`.
    Scroll,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Default)]
pub struct Style {
    pub direction: Direction,
    pub gap: f32,
    pub align: Align,
    pub justify: Justify,
    pub width: Dim,
    pub height: Dim,
    pub min_width: Dim,
    pub max_width: Dim,
    pub min_height: Dim,
    pub max_height: Dim,
    pub grow: f32,
    /// `None`: shrinks when its size is automatic, not when it is set.
    pub shrink: Option<f32>,
    pub padding: Edges,
    pub margin: Edges,
    pub bg: Option<Rgba>,
    pub gradient: Option<Gradient>,
    pub border: Edges,
    pub border_style: BorderStyle,
    pub border_color: Option<Rgba>,
    pub outline: Option<Outline>,
    pub radius: Corners,
    /// 0 to 1 for the node and what is in it; none is 1.
    pub opacity: Option<f32>,
    /// Restyles while the pointer is over, pressing, or the focus is on
    /// the node. Applied by Rust each frame, with no rebuild in Lua.
    pub hover: Option<Box<State>>,
    pub active: Option<Box<State>>,
    pub focus: Option<Box<State>>,
    pub tooltip: Option<String>,
    pub transition: Option<Transition>,
    pub scrollbar: Scrollbar,
    pub overflow: Overflow,
    /// A scroll container stays at its end while content is added there.
    pub follow: bool,
    /// The pointer over the node: `pointer`, `text`, `grab`, `not-allowed`.
    pub cursor: Option<String>,
    /// Out of the flow: placed by `top`/`right`/`bottom`/`left` in its
    /// parent, over its siblings (`z` orders them).
    pub absolute: bool,
    pub top: Dim,
    pub right: Dim,
    pub bottom: Dim,
    pub left: Dim,
    pub z: i32,
    /// A row whose children go on to another line when they run out of room.
    pub flex_wrap: bool,
    /// The space between children along (`column_gap` for a row) and across
    /// (the other) the lines; `gap` is both.
    pub row_gap: Option<f32>,
    pub column_gap: Option<f32>,
    /// This child's own cross-axis alignment, over its parent's `align`.
    pub align_self: Option<Align>,
    /// Where this child goes among its siblings (default 0, then as made).
    pub order: i32,
    /// Margins set to `"auto"` (top, right, bottom, left) take the free
    /// space: `margin = { 0, "auto" }` pushes a child to the end.
    pub margin_auto: [bool; 4],
    /// Width over height, for the side that is not set.
    pub aspect_ratio: Option<f32>,
    /// Takes the pointer from everything under it (a modal's backdrop),
    /// and keeps the keyboard focus among the nodes in it.
    pub block_input: bool,
    /// Whether the keyboard reaches the node (tab, `bone.gui.focus`).
    /// Unset: nodes with `on_key`, `on_click` or a `focus` look do.
    pub focusable: Option<bool>,
    /// Takes the keyboard focus each time it appears.
    pub autofocus: bool,
    /// For screen readers: what the node is, its name (default: its text)
    /// and, for a checkbox or an option, whether it is on. A node with
    /// `on_click` is a button unless it says otherwise.
    pub role: Option<Role>,
    pub label: Option<String>,
    pub selected: Option<bool>,
    /// Milliseconds after taking the focus that keys are ignored: a key
    /// meant for what was there before (enter, typing) does not answer a
    /// question that has just come up.
    pub key_guard: f32,
    /// Soft shadows under the box: `shadow = { blur, spread, dx, dy, color }`
    /// or a list of those.
    pub shadows: Vec<Shadow>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub blur: f32,
    pub spread: f32,
    pub dx: f32,
    pub dy: f32,
    pub color: Rgba,
}

impl Style {
    pub fn clips(&self) -> bool {
        self.overflow != Overflow::Visible
    }

    /// The space between children along the main axis.
    pub fn main_gap(&self) -> f32 {
        match self.direction {
            Direction::Row => self.column_gap.unwrap_or(self.gap),
            Direction::Column => self.row_gap.unwrap_or(self.gap),
        }
    }

    /// The space between lines of a wrapping row.
    pub fn cross_gap(&self) -> f32 {
        match self.direction {
            Direction::Row => self.row_gap.unwrap_or(self.gap),
            Direction::Column => self.column_gap.unwrap_or(self.gap),
        }
    }
}

/// Lua functions a node carries.
#[derive(Clone, Debug, Default)]
pub struct Handlers {
    pub click: Option<u64>,
    pub secondary: Option<u64>,
    /// `on_middle{ x, y }`: a click with the middle button.
    pub middle: Option<u64>,
    pub double_click: Option<u64>,
    pub press: Option<u64>,
    pub release: Option<u64>,
    /// `on_drag{ x, y }`: the pointer, in the window, while the button is
    /// held on the node and it moves.
    pub drag: Option<u64>,
    pub hover: Option<u64>,
    pub scroll: Option<u64>,
    /// `on_key(name)`: a key while the node has the focus; true takes it.
    pub key: Option<u64>,
}

/// Where a view's functions live: a Lua table by number, so a tree of
/// thousands of handlers holds no Lua references (there are only so many).
/// It is made again when the tree is.
#[derive(Clone, Debug)]
pub struct Sink {
    pub table: Table,
    pub next: std::rc::Rc<std::cell::Cell<u64>>,
    /// The window's size, for `vw` and `vh`.
    pub viewport: (f32, f32),
}

impl Sink {
    pub fn new(table: Table, next: u64) -> Sink {
        Sink {
            table,
            next: std::rc::Rc::new(std::cell::Cell::new(next)),
            viewport: (0.0, 0.0),
        }
    }

    pub fn with_viewport(mut self, w: f32, h: f32) -> Sink {
        self.viewport = (w, h);
        self
    }

    fn add(&self, f: Function) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        let _ = self.table.raw_set(id, f);
        id
    }
}

impl Handlers {
    pub fn any(&self) -> bool {
        self.clicks() || self.hover.is_some() || self.key.is_some()
    }

    /// Whether the node needs to be clicked, not just hovered.
    pub fn clicks(&self) -> bool {
        let clicks = [
            self.click,
            self.secondary,
            self.middle,
            self.double_click,
            self.press,
            self.release,
            self.drag,
        ];
        clicks.iter().any(Option::is_some)
    }
}

/// What text inherits from the boxes around it.
#[derive(Clone, Debug)]
pub struct TextStyle {
    pub color: Rgba,
    pub size: f32,
    pub mono: bool,
    /// A family Lua registered (`bone.gui.font`), over `mono`.
    pub family: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    /// Pixels from one line to the next (none: the font's own).
    pub line_height: Option<f32>,
    pub letter_spacing: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle {
            color: Rgba(0xee, 0xee, 0xee, 0xff),
            size: 15.0,
            mono: false,
            family: None,
            bold: false,
            italic: false,
            strike: false,
            line_height: None,
            letter_spacing: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub style: TextStyle,
    /// A chip behind the glyphs (a code span).
    pub bg: Option<Rgba>,
    pub underline: bool,
    pub strike: bool,
    pub dim: bool,
}

impl Span {
    fn plain(text: String, style: TextStyle) -> Span {
        Span {
            text,
            style,
            bg: None,
            underline: false,
            strike: false,
            dim: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Text {
    pub spans: Vec<Span>,
    pub align: TextAlign,
    pub wrap: bool,
    /// Drag to select, ctrl+c to copy. Takes the pointer, so a click
    /// handler above it does not see clicks on the text.
    pub selectable: bool,
    /// Text that does not fit its line ends in "…" instead of wrapping or
    /// being cut (`text_overflow = "ellipsis"`).
    pub ellipsis: bool,
    /// Drawn first, offset: `text_shadow = { dx, dy, color }`.
    pub shadow: Option<TextShadow>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextShadow {
    pub dx: f32,
    pub dy: f32,
    pub color: Rgba,
}

/// The prompt's text field, edited natively.
#[derive(Clone, Debug)]
pub struct Input {
    pub placeholder: String,
    pub text: TextStyle,
    pub min_rows: usize,
    pub max_rows: usize,
}

/// A box Lua draws on: `canvas = { draw = function(ctx) return ops end, fps
/// = 0 }`. `ctx` is `{ width, height, time }` (the box's size, and seconds
/// since the window opened); the ops are drawn in the box, with coordinates
/// from its top left. With `fps` above 0 it is drawn that many times a
/// second; else once per build and size. See `bone.gui.draw`.
#[derive(Clone, Debug)]
pub struct Canvas {
    pub draw: u64,
    pub fps: f32,
}

/// A text field of its own, edited natively (cursor, selection, IME, paste):
/// `text_input = { value, placeholder, multiline, rows, password, focus,
/// on_change = fn(text), on_submit = fn(text) }` on a node with an `id`.
/// Its text lives in the window: `value` sets it when it differs from what
/// Lua last gave; `bone.gui.input_text(id)` reads it.
#[derive(Clone, Debug)]
pub struct TextInput {
    pub id: String,
    pub value: Option<String>,
    pub placeholder: String,
    pub multiline: bool,
    pub min_rows: usize,
    pub max_rows: usize,
    pub password: bool,
    pub read_only: bool,
    /// Takes the keyboard when it first appears.
    pub focus: bool,
    /// Its text is selected when it takes the keyboard (typing replaces
    /// it).
    pub select: bool,
    pub on_change: Option<u64>,
    pub on_submit: Option<u64>,
    pub text: TextStyle,
}

/// A long scrolling column whose items Lua makes only when they are in
/// view: `list = { count, item = function(i) -> node, estimate, gap, key }`.
#[derive(Clone, Debug)]
pub struct List {
    pub count: usize,
    /// Height assumed for an item not yet seen.
    pub estimate: f32,
    pub gap: f32,
    /// When it changes, what was measured is forgotten (another chat).
    pub key: String,
    pub item: Function,
    /// The items' height together, from what has been measured.
    pub total: f32,
    /// As tall as the items made so far (`fit = true`), not the room given:
    /// a window that holds only a list is as big as what is in it.
    pub fit: bool,
    /// Item `i` is the chat's item `i` (`bone.nodes.chat`): its node is
    /// kept, through rebuilds too, until the chat says it changed.
    pub chat: bool,
    /// `chat = n`: chat buffer `n` rather than the chat on screen.
    pub buffer: Option<usize>,
}

/// Where an image's pixels are: an attachment the core holds (an item's
/// `images[i].id`), or a file on this machine.
#[derive(Clone, Debug, PartialEq)]
pub enum ImageSource {
    Attachment(String),
    Path(String),
}

impl ImageSource {
    /// What the texture cache calls it.
    pub fn key(&self) -> String {
        match self {
            ImageSource::Attachment(id) => id.clone(),
            ImageSource::Path(p) => format!("file:{p}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Fit {
    /// Whole, as large as the box allows, centered.
    #[default]
    Contain,
    /// The box filled, the overhang cut off.
    Cover,
    /// Stretched to the box.
    Fill,
    /// Its own size, centered.
    None,
}

/// `image = { id = … | path = …, width, height, fit }`: the picture. Its
/// `width` and `height` are its own size in pixels, which gives the box its
/// shape before the pixels are read.
#[derive(Clone, Debug)]
pub struct Image {
    pub source: ImageSource,
    pub fit: Fit,
    pub natural: Option<(f32, f32)>,
}

#[derive(Clone, Debug)]
pub enum Kind {
    Box,
    Text(Text),
    Input(Input),
    List(List),
    Image(Image),
    TextInput(TextInput),
    Canvas(Canvas),
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: Option<String>,
    pub style: Style,
    pub handlers: Handlers,
    pub kind: Kind,
    pub children: Vec<Node>,
}

impl Node {
    /// A box with nothing in it.
    pub fn empty() -> Node {
        Node {
            id: None,
            style: Style::default(),
            handlers: Handlers::default(),
            kind: Kind::Box,
            children: Vec::new(),
        }
    }
}

/// Turns a color string into a color: `#rgb`, `#rrggbb`, `#rrggbbaa`, or a
/// highlight group (its background when the flag is set, else its
/// foreground).
pub trait Colors {
    fn group(&self, name: &str, bg: bool) -> Option<Rgba>;
    /// A highlight group whole: colors and the text attributes.
    fn style(&self, name: &str) -> GroupStyle;
}

#[derive(Clone, Debug, Default)]
pub struct GroupStyle {
    pub fg: Option<Rgba>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
}

pub fn parse_hex(s: &str) -> Option<Rgba> {
    let h = s.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    let nib = |i: usize| {
        u8::from_str_radix(h.get(i..i + 1)?, 16)
            .ok()
            .map(|n| n * 17)
    };
    match h.len() {
        3 => Some(Rgba(nib(0)?, nib(1)?, nib(2)?, 0xff)),
        4 => Some(Rgba(nib(0)?, nib(1)?, nib(2)?, nib(3)?)),
        6 => Some(Rgba(byte(0)?, byte(2)?, byte(4)?, 0xff)),
        8 => Some(Rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

pub fn color(c: &dyn Colors, s: &str, bg: bool) -> Option<Rgba> {
    if s == "transparent" {
        return Some(Rgba(0, 0, 0, 0));
    }
    parse_hex(s).or_else(|| c.group(s, bg))
}

fn le(e: mlua::Error) -> String {
    e.to_string()
}

/// A number, or a string that is one, as Lua reads it.
fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(n) => Some(*n as f64),
        Value::Number(n) => Some(*n),
        Value::String(s) => s.to_str().ok()?.trim().parse().ok(),
        _ => None,
    }
}

/// A field's type, read from a Lua value.
trait Prop: Sized {
    const WHAT: &'static str;
    fn from_value(v: &Value) -> Option<Self>;
}

macro_rules! props {
    ($($t:ty, $what:literal, |$v:ident| $e:expr;)*) => {$(
        impl Prop for $t {
            const WHAT: &'static str = $what;
            fn from_value($v: &Value) -> Option<Self> {
                $e
            }
        }
    )*};
}

props! {
    String, "a string", |v| match v {
        Value::String(s) => s.to_str().ok().map(|s| s.to_owned()),
        Value::Integer(n) => Some(n.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    f32, "a number", |v| num(v).map(|n| n as f32);
    i32, "a number", |v| num(v).map(|n| n as i32);
    usize, "a number", |v| num(v).map(|n| n as usize);
    // Truthy, as in Lua: `link = "https://…"` is true.
    bool, "true or false", |v| Some(!matches!(v, Value::Boolean(false) | Value::Nil));
    Table, "a table", |v| match v {
        Value::Table(t) => Some(t.clone()),
        _ => None,
    };
    Function, "a function", |v| match v {
        Value::Function(f) => Some(f.clone()),
        _ => None,
    };
}

/// An enum a field names: `direction = "row"`.
trait Named: Copy + 'static {
    const NAMES: &'static [(&'static str, Self)];
}

macro_rules! names {
    ($($t:ident { $($n:literal => $v:ident),* })*) => {$(
        impl Named for $t {
            const NAMES: &'static [(&'static str, Self)] = &[$(($n, $t::$v)),*];
        }
    )*};
}

names! {
    Direction { "row" => Row, "column" => Column }
    Align { "stretch" => Stretch, "start" => Start, "center" => Center, "end" => End }
    Justify { "start" => Start, "center" => Center, "end" => End, "between" => Between }
    BorderStyle { "solid" => Solid, "dashed" => Dashed, "dotted" => Dotted }
    Overflow { "visible" => Visible, "hidden" => Hidden, "scroll" => Scroll }
    Role {
        "button" => Button, "link" => Link, "checkbox" => Checkbox, "radio" => Radio,
        "option" => Option, "image" => Image, "dialog" => Dialog, "text" => Text, "none" => None
    }
    Fit { "contain" => Contain, "cover" => Cover, "fill" => Fill, "none" => None }
    TextAlign { "left" => Left, "center" => Center, "right" => Right }
    Easing {
        "linear" => Linear, "ease-in" => EaseIn, "ease-out" => EaseOut, "ease-in-out" => EaseInOut
    }
    ScrollbarShow { "auto" => Auto, "hover" => Hover, "hidden" => Hidden }
}

/// A node's table read in one pass: its string keys and their values, and
/// its array part. A node has dozens of optional fields and sets a few;
/// asking Lua for each costs a call apiece, a walk over the few costs one.
struct Props {
    named: Vec<(mlua::String, Value)>,
    /// The integer keys in order: the children (a nil among them leaves a
    /// gap, not an end).
    items: Vec<(i64, Value)>,
}

impl Props {
    fn new(t: &Table) -> Result<Props, String> {
        let (mut named, mut items) = (Vec::new(), Vec::new());
        for pair in t.pairs::<Value, Value>() {
            match pair.map_err(le)? {
                (Value::String(k), v) => named.push((k, v)),
                (Value::Integer(i @ 1..), v) => items.push((i, v)),
                _ => {}
            }
        }
        items.sort_unstable_by_key(|(i, _)| *i);
        Ok(Props { named, items })
    }

    fn raw(&self, k: &str) -> Option<&Value> {
        let k = k.as_bytes();
        (self.named.iter())
            .find(|(n, _)| &*n.as_bytes() == k)
            .map(|(_, v)| v)
    }

    fn get<T: Prop>(&self, k: &str) -> Result<Option<T>, String> {
        let Some(v) = self.raw(k) else {
            return Ok(None);
        };
        T::from_value(v)
            .map(Some)
            .ok_or_else(|| format!("{k}: expected {}", T::WHAT))
    }

    /// The `i`th (from 1) of the array part, as a `T`.
    fn item<T: Prop>(&self, i: i64) -> Option<T> {
        let (_, v) = self.items.iter().find(|(n, _)| *n == i)?;
        T::from_value(v)
    }

    /// A nested table's own properties.
    fn sub(&self, k: &str) -> Result<Option<Props>, String> {
        self.get::<Table>(k)?.map(|t| Props::new(&t)).transpose()
    }

    fn color(&self, k: &str, colors: &dyn Colors, bg: bool) -> Result<Option<Rgba>, String> {
        match self.get::<String>(k)? {
            Some(c) => color(colors, &c, bg)
                .map(Some)
                .ok_or_else(|| format!("{k}: unknown color {c:?}")),
            None => Ok(None),
        }
    }

    fn pick<T: Copy>(&self, k: &str, options: &[(&str, T)]) -> Result<Option<T>, String> {
        let Some(s) = self.get::<String>(k)? else {
            return Ok(None);
        };
        (options.iter().find(|(n, _)| *n == s))
            .map(|(_, v)| Some(*v))
            .ok_or_else(|| {
                let names: Vec<&str> = options.iter().map(|(n, _)| *n).collect();
                format!("{k}: {s:?} is not one of {}", names.join(", "))
            })
    }

    fn choice<T: Named>(&self, k: &str) -> Result<Option<T>, String> {
        self.pick(k, T::NAMES)
    }

    /// A function field, kept in the sink as a handler.
    fn on(&self, k: &str, sink: &Sink) -> Result<Option<u64>, String> {
        Ok(self.get::<Function>(k)?.map(|f| sink.add(f)))
    }
}

/// What a unit is: `em` is the node's font size, `vw` and `vh` the
/// window's.
struct Units {
    em: f32,
    vw: f32,
    vh: f32,
}

/// One term of a size: `12`, `"50%"`, `"12px"`, `"1.5em"`, `"50vw"`, as
/// (fraction of the parent, pixels).
fn term(s: &str, u: &Units) -> Result<(f32, f32), String> {
    let s = s.trim();
    let number = |n: &str| {
        n.trim()
            .parse::<f32>()
            .map_err(|_| format!("{s:?} is not a size"))
    };
    if let Some(n) = s.strip_suffix('%') {
        return Ok((number(n)? / 100.0, 0.0));
    }
    for (suffix, per) in [
        ("px", 1.0),
        ("em", u.em),
        ("vw", u.vw / 100.0),
        ("vh", u.vh / 100.0),
    ] {
        if let Some(n) = s.strip_suffix(suffix) {
            return Ok((0.0, number(n)? * per));
        }
    }
    Ok((0.0, number(s)?))
}

/// A size written as a string: a term, `calc(...)` of terms joined by `+`
/// and `-`, or `auto`.
fn size(s: &str, u: &Units) -> Result<Dim, String> {
    let s = s.trim();
    if s == "auto" {
        return Ok(Dim::Auto);
    }
    let (mut pct, mut px) = (0.0, 0.0);
    if let Some(expr) = s.strip_prefix("calc(").and_then(|e| e.strip_suffix(')')) {
        let mut sign = 1.0;
        for piece in expr.split_inclusive(['+', '-']) {
            let (body, next) = match piece.chars().last() {
                Some('+') => (&piece[..piece.len() - 1], 1.0),
                Some('-') => (&piece[..piece.len() - 1], -1.0),
                _ => (piece, 1.0),
            };
            if !body.trim().is_empty() {
                let (p, x) = term(body, u)?;
                pct += sign * p;
                px += sign * x;
            }
            sign = next;
        }
    } else {
        (pct, px) = term(s, u)?;
    }
    Ok(match (pct != 0.0, px != 0.0) {
        (true, false) => Dim::Pct(pct),
        (false, _) => Dim::Px(px),
        (true, true) => Dim::Mix { pct, px },
    })
}

fn dim(p: &Props, k: &str, u: &Units) -> Result<Dim, String> {
    match p.raw(k) {
        None => Ok(Dim::Auto),
        Some(Value::String(s)) => {
            size(&s.to_str().map_err(le)?, u).map_err(|e| format!("{k}: {e}"))
        }
        Some(v) => num(v)
            .map(|n| Dim::Px(n as f32))
            .ok_or_else(|| format!("{k}: expected a number or a size like \"50%\"")),
    }
}

/// `n`, `{ a, b }` or `{ a, b, c, d }` as four values: the pair fills the
/// four as `two` says (indexes into it). An entry is `None` for `"auto"`,
/// where `auto` allows it.
fn sides(
    p: &Props,
    k: &str,
    two: [usize; 4],
    auto: bool,
) -> Result<Option<[Option<f32>; 4]>, String> {
    let one = |v: &Value| -> Result<Option<f32>, String> {
        match v {
            Value::String(s) if auto && &*s.to_str().map_err(le)? == "auto" => Ok(None),
            v => num(v).map(|n| Some(n as f32)).ok_or_else(|| {
                let what = if auto {
                    "numbers or \"auto\""
                } else {
                    "numbers"
                };
                format!("{k}: expected {what}")
            }),
        }
    };
    let list: Vec<Option<f32>> = match p.raw(k) {
        None => return Ok(None),
        Some(Value::Table(a)) => a
            .sequence_values::<Value>()
            .map(|v| one(&v.map_err(le)?))
            .collect::<Result<_, _>>()?,
        Some(v) => vec![one(v)?],
    };
    Ok(Some(match list[..] {
        [all] => [all; 4],
        [a, b] => two.map(|i| [a, b][i]),
        [a, b, c, d] => [a, b, c, d],
        _ => return Err(format!("{k}: expected 1, 2 or 4 values")),
    }))
}

/// A margin, and which of its sides are `"auto"`.
fn margin(p: &Props) -> Result<(Edges, [bool; 4]), String> {
    Ok(match sides(p, "margin", [0, 1, 0, 1], true)? {
        Some(s) => (
            Edges::of(s.map(|x| x.unwrap_or(0.0))),
            s.map(|x| x.is_none()),
        ),
        None => (Edges::default(), [false; 4]),
    })
}

/// `n`, `{ v, h }` or `{ top, right, bottom, left }`.
fn edges(p: &Props, k: &str) -> Result<Edges, String> {
    Ok((sides(p, k, [0, 1, 0, 1], false)?)
        .map_or(Edges::default(), |s| Edges::of(s.map(|x| x.unwrap_or(0.0)))))
}

impl Edges {
    fn of([top, right, bottom, left]: [f32; 4]) -> Self {
        Edges {
            top,
            right,
            bottom,
            left,
        }
    }
}

/// `n`, `{ top, bottom }` or `{ tl, tr, br, bl }`.
fn corners(p: &Props, k: &str) -> Result<Corners, String> {
    Ok(match sides(p, k, [0, 0, 1, 1], false)? {
        Some([tl, tr, br, bl]) => Corners {
            tl: tl.unwrap_or(0.0),
            tr: tr.unwrap_or(0.0),
            br: br.unwrap_or(0.0),
            bl: bl.unwrap_or(0.0),
        },
        None => Corners::default(),
    })
}

fn one_shadow(p: &Props, colors: &dyn Colors) -> Result<Shadow, String> {
    Ok(Shadow {
        blur: p.get("blur")?.unwrap_or(16.0),
        spread: p.get("spread")?.unwrap_or(0.0),
        dx: p.get("dx")?.unwrap_or(0.0),
        dy: p.get("dy")?.unwrap_or(4.0),
        color: p
            .color("color", colors, true)?
            .unwrap_or(Rgba(0, 0, 0, 110)),
    })
}

/// `shadow = { … }` or a list of them.
fn shadows(p: &Props, colors: &dyn Colors) -> Result<Vec<Shadow>, String> {
    let Some(t) = p.get::<Table>("shadow")? else {
        return Ok(Vec::new());
    };
    if t.raw_len() == 0 {
        return Ok(vec![one_shadow(&Props::new(&t)?, colors)?]);
    }
    (t.sequence_values::<Table>())
        .map(|s| one_shadow(&Props::new(&s.map_err(le)?)?, colors))
        .collect()
}

fn gradient(p: &Props, colors: &dyn Colors) -> Result<Option<Gradient>, String> {
    let Some(g) = p.get::<Table>("gradient")? else {
        return Ok(None);
    };
    // `{ "#a", "#b" }` is the stops alone.
    let (angle, list) = if g.raw_len() > 0 {
        (180.0, g)
    } else {
        let g = Props::new(&g)?;
        (
            g.get("angle")?.unwrap_or(180.0),
            g.get::<Table>("stops")?.ok_or("gradient: needs stops")?,
        )
    };
    let raw: Vec<Value> = list
        .sequence_values::<Value>()
        .collect::<Result<_, _>>()
        .map_err(le)?;
    if raw.len() < 2 {
        return Err("gradient: needs at least two stops".into());
    }
    let one =
        |c: &str| color(colors, c, true).ok_or_else(|| format!("gradient: unknown color {c:?}"));
    let mut stops = Vec::new();
    for (i, v) in raw.iter().enumerate() {
        let even = i as f32 / (raw.len() - 1) as f32;
        stops.push(match v {
            Value::String(s) => (even, one(&s.to_str().map_err(le)?)?),
            Value::Table(p) => {
                let pos: f32 = p.get(1).map_err(le)?;
                let c: String = p.get(2).map_err(le)?;
                (pos.clamp(0.0, 1.0), one(&c)?)
            }
            _ => return Err("gradient: a stop is a color or { position, color }".into()),
        });
    }
    Ok(Some(Gradient { angle, stops }))
}

fn state(p: &Props, k: &str, colors: &dyn Colors) -> Result<Option<Box<State>>, String> {
    let Some(s) = p.sub(k)? else {
        return Ok(None);
    };
    Ok(Some(Box::new(State {
        bg: s.color("bg", colors, true)?,
        gradient: gradient(&s, colors)?,
        border_color: s.color("border_color", colors, false)?,
        color: s.color("color", colors, false)?,
        opacity: s.get("opacity")?,
        shadows: s.raw("shadow").map(|_| shadows(&s, colors)).transpose()?,
    })))
}

fn transition(p: &Props) -> Result<Option<Transition>, String> {
    Ok(match p.raw("transition") {
        None | Some(Value::Boolean(false)) => None,
        Some(Value::Table(t)) => {
            let t = Props::new(t)?;
            Some(Transition {
                duration_ms: t.get("duration")?.unwrap_or(150.0),
                easing: t.choice("easing")?.unwrap_or_default(),
            })
        }
        Some(v) => Some(Transition {
            duration_ms: num(v)
                .ok_or("transition: a number of milliseconds or { duration, easing }")?
                as f32,
            easing: Easing::default(),
        }),
    })
}

fn scrollbar(p: &Props, colors: &dyn Colors) -> Result<Scrollbar, String> {
    let d = Scrollbar::default();
    let Some(s) = p.sub("scrollbar")? else {
        return Ok(d);
    };
    Ok(Scrollbar {
        width: s.get("width")?.unwrap_or(d.width),
        color: s.color("color", colors, true)?.unwrap_or(d.color),
        radius: s.get("radius")?.unwrap_or(d.radius),
        inset: s.get("inset")?.unwrap_or(d.inset),
        show: s.choice("show")?.unwrap_or_default(),
    })
}

/// What a node or span says about its text, over what it inherits: its
/// highlight group first, then its own fields.
fn apply_text(
    p: &Props,
    colors: &dyn Colors,
    text: &mut TextStyle,
    hl: Option<String>,
) -> Result<(), String> {
    if let Some(g) = hl {
        apply_group(text, &colors.style(&g));
    }
    if let Some(c) = p.get::<String>("color")? {
        text.color = color(colors, &c, false).ok_or_else(|| format!("color: unknown {c:?}"))?;
    }
    if let Some(n) = p.get::<f32>("size")? {
        text.size = n;
    }
    apply_font(text, p.get("font")?);
    if let Some(b) = p.get::<bool>("bold")? {
        text.bold = b;
    }
    if let Some(b) = p.get::<bool>("italic")? {
        text.italic = b;
    }
    Ok(())
}

/// A node from its Lua table; `inherit` is the text style around it.
pub fn parse(
    t: &Table,
    colors: &dyn Colors,
    inherit: &TextStyle,
    sink: &Sink,
) -> Result<Node, String> {
    let p = Props::new(t)?;
    let mut text = inherit.clone();
    apply_text(&p, colors, &mut text, p.get("hl")?)?;
    if let Some(b) = p.get::<bool>("strike")? {
        text.strike = b;
    }
    if let Some(n) = p.get::<f32>("line_height")? {
        text.line_height = Some(n);
    }
    if let Some(n) = p.get::<f32>("letter_spacing")? {
        text.letter_spacing = n;
    }

    let units = Units {
        em: text.size,
        vw: sink.viewport.0,
        vh: sink.viewport.1,
    };
    let (margin, margin_auto) = margin(&p)?;
    let mut style = Style {
        direction: p.choice("direction")?.unwrap_or_default(),
        gap: p.get("gap")?.unwrap_or(0.0),
        align: p.choice("align")?.unwrap_or_default(),
        justify: p.choice("justify")?.unwrap_or_default(),
        width: dim(&p, "width", &units)?,
        height: dim(&p, "height", &units)?,
        min_width: dim(&p, "min_width", &units)?,
        max_width: dim(&p, "max_width", &units)?,
        min_height: dim(&p, "min_height", &units)?,
        max_height: dim(&p, "max_height", &units)?,
        grow: p.get("grow")?.unwrap_or(0.0),
        shrink: p.get("shrink")?,
        padding: edges(&p, "padding")?,
        margin,
        margin_auto,
        flex_wrap: p.get("flex_wrap")?.unwrap_or(false),
        row_gap: p.get("row_gap")?,
        column_gap: p.get("column_gap")?,
        align_self: p.choice("align_self")?,
        order: p.get("order")?.unwrap_or(0),
        aspect_ratio: p.get("aspect_ratio")?,
        bg: p.color("bg", colors, true)?,
        gradient: gradient(&p, colors)?,
        border: edges(&p, "border")?,
        border_style: p.choice("border_style")?.unwrap_or_default(),
        border_color: p.color("border_color", colors, false)?,
        outline: match p.sub("outline")? {
            Some(o) => Some(Outline {
                width: o.get("width")?.unwrap_or(2.0),
                color: o
                    .color("color", colors, false)?
                    .unwrap_or(Rgba(255, 255, 255, 120)),
                offset: o.get("offset")?.unwrap_or(1.0),
            }),
            None => None,
        },
        radius: corners(&p, "radius")?,
        opacity: p.get("opacity")?,
        hover: {
            let mut h = state(&p, "hover", colors)?;
            // The old one-field form.
            if let Some(bg) = p.color("hover_bg", colors, true)? {
                h.get_or_insert_with(Default::default).bg = Some(bg);
            }
            h
        },
        active: state(&p, "active", colors)?,
        focus: state(&p, "focus", colors)?,
        tooltip: p.get("tooltip")?,
        transition: transition(&p)?,
        scrollbar: scrollbar(&p, colors)?,
        overflow: p.choice("overflow")?.unwrap_or_default(),
        follow: p.get("follow")?.unwrap_or(false),
        cursor: p.get("cursor")?,
        absolute: p
            .pick("position", &[("relative", false), ("absolute", true)])?
            .unwrap_or(false),
        top: dim(&p, "top", &units)?,
        right: dim(&p, "right", &units)?,
        bottom: dim(&p, "bottom", &units)?,
        left: dim(&p, "left", &units)?,
        z: p.get("z")?.unwrap_or(0),
        block_input: p.get("block_input")?.unwrap_or(false),
        focusable: p.get("focusable")?,
        autofocus: p.get("autofocus")?.unwrap_or(false),
        key_guard: p.get("key_guard")?.unwrap_or(0.0),
        role: p.choice("role")?,
        label: p.get("label")?,
        selected: p.get("selected")?,
        shadows: shadows(&p, colors)?,
    };
    let handlers = Handlers {
        click: p.on("on_click", sink)?,
        secondary: p.on("on_secondary", sink)?,
        middle: p.on("on_middle", sink)?,
        double_click: p.on("on_double_click", sink)?,
        press: p.on("on_press", sink)?,
        release: p.on("on_release", sink)?,
        drag: p.on("on_drag", sink)?,
        hover: p.on("on_hover", sink)?,
        scroll: p.on("on_scroll", sink)?,
        key: p.on("on_key", sink)?,
    };

    // `text_input = true`: its options are the node's own fields.
    let ti_own = match p.raw("text_input") {
        Some(Value::Table(t)) => Some(Props::new(t)?),
        _ => None,
    };
    let ti = ti_own
        .as_ref()
        .or_else(|| matches!(p.raw("text_input"), Some(Value::Boolean(true))).then_some(&p));
    let kind = if let Some(l) = p.sub("list")? {
        style.overflow = Overflow::Scroll;
        style.direction = Direction::Column;
        Kind::List(List {
            count: l.get("count")?.unwrap_or(0),
            estimate: l.get("estimate")?.unwrap_or(60.0),
            gap: l.get("gap")?.unwrap_or(0.0),
            key: l.get("key")?.unwrap_or_default(),
            item: l.get("item")?.ok_or("list: item must be a function")?,
            total: 0.0,
            fit: l.get("fit")?.unwrap_or(false),
            chat: !matches!(
                l.raw("chat"),
                None | Some(Value::Nil | Value::Boolean(false))
            ),
            buffer: match l.raw("chat") {
                Some(Value::Integer(n)) => usize::try_from(*n).ok(),
                Some(Value::Number(n)) if *n >= 0.0 => Some(*n as usize),
                Some(Value::Boolean(_) | Value::Nil) | None => None,
                Some(_) => return Err("list: chat must be true or a chat number".into()),
            },
        })
    } else if let Some(i) = p.sub("image")? {
        let source = match (i.get::<String>("id")?, i.get::<String>("path")?) {
            (Some(id), _) => ImageSource::Attachment(id),
            (None, Some(path)) => ImageSource::Path(path),
            _ => return Err("image: needs an id or a path".into()),
        };
        let natural = match (i.get::<f32>("width")?, i.get::<f32>("height")?) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((w, h)),
            _ => None,
        };
        // The box has the picture's shape.
        if let (Some((w, h)), None) = (natural, style.aspect_ratio) {
            style.aspect_ratio = Some(w / h);
        }
        Kind::Image(Image {
            source,
            fit: i.choice("fit")?.unwrap_or_default(),
            natural,
        })
    } else if let Some(c) = p.sub("canvas")? {
        Kind::Canvas(Canvas {
            draw: c
                .on("draw", sink)?
                .ok_or("canvas: draw must be a function")?,
            fps: c.get("fps")?.unwrap_or(0.0),
        })
    } else if let Some(ti) = ti {
        Kind::TextInput(TextInput {
            id: p.get("id")?.ok_or("text_input: the node needs an id")?,
            value: ti.get("value")?,
            placeholder: ti.get("placeholder")?.unwrap_or_default(),
            multiline: ti.get("multiline")?.unwrap_or(false),
            min_rows: ti.get("min_rows")?.unwrap_or(1),
            max_rows: ti.get("max_rows")?.unwrap_or(6),
            password: ti.get("password")?.unwrap_or(false),
            read_only: ti.get("read_only")?.unwrap_or(false),
            focus: ti.get("focus")?.unwrap_or(false),
            select: ti.get("select")?.unwrap_or(false),
            on_change: ti.on("on_change", sink)?,
            on_submit: ti.on("on_submit", sink)?,
            text: text.clone(),
        })
    } else if p.get::<bool>("input")?.unwrap_or(false) {
        Kind::Input(Input {
            placeholder: p.get("placeholder")?.unwrap_or_default(),
            text: text.clone(),
            min_rows: p.get("min_rows")?.unwrap_or(1),
            max_rows: p.get("max_rows")?.unwrap_or(8),
        })
    } else if let Some(spans) = p.get::<Table>("spans")? {
        let spans = (spans.sequence_values::<Value>())
            .map(|s| span(&s.map_err(le)?, colors, &text))
            .collect::<Result<_, _>>()?;
        Kind::Text(text_of(&p, spans, colors)?)
    } else if let Some(s) = p.get::<String>("text")? {
        Kind::Text(text_of(&p, vec![Span::plain(s, text.clone())], colors)?)
    } else {
        Kind::Box
    };

    let mut children = Vec::new();
    if matches!(kind, Kind::Box) {
        for (i, c) in &p.items {
            match c {
                Value::Table(c) => children
                    .push(parse(c, colors, &text, sink).map_err(|e| format!("child {i}: {e}"))?),
                Value::Boolean(false) => {}
                other => {
                    return Err(format!(
                        "child {i}: expected a node table, got a {}",
                        other.type_name()
                    ));
                }
            }
        }
    }
    Ok(Node {
        id: p.get("id")?,
        style,
        handlers,
        kind,
        children,
    })
}

fn text_of(p: &Props, spans: Vec<Span>, colors: &dyn Colors) -> Result<Text, String> {
    Ok(Text {
        spans,
        align: p.choice("text_align")?.unwrap_or_default(),
        wrap: p.get("wrap")?.unwrap_or(true),
        selectable: p.get("selectable")?.unwrap_or(false),
        ellipsis: p
            .pick("text_overflow", &[("clip", false), ("ellipsis", true)])?
            .unwrap_or(false),
        shadow: match p.sub("text_shadow")? {
            Some(s) => Some(TextShadow {
                dx: s.get("dx")?.unwrap_or(0.0),
                dy: s.get("dy")?.unwrap_or(1.0),
                color: s
                    .color("color", colors, false)?
                    .unwrap_or(Rgba(0, 0, 0, 160)),
            }),
            None => None,
        },
    })
}

/// A span: a string, `{ text, color, bold, italic, size, font, ... }`, or
/// `{ "text", "Group" }` as a line's spans are written for the TUI.
fn span(v: &Value, colors: &dyn Colors, base: &TextStyle) -> Result<Span, String> {
    let t = match v {
        Value::String(s) => {
            return Ok(Span::plain(
                s.to_str().map_err(le)?.to_owned(),
                base.clone(),
            ));
        }
        Value::Table(t) => Props::new(t)?,
        other => {
            return Err(format!(
                "span: expected a string or table, got a {}",
                other.type_name()
            ));
        }
    };
    let positional = t.raw("text").is_none();
    let mut style = base.clone();
    let hl = match t.get::<String>("hl")? {
        None if positional => t.item(2),
        hl => hl,
    };
    // A group's background is a chip behind code, not behind every span
    // (Normal has one: the window's).
    let gs = hl.as_ref().map(|g| colors.style(g));
    let (mut underline, dim) = gs.as_ref().map_or((false, false), |g| (g.underline, g.dim));
    apply_text(&t, colors, &mut style, hl)?;
    underline |= t.get::<bool>("link")?.unwrap_or(false);
    // An unknown group is no chip, not an error: a window's lines name groups
    // the colorscheme may not set.
    let mut bg = t.get::<String>("bg")?.and_then(|c| color(colors, &c, true));
    if t.get::<bool>("code")?.unwrap_or(false) {
        style.mono = true;
        bg = bg.or_else(|| colors.group("CodeBlock", true));
    }
    let text = match t.get::<String>("text")? {
        None if positional => t.item(1),
        text => text,
    };
    Ok(Span {
        text: text.unwrap_or_default(),
        style,
        bg,
        underline,
        strike: t.get("strike")?.unwrap_or(false),
        dim,
    })
}

/// `font`: `"sans"`, `"mono"`, or a family registered with `bone.gui.font`.
fn apply_font(style: &mut TextStyle, font: Option<String>) {
    match font.as_deref() {
        None => {}
        Some("sans") => (style.mono, style.family) = (false, None),
        Some("mono") => (style.mono, style.family) = (true, None),
        Some(name) => style.family = Some(name.to_owned()),
    }
}

/// What a highlight group says about text: its color and attributes.
fn apply_group(style: &mut TextStyle, g: &GroupStyle) {
    if let Some(c) = g.fg {
        style.color = c;
    }
    style.bold |= g.bold;
    style.italic |= g.italic;
}
