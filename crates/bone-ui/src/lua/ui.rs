//! Reading `bone.ui`: the layout, regions, the statusline and divider,
//! the prompt box, popups' lines, the spinner and the title, and the
//! context each gets. Frontends lay the lines out and draw them.

use mlua::{Function, Table, Value};
use serde_json::{Value as Json, json};

use bone_lua::to_lua;

use super::{Host, guarded};
use crate::editor::TextBuffer;
use crate::model::{
    Item, LayoutNode, Mark, PromptMarks, PromptSpec, Size, Spinner, border_of, layout_children,
    line_of, parse_items, parse_lines,
};

pub fn session_ctx<H: Host>(h: &mut H, buf: usize) -> Json {
    let store = h.chat_store();
    let Some(c) = store.chats.get(buf) else {
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
    })
}

pub fn popup_name<H: Host>(h: &mut H) -> Json {
    match h.context() {
        crate::keymap::Context::Main => Json::Null,
        c => json!(c.name()),
    }
}

/// `bone.ui.spinner = { frames = { ... }, interval = ms }`, or the
/// default; read once per frame.
pub fn read_spinner<H: Host>(h: &mut H) -> Spinner {
    guarded(h, "spinner", |lua| {
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
pub fn ui_title<H: Host>(h: &mut H) -> Option<String> {
    if !ui_defined(h, "title") {
        return None;
    }
    let (w, cur) = (h.screen_cols(), h.chat_store().current);
    let ctx = statusline_ctx(h, w, cur);
    guarded(h, "title", |lua| {
        let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
        let f: Function = ui.get("title")?;
        f.call::<Option<String>>(to_lua(lua, &ctx)?)
    })
    .flatten()
    .map(|t| crate::text::sanitize(&t))
}

/// What chat `buf`'s statusline and divider get.
pub fn statusline_ctx<H: Host>(h: &mut H, width_cols: u16, buf: usize) -> Json {
    json!({
        "title": h.chat_store().chats[buf].title(),
        "popup": popup_name(h),
        "panel": h.focused_panel(),
        "jobs": h.jobs_running(),
        "spinner": h.spinner().frame(),
        "width": width_cols,
        "session": session_ctx(h, buf),
    })
}

/// `bone.ui.layout`: the rows of the screen, top to bottom. `chat`,
/// `prompt`, `divider` and `statusline` are built in; any other name is
/// a region from `bone.ui.regions`.
/// `bone.ui.layout` as a tree: the top level is a stack of rows.
pub fn layout<H: Host>(h: &mut H) -> LayoutNode {
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
    guarded(h, "layout", |lua| {
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
pub fn ui_defined<H: Host>(h: &H, name: &str) -> bool {
    let Some(lua) = h.lua() else { return false };
    let ui = || -> mlua::Result<bool> {
        let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
        Ok(matches!(ui.get::<Value>(name)?, Value::Function(_)))
    };
    ui().unwrap_or(false)
}

/// `bone.ui.prompt`, read with `ctx` (fields may be functions of it).
/// Nothing set means a bare prompt.
pub fn prompt_spec<H: Host>(h: &mut H, ctx: &Json) -> PromptSpec {
    guarded(h, "prompt", |lua| {
        let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
        let Some(p) = ui.get::<Option<Table>>("prompt")? else {
            return Ok(PromptSpec::default());
        };
        let line =
            |key: &str, hl: &str| -> mlua::Result<Vec<Item>> { line_of(lua, p.get(key)?, ctx, hl) };
        let border = border_of(p.get("border")?)?;
        let (pad_rows, pad_cols) = match p.get::<Value>("padding")? {
            Value::Nil => (0, 0),
            Value::Integer(n) => (0, n.clamp(0, u16::MAX as i64) as u16),
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
pub fn prompt_edge<H: Host>(h: &mut H, edge: &str, ctx: &Json, hl: &str) -> Vec<Item> {
    guarded(h, "prompt", |lua| {
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
pub fn prompt_highlight<H: Host>(h: &mut H, ctx: &Json) -> PromptMarks {
    guarded(h, "prompt_highlight", |lua| {
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

/// Chat `buf`'s prompt box, holding `text`.
pub fn prompt_ctx<H: Host>(h: &mut H, width_cols: u16, buf: usize, text: &TextBuffer) -> Json {
    let (row, col) = text.cursor();
    let selection = text.selection().map(
        |(a, b)| json!({ "start": { "row": a.0, "col": a.1 }, "end": { "row": b.0, "col": b.1 } }),
    );
    let session = session_ctx(h, buf);
    json!({
        "width": width_cols,
        "text": text.text(),
        "lines": text.lines().len(),
        "empty": text.is_empty(),
        "cursor": { "row": row, "col": col },
        "selection": selection,
        "focused": h.prompt_focused(buf),
        "running": session["running"].as_bool().unwrap_or(false),
        "elapsed": session["elapsed"],
        "spinner": h.spinner().frame(),
        "session": session,
    })
}

/// Call `bone.ui[name](ctx)` for a single row (statusline, divider).
pub fn ui_items<H: Host>(h: &mut H, name: &str, ctx: Json, default_hl: &str) -> Option<Vec<Item>> {
    let default_hl = default_hl.to_owned();
    guarded(h, name, |lua| {
        let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
        let Some(f) = ui.get::<Option<Function>>(name)? else {
            return Ok(None);
        };
        let v = f.call::<Value>(to_lua(lua, &ctx)?)?;
        parse_items(v, &default_hl).map(Some)
    })
    .flatten()
}

/// A region in a stack of rows (`rows`: sized in rows, up to `height`)
/// or a row of columns (sized in columns, up to half of `width_cols`).
/// `exact` draws it at exactly `width_cols` × `height`, whatever its size.
pub fn region_sized<H: Host>(
    h: &mut H,
    name: &str,
    width_cols: u16,
    height: u16,
    rows: bool,
    exact: bool,
) -> Option<(u16, Vec<Vec<Item>>)> {
    let shown = {
        let c = h.chat_store();
        c.rendering.unwrap_or(c.current)
    };
    let base = json!({
        "region": name,
        "spinner": h.spinner().frame(),
        "popup": popup_name(h),
        "session": session_ctx(h, shown),
    });
    let (size, lines) = guarded(h, &format!("regions.{name}"), |lua| {
        let regions: Table = lua
            .globals()
            .get::<Table>("bone")?
            .get::<Table>("ui")?
            .get("regions")?;
        let (render, size, max): (Function, Value, u16) = match regions.get::<Value>(name)? {
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
            Value::Integer(n) => Some(n.clamp(0, u16::MAX as i64) as u16),
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
    Some((size, lines))
}

/// A popup's lines: its function called with `{ width }`, or its list.
pub fn popup_lines<H: Host>(
    h: &mut H,
    id: u64,
    cb: u64,
    width_cols: u16,
    height: u16,
) -> Vec<Vec<Item>> {
    guarded(h, &format!("popup.{id}"), |lua| {
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
    .unwrap_or_default()
}
