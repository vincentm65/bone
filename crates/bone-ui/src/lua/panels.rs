//! Panels' ops and their per-frame work, shared by every frontend: opening
//! and updating them from Lua, focus, measuring, docking beside the chat,
//! and preparing what each shows (its title, its visible content). Frontends
//! paint the result.

use mlua::{Function, Lua, MultiValue, Table, Value};
use serde_json::{Value as Json, json};

use super::{
    CALLBACKS, Host, Owned, args, context_from_name, drop_callback, emit_focus_changed, err, fire,
    guarded, json as ret_json, own, ret, store_callback,
};
use crate::keymap::Context;
use crate::keys;
use crate::layout::Rect;
use crate::model::{Item, LayoutNode, items_width, parse_lines};
use crate::panel::{
    Dock, MIN_CHAT_COLS, MIN_CHAT_ROWS, Panel, PanelPlacement, Size, split, wanted,
};

/// The panels and which one has the keyboard.
pub struct PanelStore<'a> {
    pub panels: &'a mut Vec<Panel>,
    pub focus: &'a mut Option<String>,
}

pub(super) fn dispatch<H: Host>(
    h: &mut H,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    Ok(match op {
        "panel_open" => panel_open(h, lua, a),
        "panel_update" => panel_update(h, lua, a),
        "panel_set_lines" => panel_set_lines(h, lua, a),
        "panel_close" => args::<String>(lua, a)
            .and_then(|id| close_panel(h, lua, &id))
            .and_then(|closed| ret(lua, closed)),
        "panel_info" => panel_info_op(h, lua, a),
        "panel_list" => {
            let list: Vec<Json> = (in_order(h.panel_store().panels).into_iter().cloned())
                .collect::<Vec<_>>()
                .iter()
                .map(|p| panel_info(h, p))
                .collect();
            ret_json(lua, &Json::Array(list))
        }
        "panel_focus" => args::<Option<String>>(lua, a)
            .and_then(|id| focus_panel(h, id.as_deref()).map_err(err))
            .and_then(|()| ret(lua, ())),
        "panel_scroll" => panel_scroll(h, lua, a),
        _ => return Err(a),
    })
}

pub fn panel<'a>(panels: &'a [Panel], id: &str) -> Option<&'a Panel> {
    panels.iter().find(|p| p.id == id)
}

/// Panels in placement order: left, right, top, bottom; then `order`, then
/// age.
pub fn in_order(panels: &[Panel]) -> Vec<&Panel> {
    let mut v: Vec<&Panel> = panels.iter().collect();
    v.sort_by_key(|p| (p.dock.rank(), p.order, p.seq));
    v
}

/// The panel with the keyboard, if any (a focused window still comes first).
pub fn focused<'a>(panels: &'a [Panel], focus: &Option<String>) -> Option<&'a Panel> {
    let id = focus.as_deref()?;
    panel(panels, id).filter(|p| !p.hidden && p.focusable)
}

/// The panel drawn at cell `(x, y)`.
pub fn at(panels: &[Panel], x: u16, y: u16) -> Option<&Panel> {
    panels
        .iter()
        .find(|p| p.area.is_some_and(|a| a.contains(x, y)))
}

/// The panel at a cell and the line of its content there (from 1; 0 on its
/// title row).
pub fn hit(panels: &[Panel], x: u16, y: u16) -> Option<(String, usize)> {
    let p = at(panels, x, y)?;
    let area = p.area?;
    let row = (y - area.y) as usize;
    let title = usize::from(p.title.is_some());
    let line = if row < title {
        0
    } else {
        p.top + row - title + 1
    };
    Some((p.id.clone(), line))
}

/// The panel as Lua sees it (`info`, and the `panel/*` events).
pub fn panel_info<H: Host>(h: &mut H, p: &Panel) -> Json {
    let s = h.panel_store();
    p.info(s.focus.as_deref() == Some(p.id.as_str()))
}

/// The Lua handle of panel `id` (`bone.ui.panel._handle`), given to its
/// callbacks.
pub fn panel_handle(lua: &Lua, id: &str) -> mlua::Result<Value> {
    let panel: Table = lua
        .globals()
        .get::<Table>("bone")?
        .get::<Table>("ui")?
        .get("panel")?;
    match panel.get::<Option<Function>>("_handle")? {
        Some(f) => f.call(id),
        None => Ok(Value::String(lua.create_string(id)?)),
    }
}

/// Give a panel the keyboard, or (`None`) give it back to the prompt.
pub fn focus_panel<H: Host>(h: &mut H, id: Option<&str>) -> Result<(), String> {
    let s = h.panel_store();
    if let Some(id) = id {
        let p = panel(s.panels, id).ok_or_else(|| format!("no panel {id}"))?;
        if p.hidden {
            return Err(format!("panel {id} is hidden"));
        }
        if !p.focusable {
            return Err(format!("panel {id} is not focusable"));
        }
    }
    let before = focused(s.panels, s.focus).map(|p| p.id.clone());
    let after = id.map(str::to_owned);
    if before == after && *s.focus == after {
        return Ok(());
    }
    h.flush_keys();
    *h.panel_store().focus = after;
    emit_focus_changed(h);
    h.changed();
    Ok(())
}

/// Drop the focus if its panel is gone, hidden or no longer focusable.
pub fn check_panel_focus<H: Host>(h: &mut H) {
    let s = h.panel_store();
    if s.focus.is_some() && focused(s.panels, s.focus).is_none() {
        *s.focus = None;
        emit_focus_changed(h);
    }
}

fn valid_panel_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn apply_panel_spec<H: Host>(
    h: &mut H,
    lua: &Lua,
    p: &mut Panel,
    spec: &Table,
) -> mlua::Result<()> {
    let content = match (spec.get::<Value>("render")?, spec.get::<Value>("lines")?) {
        (Value::Nil, Value::Nil) => None,
        (f @ Value::Function(_), Value::Nil) => Some(f),
        (Value::Nil, v @ (Value::Function(_) | Value::Table(_))) => Some(v),
        (Value::Nil, _) => return Err(err("panel lines must be a list or a function")),
        (_, Value::Nil) => return Err(err("panel render must be a function")),
        _ => return Err(err("give a panel render or lines, not both")),
    };
    if let Some(v) = content {
        if p.content != 0 {
            drop_callback(lua, p.content)?;
        }
        let next = h.owners().next_callback;
        *next += 1;
        p.content = *next;
        lua.named_registry_value::<Table>(CALLBACKS)?
            .set(p.content, v)?;
        h.ui_broken().remove(&format!("panel.{}", p.id));
    }
    if let Some(dock) = spec.get::<Option<String>>("dock")? {
        p.dock = Dock::parse(&dock).ok_or_else(|| {
            err(format!(
                "unknown dock {dock:?} (left, right, top or bottom)"
            ))
        })?;
    }
    if let Some(full_height) = spec.get::<Option<bool>>("full_height")? {
        p.full_height = full_height;
    }
    if let Some(separator) = spec.get::<Option<bool>>("separator")? {
        p.separator = separator;
    }
    match spec.get::<Value>("size")? {
        Value::Nil => {}
        Value::Boolean(false) => p.size = None,
        Value::String(s) if s.to_str()?.as_ref() == "auto" => p.size = Some(Size::Auto),
        Value::Integer(n) if n >= 1 => p.size = Some(Size::Cells(n.min(u16::MAX as i64) as u16)),
        Value::Number(f) if f > 0.0 && f < 1.0 => p.size = Some(Size::Fraction(f)),
        Value::Number(f) if f >= 1.0 => p.size = Some(Size::Cells(f.min(u16::MAX as f64) as u16)),
        _ => {
            return Err(err(
                "panel size is a number of cells, a fraction between 0 and 1, \"auto\" or false",
            ));
        }
    }
    match spec.get::<Value>("max")? {
        Value::Nil => {}
        Value::Boolean(false) => p.max = None,
        Value::Integer(n) if n >= 1 => p.max = Some(n.min(u16::MAX as i64) as u16),
        _ => return Err(err("panel max must be a positive integer or false")),
    }
    if let Some(min) = spec.get::<Option<u16>>("min")? {
        p.min = min;
    }
    if let Some(order) = spec.get::<Option<i32>>("order")? {
        p.order = order;
    }
    match spec.get::<Value>("title")? {
        Value::Nil => {}
        Value::Boolean(false) => p.title = None,
        Value::String(t) => p.title = Some(crate::text::sanitize(&t.to_str()?).replace('\n', " ")),
        _ => return Err(err("panel title must be a string or false")),
    }
    if let Some(t) = spec.get::<Option<Table>>("keys")? {
        let mut keys = Vec::new();
        for pair in t.pairs::<String, Function>() {
            let (name, f) = pair?;
            keys.push((keys::parse(&name).map_err(err)?, f));
        }
        for (_, cb) in p.keys.drain(..) {
            drop_callback(lua, cb)?;
        }
        for (key, f) in keys {
            p.keys.push((key, store_callback(h, lua, f)?));
        }
    }
    for (field, slot) in [("on_key", &mut p.on_key), ("on_close", &mut p.on_close)] {
        if let Some(f) = spec.get::<Option<Function>>(field)? {
            if let Some(cb) = slot.take() {
                drop_callback(lua, cb)?;
            }
            *slot = Some(store_callback(h, lua, f)?);
        }
    }
    match spec.get::<Value>("context")? {
        Value::Nil => {}
        Value::Boolean(false) => p.context = None,
        Value::String(name) => {
            let ctx = context_from_name(name.to_str()?.to_owned())?;
            if matches!(ctx, Context::Main | Context::Popup) {
                return Err(err(format!(
                    "a panel's context cannot be {} (use panel or a named context)",
                    ctx.name()
                )));
            }
            p.context = Some(ctx);
        }
        _ => return Err(err("panel context must be a name or false")),
    }
    if let Some(v) = spec.get::<Option<bool>>("focusable")? {
        p.focusable = v;
    }
    if let Some(v) = spec.get::<Option<bool>>("hidden")? {
        p.hidden = v;
    }
    if let Some(v) = spec.get::<Option<bool>>("follow")? {
        p.follow = v;
        p.at_end = v;
    }
    h.changed();
    Ok(())
}

fn panel_open<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let spec: Table = args(lua, a)?;
    let next = h.owners().next_callback;
    *next += 1;
    let seq = *next;
    let panels = &*h.panel_store().panels;
    let id = match spec.get::<Option<String>>("id")? {
        Some(id) if !valid_panel_id(&id) => {
            return Err(err(format!(
                "invalid panel id {id:?} (letters, digits, _, - and .)"
            )));
        }
        Some(id) if panel(panels, &id).is_some() => {
            return Err(err(format!("panel {id} is already open")));
        }
        Some(id) => id,
        None => {
            let mut n = seq;
            while panel(panels, &format!("panel{n}")).is_some() {
                n += 1;
            }
            format!("panel{n}")
        }
    };
    if matches!(
        (spec.get::<Value>("render")?, spec.get::<Value>("lines")?),
        (Value::Nil, Value::Nil)
    ) {
        return Err(err("a panel needs render (a function) or lines"));
    }
    let focus = spec.get::<Option<bool>>("focus")? == Some(true);
    let mut p = Panel::new(id.clone(), seq);
    let applied = apply_panel_spec(h, lua, &mut p, &spec).and_then(|()| {
        if focus && (p.hidden || !p.focusable) {
            return Err(err("a hidden or unfocusable panel cannot open focused"));
        }
        Ok(())
    });
    if let Err(e) = applied {
        // Release whatever was stored before the bad field.
        for cb in [p.content]
            .into_iter()
            .chain(p.keys.iter().map(|(_, cb)| *cb))
            .chain(p.on_key)
            .chain(p.on_close)
            .filter(|cb| *cb != 0)
        {
            drop_callback(lua, cb)?;
        }
        return Err(e);
    }
    let event = panel_info(h, &p);
    own(h, Owned::Panel(id.clone(), seq));
    h.panel_store().panels.push(p);
    fire(h, "panel/opened", event);
    if focus {
        // An opened callback may have closed or hidden it again.
        let _ = focus_panel(h, Some(&id));
    }
    ret(lua, id)
}

fn panel_update<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (id, spec): (String, Table) = args(lua, a)?;
    let Some(i) = h.panel_store().panels.iter().position(|p| p.id == id) else {
        return ret(lua, false);
    };
    let mut p = h.panel_store().panels.remove(i);
    let applied = apply_panel_spec(h, lua, &mut p, &spec);
    h.panel_store().panels.insert(i, p);
    applied?;
    check_panel_focus(h);
    match spec.get::<Option<bool>>("focus")? {
        Some(true) => focus_panel(h, Some(&id)).map_err(err)?,
        Some(false) if h.focused_panel().as_deref() == Some(id.as_str()) => {
            focus_panel(h, None).map_err(err)?
        }
        _ => {}
    }
    if let Some(p) = panel(h.panel_store().panels, &id).cloned() {
        let event = panel_info(h, &p);
        fire(h, "panel/updated", event);
    }
    ret(lua, true)
}

fn panel_set_lines<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (id, lines): (String, Value) = args(lua, a)?;
    if !matches!(lines, Value::Table(_) | Value::Function(_)) {
        return Err(err("panel lines must be a list or a function"));
    }
    let Some(content) = panel(h.panel_store().panels, &id).map(|p| p.content) else {
        return ret(lua, false);
    };
    lua.named_registry_value::<Table>(CALLBACKS)?
        .set(content, lines)?;
    h.ui_broken().remove(&format!("panel.{id}"));
    h.changed();
    ret(lua, true)
}

/// Close panel `id`, releasing its callbacks after its `on_close` ran.
pub fn close_panel<H: Host>(h: &mut H, lua: &Lua, id: &str) -> mlua::Result<bool> {
    let Some(i) = h.panel_store().panels.iter().position(|p| p.id == id) else {
        return Ok(false);
    };
    let p = h.panel_store().panels[i].clone();
    let event = panel_info(h, &p);
    let p = h.panel_store().panels.remove(i);
    drop_callback(lua, p.content)?;
    for (_, cb) in p.keys {
        drop_callback(lua, cb)?;
    }
    if let Some(cb) = p.on_key {
        drop_callback(lua, cb)?;
    }
    check_panel_focus(h);
    if let Some(cb) = p.on_close {
        let id = id.to_owned();
        h.call_callback(cb, "panel on_close", |lua| panel_handle(lua, &id));
        drop_callback(lua, cb)?;
    }
    fire(h, "panel/closed", event);
    h.changed();
    Ok(true)
}

fn panel_info_op<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let id: String = args(lua, a)?;
    match panel(h.panel_store().panels, &id).cloned() {
        Some(p) => {
            let info = panel_info(h, &p);
            ret_json(lua, &info)
        }
        None => ret(lua, Value::Nil),
    }
}

fn panel_scroll<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (id, to): (String, Value) = args(lua, a)?;
    let Some(p) = h.panel_store().panels.iter_mut().find(|p| p.id == id) else {
        return ret(lua, false);
    };
    match to {
        Value::Integer(n) => p.scroll_by(n),
        Value::Number(n) => p.scroll_by(n as i64),
        Value::String(s) if s.to_str()?.as_ref() == "top" => p.scroll_to(false),
        Value::String(s) if s.to_str()?.as_ref() == "bottom" => p.scroll_to(true),
        _ => {
            return Err(err(
                "scroll by a number of rows, or to \"top\" or \"bottom\"",
            ));
        }
    }
    h.changed();
    ret(lua, true)
}

// ---- each frame ------------------------------------------------------------

fn panel_ctx<H: Host>(h: &mut H, p: &Panel, width: u16, height: u16) -> Json {
    let focused = h.focused_panel().as_deref() == Some(p.id.as_str());
    json!({ "id": p.id, "spinner": h.spinner().frame(), "dock": p.dock.name(),
        "width": width, "height": height, "focused": focused,
        "top": p.top, "title": p.title })
}

/// A panel's content: its function called with `ctx`, or its list.
fn panel_content<H: Host>(h: &mut H, id: &str, content: u64, ctx: Json) -> Vec<Vec<Item>> {
    guarded(h, &format!("panel.{id}"), |lua| {
        let v: Value = lua.named_registry_value::<Table>(CALLBACKS)?.get(content)?;
        let v = match v {
            Value::Function(f) => f.call::<Value>(bone_lua::to_lua(lua, &ctx)?)?,
            v => v,
        };
        parse_lines(v)
    })
    .flatten()
    .unwrap_or_default()
}

/// Measure a panel: rows of its content (in a stack) or its widest line (in
/// a row), up to its max. Painting calls the content again with the final
/// body size; measuring is one pass.
pub fn measure_panel<H: Host>(h: &mut H, id: &str, width: u16, height: u16, rows: bool) -> u16 {
    let Some(p) = panel(h.panel_store().panels, id)
        .cloned()
        .filter(|p| !p.hidden)
    else {
        return 0;
    };
    let title = u16::from(p.title.is_some());
    let ctx = panel_ctx(h, &p, width, height.saturating_sub(title));
    let lines = panel_content(h, id, p.content, ctx);
    if rows {
        (lines.len().min(u16::MAX as usize) as u16)
            .saturating_add(title)
            .min(p.max())
    } else {
        lines
            .iter()
            .map(|l| items_width(l))
            .max()
            .unwrap_or(0)
            .min(p.max() as usize) as u16
    }
}

/// Dock the panels the layout does not place itself into `room`: screen-high
/// side panels when `full_height`, the rest beside or above and below the
/// chat. Returns what is left of `room`.
pub fn allocate_panels<H: Host>(
    h: &mut H,
    room: Rect,
    full_height: bool,
    layout: &LayoutNode,
    placements: &mut Vec<PanelPlacement>,
) -> Rect {
    let order: Vec<String> = (in_order(h.panel_store().panels).into_iter())
        .map(|p| p.id.clone())
        .collect();
    let mut room = room;
    for id in order {
        let Some(p) = panel(h.panel_store().panels, &id).cloned() else {
            continue;
        };
        if p.hidden
            || layout.contains(&format!("panel:{id}"))
            || (p.full_height && p.dock.sideways()) != full_height
        {
            continue;
        }
        let side = p.dock.sideways();
        let room_len = if side { room.width } else { room.height };
        let want = wanted(p.size(), p.max(), room_len, || {
            let w = if side {
                p.max().min(room.width)
            } else {
                room.width
            };
            let height = if side {
                room.height
            } else {
                p.max().min(room.height)
            };
            measure_panel(h, &id, w, height, !side)
        });
        let keep = if side { MIN_CHAT_COLS } else { MIN_CHAT_ROWS };
        let Some((area, rest)) = split(room, p.dock, want, p.min.max(1), keep) else {
            continue;
        };
        room = rest;
        let separator = if side && p.separator {
            Some(if p.dock == Dock::Left {
                area.right()
            } else {
                area.x - 1
            })
        } else {
            None
        };
        placements.push(PanelPlacement {
            id,
            area,
            separator,
        });
    }
    room
}

/// Record where every panel went this frame, before any content runs.
pub fn publish_panels<H: Host>(h: &mut H, placements: &[PanelPlacement]) {
    let panels = &mut *h.panel_store().panels;
    for p in panels.iter_mut() {
        p.area = None;
        p.view = 0;
    }
    for placement in placements {
        if let Some(p) = panels.iter_mut().find(|p| p.id == placement.id) {
            p.area = Some(placement.area);
            p.view = placement
                .area
                .height
                .saturating_sub(u16::from(p.title.is_some()));
        }
    }
}

/// What a placed panel shows this frame.
pub struct PanelFrame {
    pub area: Rect,
    /// Its title row (text, then a fill), in the focused or plain title
    /// group.
    pub title: Option<Vec<Item>>,
    /// Below the title.
    pub body: Rect,
    /// The content rows in view.
    pub lines: Vec<Vec<Item>>,
    /// The column of its separator bar, for a side panel that has one.
    pub separator: Option<u16>,
}

/// Fetch a placed panel's content at its final size and scroll it (clamped,
/// or kept at the end while it follows); `None` if it is hidden or gone.
pub fn panel_frame<H: Host>(h: &mut H, placement: &PanelPlacement) -> Option<PanelFrame> {
    let p = panel(h.panel_store().panels, &placement.id)
        .cloned()
        .filter(|p| !p.hidden)?;
    let rect = placement.area;
    let title_h = u16::from(p.title.is_some()).min(rect.height);
    let body = Rect {
        y: rect.y + title_h,
        height: rect.height - title_h,
        ..rect
    };
    let focused = h.focused_panel().as_deref() == Some(p.id.as_str());
    let ctx = panel_ctx(h, &p, body.width, body.height);
    let lines = panel_content(h, &p.id, p.content, ctx);
    let p = h
        .panel_store()
        .panels
        .iter_mut()
        .find(|q| q.id == placement.id)?;
    let view = body.height as usize;
    let max_top = lines.len().saturating_sub(view);
    if p.follow && p.at_end {
        p.top = max_top;
    }
    p.top = p.top.min(max_top);
    p.rows = lines.len();
    let title = p.title.clone().filter(|_| title_h > 0).map(|t| {
        let hl = if focused {
            "PanelTitleFocus"
        } else {
            "PanelTitle"
        };
        vec![Item::Text(t, hl.into()), Item::Fill(" ".into(), hl.into())]
    });
    let lines = lines.into_iter().skip(p.top).take(view).collect();
    Some(PanelFrame {
        area: rect,
        title,
        body,
        lines,
        separator: placement.separator,
    })
}
