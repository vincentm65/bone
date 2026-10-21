//! The Lua UI model: what `bone.ui` describes, parsed from Lua values —
//! lines of styled items, the layout tree, the prompt box and its marks, and
//! the spinner. Frontends draw these; nothing here draws.

use mlua::{Table, Value};

use crate::text::{sanitize, width};

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
pub fn border_of(v: Value) -> mlua::Result<Option<Border>> {
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
    pub fn leaf(name: &str, size: Option<Size>) -> Self {
        let default = if ["chat", "panel"].contains(&name.split(':').next().unwrap_or(name)) {
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
        Value::Integer(n) => Some(Size::Cells(n.clamp(0, u16::MAX as i64) as u16)),
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

pub fn layout_children(t: &Table) -> mlua::Result<Vec<LayoutNode>> {
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
            if let Some(id) = t.get::<Option<String>>("panel")? {
                return Ok(LayoutNode::leaf(&format!("panel:{id}"), size));
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
pub fn parse_lines(v: Value) -> mlua::Result<Option<Vec<Vec<Item>>>> {
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
