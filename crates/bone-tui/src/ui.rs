//! Where Lua draws the screen:
//!
//! - `bone.ui.views[kind](item, ctx)` renders each transcript item (user,
//!   reasoning, assistant, tool, notice) to lines.
//! - `bone.ui.regions[name]` fills `top`, `left`, `right` and `above_prompt`.
//! - `bone.ui.statusline(ctx)` and `bone.ui.divider(ctx)` draw those rows;
//!   without them the rows are not there.
//! - `bone.ui.prompt` sets the prompt's prefix and placeholder.
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

pub fn spinner() -> char {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    SPINNER[(ms / 100) as usize % SPINNER.len()]
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

/// A list of lines, as views and regions return.
fn parse_lines(v: Value) -> mlua::Result<Option<Vec<Vec<Item>>>> {
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

    pub fn statusline_ctx(&self, width_cols: u16) -> Json {
        json!({
            "title": self.chats[self.current].title(),
            "popup": self.popup_name(),
            "spinner": spinner().to_string(),
            "width": width_cols,
            "session": self.session_ctx(Some(self.current)),
        })
    }

    /// The line between the chat and the prompt.
    pub fn divider_ctx(&self, width_cols: u16) -> Json {
        json!({
            "spinner": spinner().to_string(),
            "width": width_cols,
            "session": self.session_ctx(Some(self.current)),
        })
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

    /// `bone.ui.prompt = { prefix = line, placeholder = line }`: what goes
    /// before the prompt text and what shows while it is empty. Both default
    /// to nothing.
    pub fn prompt_decor(&mut self) -> (Vec<Item>, Vec<Item>) {
        self.guarded("prompt", |lua| {
            let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
            let Some(p) = ui.get::<Option<Table>>("prompt")? else {
                return Ok((Vec::new(), Vec::new()));
            };
            Ok((
                parse_items(p.get("prefix")?, "UserPrompt")?,
                parse_items(p.get("placeholder")?, "Placeholder")?,
            ))
        })
        .unwrap_or_default()
    }

    /// Run Lua that may fail; a failure is reported once and `name` is
    /// skipped until the UI is refreshed (`bone.ui.refresh`, or redefining it).
    fn guarded<R>(
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
        let stale = self.chats[buf].stale(width_cols, generation);
        for (item, index, key) in stale {
            let lines = self.render_item(buf, item, index, &key);
            self.chats[buf].store(item, key, lines);
        }
    }

    fn render_item(
        &mut self,
        buf: BufferId,
        item: ChatItem,
        index: usize,
        key: &RenderKey,
    ) -> Vec<Line<'static>> {
        let data = self.chats[buf].item_data(item, index);
        let kind = item.part.name();
        let ctx = json!({
            "width": key.width,
            "region": "chat",
            "prev": key.prev.map(|p| json!({ "kind": p.name() })),
        });
        let view_data = data.clone();
        let lines = self
            .guarded(&format!("views.{kind}"), |lua| {
                let views: Table = lua
                    .globals()
                    .get::<Table>("bone")?
                    .get::<Table>("ui")?
                    .get("views")?;
                let Some(f) = views.get::<Option<Function>>(kind)? else {
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

    /// The size `bone.ui.regions[name]` asks for and its lines. Row regions
    /// (`top`, `above_prompt`) size in rows, `size = "auto"` fitting the
    /// content up to `max`; column regions (`left`, `right`) in columns.
    pub fn region(
        &mut self,
        name: &str,
        width_cols: u16,
        height: u16,
    ) -> Option<(u16, Vec<Line<'static>>)> {
        let rows = matches!(name, "top" | "above_prompt");
        let base = json!({
            "region": name,
            "spinner": spinner().to_string(),
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

    /// Items as data, for `bone.chat.items`. `kind` filters; `last` keeps
    /// the last N.
    pub fn chat_items(&self, kind: Option<&str>, last: Option<usize>) -> Vec<Json> {
        let chat = &self.chats[self.current];
        let picked: Vec<(usize, ChatItem)> = chat
            .items()
            .into_iter()
            .enumerate()
            .filter(|(_, i)| kind.is_none_or(|k| i.part.name() == k))
            .collect();
        let skip = picked.len().saturating_sub(last.unwrap_or(usize::MAX));
        picked
            .into_iter()
            .skip(skip)
            .map(|(n, i)| chat.item_data(i, n + 1))
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
