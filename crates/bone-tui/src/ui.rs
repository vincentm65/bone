//! Where Lua draws the screen:
//!
//! - `bone.ui.views[kind](item, ctx)` renders each transcript item (user,
//!   reasoning, assistant, tool, notice) to lines.
//! - `bone.ui.regions[name]` fills `top`, `left`, `right` and `above_prompt`.
//! - `bone.ui.statusline(ctx)` and `bone.ui.divider(ctx)` draw those rows;
//!   without them the rows are not there.
//! - `bone.ui.prompt` sets how the prompt box looks: prefix, placeholder,
//!   border, background, padding and lines in its top and bottom edges.
//!   Rust keeps the text: it wraps, scrolls, selects and places the cursor.
//!
//! A line is a string or a list of items: `"text"`, `{ text, hl }`,
//! `{ fill = "─", hl = ... }` (stretches to fill the row) or `"%="` (a
//! blank fill). Without Lua, Rust draws plain text and nothing else.

use bone_lua::to_lua;
use mlua::{Function, Table, Value};
use ratatui::text::{Line, Span};
use serde_json::{Value as Json, json};

use crate::app::App;
use crate::chat::{Item as ChatItem, RenderKey, bare};
use crate::layout::BufferId;
use crate::text::{sanitize, truncate, width};
use crate::theme::Theme;

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// `bone.ui.prompt`, as Rust needs it to lay out and draw the box.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptSpec {
    pub prefix: Vec<Item>,
    /// Before wrapped and later rows; default: blank, as wide as `prefix`.
    pub continuation: Option<Vec<Item>>,
    pub placeholder: Vec<Item>,
    pub border: Option<Border>,
    /// A highlight group filling the box.
    pub background: Option<String>,
    /// Blank rows above and below the text, and columns beside it, inside
    /// the border.
    pub pad_rows: u16,
    pub pad_cols: u16,
    /// Text rows the box shows at least, and at most (default
    /// `bone.o.prompt_max_height`).
    pub min_rows: usize,
    pub max_rows: Option<usize>,
    /// `top`/`bottom` lines are set: they get a row even without a border.
    pub has_top: bool,
    pub has_bottom: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Border {
    /// Top-left, top-right, bottom-left, bottom-right, horizontal, vertical.
    pub chars: [char; 6],
    pub hl: String,
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

const BORDERS: [(&str, &str); 5] = [
    ("rounded", "╭╮╰╯─│"),
    ("single", "┌┐└┘─│"),
    ("double", "╔╗╚╝═║"),
    ("thick", "┏┓┗┛━┃"),
    ("ascii", "++++-|"),
];

/// `border = "rounded"`, `true`, or `{ style | chars, hl, sides = "tblr" }`.
fn border_of(v: Value) -> mlua::Result<Option<Border>> {
    let (style, hl, sides) = match v {
        Value::Nil | Value::Boolean(false) => return Ok(None),
        Value::Boolean(true) => ("rounded".to_owned(), None, None),
        Value::String(s) => (s.to_str()?.to_owned(), None, None),
        Value::Table(t) => (
            t.get::<Option<String>>("chars")?
                .or(t.get::<Option<String>>("style")?)
                .unwrap_or_else(|| "rounded".into()),
            t.get::<Option<String>>("hl")?,
            t.get::<Option<String>>("sides")?,
        ),
        other => {
            return Err(mlua::Error::runtime(format!(
                "border is a style name or a table, not {}",
                other.type_name()
            )));
        }
    };
    let chars = BORDERS
        .iter()
        .find(|(name, _)| *name == style)
        .map_or(style.as_str(), |(_, c)| c);
    let chars: Vec<char> = chars.chars().collect();
    let chars: [char; 6] = chars.try_into().map_err(|_| {
        mlua::Error::runtime(format!(
            "border style {style:?} is not one of rounded, single, double, thick, ascii, or 6 characters"
        ))
    })?;
    let sides = sides.unwrap_or_else(|| "tblr".into());
    Ok(Some(Border {
        chars,
        hl: hl.unwrap_or_else(|| "PromptBorder".into()),
        top: sides.contains('t'),
        bottom: sides.contains('b'),
        left: sides.contains('l'),
        right: sides.contains('r'),
    }))
}

/// A line, or a function of `ctx` returning one.
fn line_of(lua: &mlua::Lua, v: Value, ctx: &Json, hl: &str) -> mlua::Result<Vec<Item>> {
    match v {
        Value::Function(f) => parse_items(f.call::<Value>(to_lua(lua, ctx)?)?, hl),
        v => parse_items(v, hl),
    }
}

/// A styled range of the prompt: 0-based row, chars `from..to`.
#[derive(Debug, Clone)]
pub struct Mark {
    pub row: usize,
    pub from: usize,
    pub to: usize,
    pub hl: String,
}

#[derive(Debug, Clone, Default)]
pub struct PromptMarks {
    pub ranges: Vec<Mark>,
    pub ghost: Option<String>,
    pub ghost_hl: String,
}

/// How much room a layout node takes along its split.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    /// Its natural size: a region's own `size` or content, the prompt's
    /// text, one row for the statusline and divider.
    Auto,
    /// A share of what is left (the chat and splits by default).
    Fill,
    Cells(u16),
    Percent(u16),
}

/// `bone.ui.layout`: leaves are built-ins (`chat`, `prompt`, `divider`,
/// `statusline`, `message`) or region names; splits stack rows or place
/// columns side by side.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    Leaf {
        name: String,
        size: Size,
    },
    Split {
        rows: bool,
        /// Drawn between columns, one cell wide.
        sep: Option<String>,
        size: Size,
        children: Vec<LayoutNode>,
    },
}

impl LayoutNode {
    fn leaf(name: &str, size: Option<Size>) -> Self {
        let default = if name == "chat" {
            Size::Fill
        } else {
            Size::Auto
        };
        LayoutNode::Leaf {
            name: name.to_owned(),
            size: size.unwrap_or(default),
        }
    }

    pub fn size(&self) -> Size {
        match self {
            LayoutNode::Leaf { size, .. } | LayoutNode::Split { size, .. } => *size,
        }
    }

    pub fn contains(&self, leaf: &str) -> bool {
        match self {
            LayoutNode::Leaf { name, .. } => name == leaf,
            LayoutNode::Split { children, .. } => children.iter().any(|c| c.contains(leaf)),
        }
    }
}

fn parse_size(v: Value) -> mlua::Result<Option<Size>> {
    Ok(match v {
        Value::Nil => None,
        Value::Integer(n) => Some(Size::Cells(n.max(0) as u16)),
        Value::Number(n) => Some(Size::Cells(n.max(0.0) as u16)),
        Value::String(s) => {
            let s = s.to_str()?.to_owned();
            match s.as_str() {
                "auto" => Some(Size::Auto),
                "fill" => Some(Size::Fill),
                p if p.ends_with('%') => Some(Size::Percent(
                    p[..p.len() - 1]
                        .trim()
                        .parse::<u16>()
                        .map_err(|_| mlua::Error::runtime(format!("bad size {s:?}")))?
                        .min(100),
                )),
                _ => return Err(mlua::Error::runtime(format!("bad size {s:?}"))),
            }
        }
        other => {
            return Err(mlua::Error::runtime(format!(
                "a size is a number, \"auto\", \"fill\" or \"N%\", not {}",
                other.type_name()
            )));
        }
    })
}

fn layout_children(t: &Table) -> mlua::Result<Vec<LayoutNode>> {
    t.sequence_values::<Value>()
        .map(|v| layout_node(v?))
        .collect()
}

fn layout_node(v: Value) -> mlua::Result<LayoutNode> {
    match v {
        Value::String(s) => Ok(LayoutNode::leaf(&s.to_str()?, None)),
        Value::Table(t) => {
            let size = parse_size(t.get("size")?)?;
            for (key, rows) in [("rows", true), ("cols", false)] {
                if let Some(children) = t.get::<Option<Table>>(key)? {
                    return Ok(LayoutNode::Split {
                        rows,
                        sep: t.get("sep")?,
                        size: size.unwrap_or(Size::Fill),
                        children: layout_children(&children)?,
                    });
                }
            }
            let name: Option<String> = match t.get::<Option<String>>(1)? {
                Some(n) => Some(n),
                None => t.get("name")?,
            };
            match name {
                Some(n) => Ok(LayoutNode::leaf(&n, size)),
                None => Err(mlua::Error::runtime(
                    "a layout entry is a name, { name, size = … }, { rows = {…} } or { cols = {…} }",
                )),
            }
        }
        other => Err(mlua::Error::runtime(format!(
            "a layout entry is a name or a table, not {}",
            other.type_name()
        ))),
    }
}

/// The spinner's frames and how long each shows (`bone.ui.spinner`).
#[derive(Debug, Clone)]
pub struct Spinner {
    pub frames: Vec<String>,
    pub interval_ms: u64,
}

impl Default for Spinner {
    fn default() -> Self {
        Spinner {
            frames: SPINNER.iter().map(|c| c.to_string()).collect(),
            interval_ms: 100,
        }
    }
}

impl Spinner {
    /// The frame to show now.
    pub fn frame(&self) -> String {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()) as u64;
        let n = self.frames.len().max(1) as u64;
        self.frames
            .get(((ms / self.interval_ms.max(1)) % n) as usize)
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Text(String, String),
    Fill(String, String),
}

/// Lay items out in `width` columns. Fills share the leftover space; text
/// that does not fit is cut with `…`.
pub fn render_items(items: &[Item], width_cols: usize, theme: &Theme) -> Line<'static> {
    let text_w: usize = items
        .iter()
        .map(|i| match i {
            Item::Text(t, _) => width(t),
            Item::Fill(..) => 0,
        })
        .sum();
    let fills = items.iter().filter(|i| matches!(i, Item::Fill(..))).count();
    let spare = width_cols.saturating_sub(text_w);
    let mut spans = Vec::new();
    let mut used = 0;
    let mut fill_i = 0;
    for item in items {
        if used >= width_cols {
            break;
        }
        match item {
            Item::Text(t, hl) => {
                let room = width_cols - used;
                let t = if width(t) > room {
                    truncate(t, room)
                } else {
                    t.clone()
                };
                used += width(&t);
                spans.push(Span::styled(t, theme.hl(hl)));
            }
            Item::Fill(ch, hl) => {
                let n = spare / fills + usize::from(fill_i < spare % fills);
                fill_i += 1;
                let n = n.min(width_cols - used);
                used += n;
                let ch = ch.chars().next().unwrap_or(' ');
                spans.push(Span::styled(ch.to_string().repeat(n), theme.hl(hl)));
            }
        }
    }
    Line::from(spans)
}

/// One line: a string or a list of items.
pub fn parse_items(v: Value, default_hl: &str) -> mlua::Result<Vec<Item>> {
    // Text is drawn on one row: newlines become spaces.
    let text = |s: &str| sanitize(s).replace('\n', " ");
    let t = match v {
        Value::String(s) => return Ok(vec![Item::Text(text(&s.to_str()?), default_hl.into())]),
        Value::Nil => return Ok(Vec::new()),
        Value::Table(t) => t,
        other => {
            return Err(mlua::Error::runtime(format!(
                "a line is a string or a list, not {}",
                other.type_name()
            )));
        }
    };
    let mut out = Vec::new();
    for item in t.sequence_values::<Value>() {
        out.push(match item? {
            Value::String(s) if s.to_str()?.as_ref() == "%=" => {
                Item::Fill(" ".into(), default_hl.into())
            }
            Value::String(s) => Item::Text(text(&s.to_str()?), default_hl.into()),
            Value::Integer(i) => Item::Text(i.to_string(), default_hl.into()),
            Value::Number(n) => Item::Text(n.to_string(), default_hl.into()),
            Value::Table(it) => {
                let hl = it
                    .get::<Option<String>>("hl")?
                    .or(it.get::<Option<String>>(2)?)
                    .unwrap_or_else(|| default_hl.into());
                match it.get::<Option<String>>("fill")? {
                    Some(fill) => Item::Fill(fill, hl),
                    None => {
                        let t = it
                            .get::<Option<String>>("text")?
                            .or(it.get::<Option<String>>(1)?)
                            .unwrap_or_default();
                        Item::Text(text(&t), hl)
                    }
                }
            }
            other => {
                return Err(mlua::Error::runtime(format!(
                    "bad item: {}",
                    other.type_name()
                )));
            }
        });
    }
    Ok(out)
}

/// Display width of a line's text (fills count as nothing).
pub fn items_width(items: &[Item]) -> usize {
    items
        .iter()
        .map(|i| match i {
            Item::Text(t, _) => width(t),
            Item::Fill(..) => 0,
        })
        .sum()
}

/// A list of lines, as views, regions and panels return.
pub(crate) fn parse_lines(v: Value) -> mlua::Result<Option<Vec<Vec<Item>>>> {
    match v {
        Value::Nil => Ok(None),
        Value::Table(t) => t
            .sequence_values::<Value>()
            .map(|l| parse_items(l?, "Normal"))
            .collect::<mlua::Result<_>>()
            .map(Some),
        other => Err(mlua::Error::runtime(format!(
            "expected a list of lines or nil, not {}",
            other.type_name()
        ))),
    }
}

impl App {
    fn session_ctx(&self, buf: Option<BufferId>) -> Json {
        let Some(c) = buf.and_then(|b| self.chat(b)) else {
            return Json::Null;
        };
        let Some(s) = &c.session else {
            return json!({ "title": c.title(), "running": c.starting, "new": true });
        };
        json!({
            "session_id": s.session_id,
            "title": c.title(),
            "cwd": s.cwd,
            "running": c.turn.is_some() || c.starting,
            "elapsed": c.turn.map(|t| t.started.elapsed().as_secs()),
            "usage": c.usage.map(|u| json!({ "input": u.input_tokens, "output": u.output_tokens })),
        })
    }

    fn popup_name(&self) -> Json {
        match self.context() {
            crate::keymap::Context::Main => Json::Null,
            c => json!(c.name()),
        }
    }

    /// `bone.ui.spinner = { frames = { ... }, interval = ms }`, or the
    /// default; read once per frame.
    pub fn read_spinner(&mut self) -> Spinner {
        self.guarded("spinner", |lua| {
            let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
            let Some(t) = ui.get::<Option<Table>>("spinner")? else {
                return Ok(None);
            };
            let frames: Vec<String> = t.get::<Option<Vec<String>>>("frames")?.unwrap_or_default();
            if frames.is_empty() {
                return Err(mlua::Error::runtime("bone.ui.spinner needs frames"));
            }
            let interval: Option<f64> = t.get("interval")?;
            Ok(Some(Spinner {
                frames,
                interval_ms: interval.unwrap_or(100.0).max(16.0) as u64,
            }))
        })
        .flatten()
        .unwrap_or_default()
    }

    /// `bone.ui.title(ctx)`: the terminal's title, or None to leave it.
    pub fn ui_title(&mut self) -> Option<String> {
        if !self.ui_defined("title") {
            return None;
        }
        let ctx = self.statusline_ctx(self.screen.width);
        self.guarded("title", |lua| {
            let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
            let f: Function = ui.get("title")?;
            f.call::<Option<String>>(to_lua(lua, &ctx)?)
        })
        .flatten()
        .map(|t| crate::text::sanitize(&t))
    }

    pub fn statusline_ctx(&self, width_cols: u16) -> Json {
        json!({
            "title": self.chats[self.current].title(),
            "popup": self.popup_name(),
            "panel": self.focused_panel().map(|p| p.id.clone()),
            "jobs": self.jobs.running(),
            "spinner": self.spinner.frame(),
            "width": width_cols,
            "session": self.session_ctx(Some(self.current)),
        })
    }

    /// The line between the chat and the prompt.
    pub fn divider_ctx(&self, width_cols: u16) -> Json {
        json!({
            "spinner": self.spinner.frame(),
            "width": width_cols,
            "session": self.session_ctx(Some(self.current)),
        })
    }

    /// `bone.ui.layout`: the rows of the screen, top to bottom. `chat`,
    /// `prompt`, `divider` and `statusline` are built in; any other name is
    /// a region from `bone.ui.regions`.
    /// `bone.ui.layout` as a tree: the top level is a stack of rows.
    pub fn layout(&mut self) -> LayoutNode {
        const DEFAULT: [&str; 6] = [
            "top",
            "chat",
            "divider",
            "above_prompt",
            "prompt",
            "statusline",
        ];
        // The chat with `left` and `right` columns beside it (empty unless
        // those regions are defined).
        let default = || LayoutNode::Split {
            rows: true,
            sep: None,
            size: Size::Fill,
            children: DEFAULT
                .iter()
                .map(|&n| {
                    if n == "chat" {
                        LayoutNode::Split {
                            rows: false,
                            sep: Some("│".into()),
                            size: Size::Fill,
                            children: ["left", "chat", "right"]
                                .iter()
                                .map(|n| LayoutNode::leaf(n, None))
                                .collect(),
                        }
                    } else {
                        LayoutNode::leaf(n, None)
                    }
                })
                .collect(),
        };
        self.guarded("layout", |lua| {
            let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
            match ui.get::<Value>("layout")? {
                Value::Nil => Ok(default()),
                Value::Table(t) => Ok(LayoutNode::Split {
                    rows: true,
                    sep: None,
                    size: Size::Fill,
                    children: layout_children(&t)?,
                }),
                other => Err(mlua::Error::runtime(format!(
                    "bone.ui.layout is a list, not {}",
                    other.type_name()
                ))),
            }
        })
        .unwrap_or_else(default)
    }

    /// Whether `bone.ui[name]` is a function (the statusline and divider
    /// rows only exist when Lua defines them).
    pub fn ui_defined(&self, name: &str) -> bool {
        let Some(lua) = &self.lua else { return false };
        let ui = || -> mlua::Result<bool> {
            let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
            Ok(matches!(ui.get::<Value>(name)?, Value::Function(_)))
        };
        ui().unwrap_or(false)
    }

    /// `bone.ui.prompt`, read with `ctx` (fields may be functions of it).
    /// Nothing set means a bare prompt.
    pub fn prompt_spec(&mut self, ctx: &Json) -> PromptSpec {
        self.guarded("prompt", |lua| {
            let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
            let Some(p) = ui.get::<Option<Table>>("prompt")? else {
                return Ok(PromptSpec::default());
            };
            let line = |key: &str, hl: &str| -> mlua::Result<Vec<Item>> {
                line_of(lua, p.get(key)?, ctx, hl)
            };
            let border = border_of(p.get("border")?)?;
            let (pad_rows, pad_cols) = match p.get::<Value>("padding")? {
                Value::Nil => (0, 0),
                Value::Integer(n) => (0, n.max(0) as u16),
                Value::Number(n) => (0, n.max(0.0) as u16),
                Value::Table(t) => (
                    t.get::<Option<u16>>(1)?.unwrap_or(0),
                    t.get::<Option<u16>>(2)?.unwrap_or(0),
                ),
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "padding is a number or {{ rows, cols }}, not {}",
                        other.type_name()
                    )));
                }
            };
            let continuation = match p.get::<Value>("continuation")? {
                Value::Nil => None,
                v => Some(line_of(lua, v, ctx, "UserPrompt")?),
            };
            Ok(PromptSpec {
                prefix: line("prefix", "UserPrompt")?,
                continuation,
                placeholder: line("placeholder", "Placeholder")?,
                background: p.get("background")?,
                pad_rows,
                pad_cols,
                min_rows: p.get::<Option<usize>>("min")?.unwrap_or(1).max(1),
                max_rows: p.get::<Option<usize>>("max")?,
                has_top: !matches!(p.get::<Value>("top")?, Value::Nil),
                has_bottom: !matches!(p.get::<Value>("bottom")?, Value::Nil),
                border,
            })
        })
        .unwrap_or_default()
    }

    /// The line `bone.ui.prompt.top` or `.bottom` draws into that edge.
    pub fn prompt_edge(&mut self, edge: &str, ctx: &Json, hl: &str) -> Vec<Item> {
        self.guarded("prompt", |lua| {
            let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
            let Some(p) = ui.get::<Option<Table>>("prompt")? else {
                return Ok(Vec::new());
            };
            line_of(lua, p.get(edge)?, ctx, hl)
        })
        .unwrap_or_default()
    }

    /// What `bone.ui.prompt` functions get: the box and its state. Layout
    /// fields (`rows`, `height`, `text_width`, `cursor.screen`) only reach
    /// `top` and `bottom`, which are drawn after the text is laid out.
    /// `bone.ui.prompt_highlight(ctx)`: styles for ranges of the prompt's
    /// text, and ghost text to show after the cursor.
    pub fn prompt_highlight(&mut self, ctx: &Json) -> PromptMarks {
        self.guarded("prompt_highlight", |lua| {
            let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
            let Some(f) = ui.get::<Option<Function>>("prompt_highlight")? else {
                return Ok(None);
            };
            let Some(t) = f.call::<Option<Table>>(to_lua(lua, ctx)?)? else {
                return Ok(None);
            };
            let mut marks = PromptMarks::default();
            if let Some(list) = t.get::<Option<Table>>("highlights")? {
                for h in list.sequence_values::<Table>() {
                    let h = h?;
                    marks.ranges.push(Mark {
                        row: h.get("row")?,
                        from: h.get("from")?,
                        to: h.get("to")?,
                        hl: h.get("hl")?,
                    });
                }
            }
            marks.ghost = t.get("ghost")?;
            marks.ghost_hl = t
                .get::<Option<String>>("ghost_hl")?
                .unwrap_or_else(|| "Placeholder".into());
            Ok(Some(marks))
        })
        .flatten()
        .unwrap_or_default()
    }

    pub fn prompt_ctx(&self, width_cols: u16) -> Json {
        let (row, col) = self.prompt.cursor();
        let selection = self.prompt.selection().map(|(a, b)| {
            json!({ "start": { "row": a.0, "col": a.1 }, "end": { "row": b.0, "col": b.1 } })
        });
        let session = self.session_ctx(Some(self.current));
        json!({
            "width": width_cols,
            "text": self.prompt.text(),
            "lines": self.prompt.lines().len(),
            "empty": self.prompt.is_empty(),
            "cursor": { "row": row, "col": col },
            "selection": selection,
            "focused": self.focused_popup().is_none() && self.focused_panel().is_none(),
            "running": session["running"].as_bool().unwrap_or(false),
            "elapsed": session["elapsed"],
            "spinner": self.spinner.frame(),
            "session": session,
        })
    }

    /// Run Lua that may fail; a failure is reported once and `name` is
    /// skipped until the UI is refreshed (`bone.ui.refresh`, or redefining it).
    pub(crate) fn guarded<R>(
        &mut self,
        name: &str,
        body: impl FnOnce(&mlua::Lua) -> mlua::Result<R>,
    ) -> Option<R> {
        if self.lua.is_none() || self.ui_broken.contains(name) {
            return None;
        }
        match self.with_api(body) {
            Ok(r) => Some(r),
            Err(e) => {
                self.ui_broken.insert(name.to_owned());
                self.lua_error(
                    &format!("bone.ui.{name} failed (plain text until it is redefined)"),
                    &e,
                );
                None
            }
        }
    }

    /// Call `bone.ui[name](ctx)` for a single row (statusline, divider).
    pub fn ui_items(&mut self, name: &str, ctx: Json, default_hl: &str) -> Option<Vec<Item>> {
        let default_hl = default_hl.to_owned();
        self.guarded(name, |lua| {
            let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
            let Some(f) = ui.get::<Option<Function>>(name)? else {
                return Ok(None);
            };
            let v = f.call::<Value>(to_lua(lua, &ctx)?)?;
            parse_items(v, &default_hl).map(Some)
        })
        .flatten()
    }

    /// Render the stale items of a chat through `bone.ui.views` and cache them.
    pub fn render_chat(&mut self, buf: BufferId, width_cols: usize) {
        let generation = (self.views_rev, self.opts_rev);
        // Views that read other chat data are drawn again when it changed.
        let data = self.data_generation();
        // Each chat is indexed once, not once per view that read it.
        let mut chats = std::collections::HashMap::new();
        let checked: Vec<_> = self.chats[buf]
            .unchecked_deps(data)
            .into_iter()
            .map(|(item, deps)| {
                let fresh = deps.iter().all(|d| {
                    let session = &d.filter.session;
                    let chat = (chats.entry(session.clone()))
                        .or_insert_with(|| self.indexed(session.as_deref()));
                    chat.as_ref().map_or(0, |c| c.signature(&d.filter)) == d.sig
                });
                (item, fresh)
            })
            .collect();
        for (item, fresh) in checked {
            if fresh {
                self.chats[buf].mark_checked(item, data);
            } else {
                self.chats[buf].invalidate(item);
            }
        }
        let stale = self.chats[buf].stale(width_cols, generation, std::time::Instant::now());
        let items = if stale.is_empty() {
            Vec::new()
        } else {
            self.chats[buf].items()
        };
        for (item, index, key) in stale {
            let prev = index
                .checked_sub(2)
                .and_then(|p| items.get(p))
                .map(|p| self.chats[buf].kind_name(p).to_owned());
            self.render_deps = Some(Vec::new());
            self.render_expires = None;
            let lines = self.render_item(buf, item, index, &key, prev);
            let deps = self.render_deps.take().unwrap_or_default();
            let expires = self.render_expires.take();
            self.chats[buf].store(item, key, lines, deps, expires, data);
        }
        self.chat_expiry = self.chats[buf].next_expiry();
    }

    fn render_item(
        &mut self,
        buf: BufferId,
        item: ChatItem,
        index: usize,
        key: &RenderKey,
        prev: Option<String>,
    ) -> Vec<Line<'static>> {
        let data = self.chats[buf].item_data(item, index);
        let kind = self.chats[buf].kind_name(&item).to_owned();
        let ctx = json!({
            "width": key.width,
            "region": "chat",
            "prev": prev.map(|kind| json!({ "kind": kind })),
        });
        let view_data = data.clone();
        let lines = self
            .guarded(&format!("views.{kind}"), |lua| {
                let views: Table = lua
                    .globals()
                    .get::<Table>("bone")?
                    .get::<Table>("ui")?
                    .get("views")?;
                let Some(f) = views.get::<Option<Function>>(kind.as_str())? else {
                    return Ok(None);
                };
                let v = f.call::<Value>((to_lua(lua, &view_data)?, to_lua(lua, &ctx)?))?;
                // nil hides the item.
                Ok(Some(parse_lines(v)?.unwrap_or_default()))
            })
            .flatten();
        match lines {
            Some(lines) => lines
                .iter()
                .map(|l| render_items(l, key.width, &self.theme))
                .collect(),
            None => bare(&data, key.width, key.prev.is_none()),
        }
    }

    /// A region in a stack of rows (`rows`: sized in rows, up to `height`)
    /// or a row of columns (sized in columns, up to half of `width_cols`).
    /// `exact` draws it at exactly `width_cols` × `height`, whatever its size.
    pub fn region_sized(
        &mut self,
        name: &str,
        width_cols: u16,
        height: u16,
        rows: bool,
        exact: bool,
    ) -> Option<(u16, Vec<Line<'static>>)> {
        let base = json!({
            "region": name,
            "spinner": self.spinner.frame(),
            "popup": self.popup_name(),
            "session": self.session_ctx(Some(self.current)),
        });
        let (size, lines) = self
            .guarded(&format!("regions.{name}"), |lua| {
                let regions: Table = lua
                    .globals()
                    .get::<Table>("bone")?
                    .get::<Table>("ui")?
                    .get("regions")?;
                let (render, size, max): (Function, Value, u16) =
                    match regions.get::<Value>(name)? {
                        Value::Nil => return Ok(None),
                        Value::Function(f) => (f, Value::Nil, 10),
                        Value::Table(t) => (
                            t.get("render")?,
                            t.get("size")?,
                            t.get::<Option<u16>>("max")?.unwrap_or(10),
                        ),
                        other => {
                            return Err(mlua::Error::runtime(format!(
                                "a region is a table or function, not {}",
                                other.type_name()
                            )));
                        }
                    };
                let call = |w: u16, h: u16| -> mlua::Result<Option<Vec<Vec<Item>>>> {
                    let mut ctx = base.clone();
                    ctx["width"] = json!(w);
                    ctx["height"] = json!(h);
                    parse_lines(render.call::<Value>(to_lua(lua, &ctx)?)?)
                };
                let fixed = match size {
                    Value::Integer(n) => Some(n.max(0) as u16),
                    Value::Number(n) => Some(n.max(0.0) as u16),
                    _ => None,
                };
                if exact {
                    let Some(lines) = call(width_cols, height)? else {
                        return Ok(None);
                    };
                    return Ok(Some((if rows { height } else { width_cols }, lines)));
                }
                Ok(Some(if rows {
                    let h = fixed.unwrap_or(max).min(height);
                    let Some(lines) = call(width_cols, h)? else {
                        return Ok(None);
                    };
                    let h = if fixed.is_some() {
                        h
                    } else {
                        (lines.len() as u16).min(h)
                    };
                    (h, lines)
                } else {
                    let w = fixed.unwrap_or(30).min(width_cols / 2);
                    let Some(lines) = call(w, height)? else {
                        return Ok(None);
                    };
                    (w, lines)
                }))
            })
            .flatten()?;
        if size == 0 {
            return None;
        }
        let w = if rows { width_cols } else { size } as usize;
        Some((
            size,
            lines
                .iter()
                .map(|l| render_items(l, w, &self.theme))
                .collect(),
        ))
    }

    /// A popup's lines: its function called with `{ width }`, or its list.
    pub fn popup_lines(
        &mut self,
        id: u64,
        cb: u64,
        width_cols: u16,
        height: u16,
    ) -> Vec<Line<'static>> {
        let lines = self
            .guarded(&format!("popup.{id}"), |lua| {
                let v: Value = lua
                    .named_registry_value::<Table>("bone.callbacks")?
                    .get(cb)?;
                let v = match v {
                    Value::Function(f) => f.call::<Value>(to_lua(
                        lua,
                        &json!({ "width": width_cols, "height": height }),
                    )?)?,
                    v => v,
                };
                parse_lines(v)
            })
            .flatten()
            .unwrap_or_default();
        lines
            .iter()
            .map(|l| render_items(l, width_cols as usize, &self.theme))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_fill_and_truncate() {
        let t = Theme::default();
        let items = vec![
            Item::Text("ab".into(), "Normal".into()),
            Item::Fill("-".into(), "Dim".into()),
            Item::Text("cd".into(), "Normal".into()),
        ];
        assert_eq!(render_items(&items, 8, &t).to_string(), "ab----cd");
        assert_eq!(render_items(&items, 3, &t).to_string(), "ab…");
        let long = vec![Item::Text("abcdef".into(), "Normal".into())];
        assert_eq!(render_items(&long, 4, &t).to_string(), "abc…");
    }
}
