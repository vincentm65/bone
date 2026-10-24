//! `bone.gui` ops: the view, scrolling, focus, fonts, text inputs and status.

use bone_ui::lua::{Host, args, drop_callback, err, ret, store_callback};
use mlua::{Function, Lua, MultiValue, Value};

use crate::Gui;

use super::{Target, remember_text};

/// `bone.ui.popup` / `win` / `update` and `bone.ui.panel.open` / `update`:
/// a spec's `node` (a function of `{ width, height }` returning the content
/// as a node) is kept here, by the window's or panel's id, and the rest of
/// the spec goes to the shared op. Content of nodes alone needs no lines.
pub(crate) fn window_spec(
    gui: &mut Gui,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> mlua::Result<MultiValue> {
    let update = op.ends_with("_update");
    let spec = match a.get(usize::from(update)) {
        Some(Value::Table(t)) => Some(t.clone()),
        _ => None,
    };
    let mut node = None;
    let mut dispatch_args = a.clone();
    if let Some(t) = &spec {
        node = t.get::<Option<Function>>("node")?;
        if node.is_some() {
            // Keep the caller's table intact.  `bone.ui.update(id, spec)` is
            // allowed to reuse a table and replace its node callback.
            let copy = lua.create_table()?;
            for pair in t.pairs::<Value, Value>() {
                let (key, value) = pair?;
                copy.set(key, value)?;
            }
            copy.set("node", Value::Nil)?;
            let (lines, render) = (t.get::<Value>("lines")?, t.get::<Value>("render")?);
            if !update && lines.is_nil() && render.is_nil() {
                copy.set("lines", lua.create_table()?)?;
            }
            let index = usize::from(update);
            dispatch_args[index] = Value::Table(copy);
        }
    }
    let r = match bone_ui::lua::dispatch(gui, lua, op, dispatch_args) {
        Ok(r) => r?,
        Err(_) => return Err(err(format!("{op}: not available"))),
    };
    let Some(f) = node else {
        return Ok(r);
    };
    let id = match (update, r.front()) {
        (true, Some(Value::Boolean(true))) => a.front().cloned(),
        (false, Some(v)) => Some(v.clone()),
        _ => None,
    };
    let id = match id {
        Some(Value::Integer(n)) => n.to_string(),
        Some(Value::Number(n)) => (n as u64).to_string(),
        Some(Value::String(s)) => s.to_str()?.to_owned(),
        _ => return Ok(r),
    };
    let kind = if op.starts_with("popup") {
        "popup"
    } else {
        "panel"
    };
    let cb = store_callback(gui, lua, f)?;
    if let Some(old) = gui
        .view
        .bound
        .window_nodes
        .insert(format!("{kind}:{id}"), cb)
    {
        drop_callback(lua, old)?;
    }
    Host::changed(gui);
    Ok(r)
}

/// These ops; `None` for an op that is not one of them.
pub(crate) fn dispatch(
    gui: &mut Gui,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    Ok(match op {
        "gui_view" => args::<Option<Function>>(lua, a).and_then(|f| {
            if let Some(old) = gui.view.bound.func.take() {
                drop_callback(lua, old)?;
            }
            if let Some(f) = f {
                gui.view.bound.func = Some(store_callback(gui, lua, f)?);
            }
            gui.view.bound.error = None;
            Host::changed(gui);
            ret(lua, ())
        }),
        // gui_scroll(id, to): pixels, "top" or "end".
        "gui_scroll" => args::<(String, Value)>(lua, a).and_then(|(id, to)| {
            let target = match &to {
                Value::Integer(n) => Target::To(*n as f32),
                Value::Number(n) => Target::To(*n as f32),
                Value::String(s) if &*s.to_str()? == "top" => Target::To(0.0),
                Value::String(s) if &*s.to_str()? == "end" => Target::End,
                _ => return Err(err("scroll: expected pixels, \"top\" or \"end\"")),
            };
            gui.view.scroll.entry(id).or_default().target = Some(target);
            Host::changed(gui);
            ret(lua, ())
        }),
        // gui_scroll_info(id) -> { offset, max, viewport } or nil.
        "gui_scroll_info" => args::<String>(lua, a).and_then(|id| match gui.view.scroll.get(&id) {
            Some(s) => bone_ui::lua::json(
                lua,
                &serde_json::json!({ "offset": s.offset, "max": s.max, "viewport": s.viewport }),
            ),
            None => ret(lua, Value::Nil),
        }),
        // gui_scroll_item(id, index, at): scroll list `id` to item `index`
        // (from 1): "top" (default), "center", "bottom" or "nearest" (only
        // as far as needed to show it). False until the list has been drawn.
        "gui_scroll_item" => {
            args::<(String, usize, Option<String>)>(lua, a).and_then(|(id, index, at)| {
                let Some((start, len, viewport)) = gui.list_item_span(&id, index) else {
                    return ret(lua, false);
                };
                let offset = gui.view.scroll.get(&id).map_or(0.0, |s| s.offset);
                let to = match at.as_deref().unwrap_or("top") {
                    "bottom" => start + len - viewport,
                    "center" => start + len / 2.0 - viewport / 2.0,
                    "nearest" if start < offset => start,
                    "nearest" if start + len > offset + viewport => start + len - viewport,
                    "nearest" => offset,
                    _ => start,
                };
                gui.view.scroll.entry(id).or_default().target = Some(Target::To(to.max(0.0)));
                gui.view.invalidate();
                ret(lua, true)
            })
        }
        // gui_popups() -> the windows of bone.ui.popup / win, placed: each
        // { id, x, y, w, h (pixels), z, focus, lines = { { { text, hl } } } },
        // with `cell = { w, h }`, the monospace cell they are laid out in.
        "gui_popups" => gui
            .popups_for_lua(lua)
            .map(|t| MultiValue::from_vec(vec![Value::Table(t)])),
        // gui_panel_node(id) -> the function panel `id` draws its content
        // with as nodes, or nil.
        "gui_panel_node" => args::<String>(lua, a).and_then(|id| {
            gui.forget_window_nodes(lua)?;
            match gui.view.bound.window_nodes.get(&format!("panel:{id}")) {
                Some(cb) => ret(lua, gui.stored_function(lua, *cb)?),
                None => ret(lua, Value::Nil),
            }
        }),
        // gui_font(name, path): register a font family for `font = name`.
        "gui_font" => args::<(String, String)>(lua, a).and_then(|(name, path)| {
            if ["sans", "mono"].contains(&name.as_str()) {
                return Err(err("font: sans and mono are the built-in families"));
            }
            let fonts = &mut gui.view.bound.extra_fonts;
            fonts.retain(|(n, _)| *n != name);
            fonts.push((name, path));
            gui.view.invalidate();
            ret(lua, ())
        }),
        // gui_input_text(id) -> the text of text input `id`, or nil.
        "gui_input_text" => args::<String>(lua, a).and_then(|id| match gui.view.inputs.get(&id) {
            Some(b) => ret(lua, b.text.clone()),
            None => ret(lua, Value::Nil),
        }),
        // gui_input_set(id, text): replace the text of text input `id`.
        "gui_input_set" => args::<(String, String)>(lua, a).and_then(|(id, text)| {
            let buf = gui.view.inputs.entry(id).or_default();
            buf.pushed = Some(text.clone());
            remember_text(buf, text);
            gui.view.invalidate();
            ret(lua, ())
        }),
        // gui_focus(id): give node or text input `id` the keyboard; nil
        // gives it back to the prompt.
        "gui_focus" => args::<Option<String>>(lua, a).and_then(|id| {
            let vs = &mut gui.view;
            match id {
                Some(id) => vs.focus_request = Some(id),
                None => {
                    (vs.focused, vs.focus_request) = (None, None);
                    // From a text input too.
                    if let Some(ctx) = gui.repaint.get() {
                        ctx.memory_mut(|m| m.request_focus(crate::prompt::edit_id()));
                    }
                }
            }
            gui.view.invalidate();
            Host::changed(gui);
            ret(lua, ())
        }),
        // gui_focused() -> the id of the text input or node with the
        // keyboard, or nil.
        "gui_focused" => {
            let vs = &gui.view;
            match (&vs.input_focus, vs.focused_node()) {
                (Some(id), _) => ret(lua, id.clone()),
                (None, Some(f)) if !f.key.starts_with('@') => ret(lua, f.key.clone()),
                _ => ret(lua, Value::Nil),
            }
        }
        // gui_status() -> { text, error } or nil: the message Lua or the
        // GUI last showed; gui_status_clear() takes it away.
        "gui_status" => match &gui.status {
            Some(s) => bone_ui::lua::json(
                lua,
                &serde_json::json!({
                    "text": s.text,
                    "error": s.error,
                    "age_ms": s.at.elapsed().as_millis() as u64,
                }),
            ),
            None => ret(lua, Value::Nil),
        },
        "gui_status_clear" => {
            gui.status = None;
            gui.view.invalidate();
            ret(lua, ())
        }
        // gui_fallback(fn|nil): the view drawn when the view fails or none is
        // set; its ctx has `error`.
        "gui_fallback" => args::<Option<Function>>(lua, a).and_then(|f| {
            if let Some(old) = gui.view.bound.fallback.take() {
                drop_callback(lua, old)?;
            }
            if let Some(f) = f {
                gui.view.bound.fallback = Some(store_callback(gui, lua, f)?);
            }
            Host::changed(gui);
            ret(lua, ())
        }),
        // gui_invalidate(ms?): build the tree again now, or in `ms`.
        "gui_invalidate" => args::<Option<f64>>(lua, a).and_then(|ms| {
            match ms {
                Some(ms) if ms > 0.0 => {
                    let at =
                        std::time::Instant::now() + std::time::Duration::from_secs_f64(ms / 1000.0);
                    let vs = &mut gui.view;
                    vs.wake = Some(vs.wake.map_or(at, |w| w.min(at)));
                    // A later build, not this one: `changed` would dirty now.
                    crate::wake(&gui.repaint);
                }
                _ => Host::changed(gui),
            }
            ret(lua, ())
        }),
        _ => return Err(a),
    })
}
