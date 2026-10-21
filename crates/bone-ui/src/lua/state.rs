//! A frontend's Lua state, and lending the frontend to it for a call.

use std::path::Path;

use bone_lua::{Side, to_lua};
use mlua::{Lua, MultiValue, Table, Value};

use super::{CALLBACKS, opt};

/// The registry slot `bone._api` forwards to while a frontend is lent.
const DISPATCH: &str = "bone.dispatch";

/// A frontend's Lua state: `bone_lua`'s for `side`, with the callback
/// registry, `bone._api` (forwarding to whatever [`lend`] has bound), the
/// pure text and markdown helpers, and the docs for `/help`.
pub fn new_state(side: Side, config_dir: Option<&Path>) -> mlua::Result<Lua> {
    let lua = bone_lua::new_state(side, config_dir)?;
    lua.set_named_registry_value(CALLBACKS, lua.create_table()?)?;
    let forward = lua.create_function(|lua, args: MultiValue| {
        let dispatch: Value = lua.named_registry_value(DISPATCH)?;
        match dispatch {
            Value::Function(f) => f.call::<MultiValue>(args),
            _ => Err(mlua::Error::runtime(
                "the bone API is only usable from Lua run by bone",
            )),
        }
    })?;
    let bone: Table = lua.globals().get("bone")?;
    bone.set("_api", forward)?;
    bone.set(
        "now",
        lua.create_function(|_, ()| {
            Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64() * 1000.0))
        })?,
    )?;
    bone.set(
        "strwidth",
        lua.create_function(|_, s: String| Ok(crate::text::width(&s)))?,
    )?;
    install_text(&lua, &bone)?;
    // The docs, for /help.
    let docs = lua.create_table()?;
    for (name, text) in DOCS.iter().chain(
        bone_lua::DOCS
            .iter()
            .filter(|(name, _)| name.starts_with("customizing")),
    ) {
        let d = lua.create_table()?;
        d.set("name", name.trim_end_matches(".md"))?;
        d.set("text", *text)?;
        docs.push(d)?;
    }
    bone.set("_docs", docs)?;
    Ok(lua)
}

/// Run `body` with the API bound to `h`: `bone._api` calls reach
/// `dispatch(h, ...)` for its duration, and then the binding of an outer
/// call (or none) is back. A nested call binds afresh, so Lua that an op
/// runs can call the API again.
pub fn lend<H, R>(
    h: &mut H,
    lua: &Lua,
    mut dispatch: impl FnMut(&mut H, &Lua, &str, MultiValue) -> mlua::Result<MultiValue>,
    body: impl FnOnce(&Lua) -> mlua::Result<R>,
) -> mlua::Result<R> {
    let prev: Value = lua.named_registry_value(DISPATCH)?;
    let result = lua.scope(|scope| {
        let f = scope.create_function_mut(|lua, (op, args): (String, MultiValue)| {
            dispatch(h, lua, &op, args)
        })?;
        lua.set_named_registry_value(DISPATCH, f)?;
        body(lua)
    });
    let _ = lua.set_named_registry_value(DISPATCH, prev);
    result
}

/// The user docs, embedded for `/help <topic>`.
const DOCS: &[(&str, &str)] = &[
    ("usage", include_str!("../../../../docs/usage.md")),
    ("lua", include_str!("../../../../docs/lua.md")),
    ("protocol", include_str!("../../../../docs/protocol.md")),
    (
        "architecture",
        include_str!("../../../../docs/architecture.md"),
    ),
];

/// Spans from Lua: a string, or a list of strings / `{ text, hl }`.
fn spans_arg(v: Value) -> mlua::Result<Vec<(String, String)>> {
    Ok(crate::model::parse_items(v, "Normal")?
        .into_iter()
        .map(|i| match i {
            crate::model::Item::Text(t, h) | crate::model::Item::Fill(t, h) => (t, h),
        })
        .collect())
}

fn spans_out(lua: &Lua, spans: Vec<(String, String)>) -> mlua::Result<Table> {
    let t = lua.create_table_with_capacity(spans.len(), 0)?;
    for (text, hl) in spans {
        t.push(lua.create_sequence_from([text, hl])?)?;
    }
    Ok(t)
}

/// `bone.markdown` and `bone.text`: pure helpers for views.
fn install_text(lua: &Lua, bone: &Table) -> mlua::Result<()> {
    let md = lua.create_table()?;
    md.set(
        "parse",
        lua.create_function(|lua, text: String| {
            to_lua(
                lua,
                &serde_json::Value::Array(crate::markdown::parse(&text)),
            )
        })?,
    )?;
    bone.set("markdown", md)?;

    let text = lua.create_table()?;
    text.set(
        "width",
        lua.create_function(|_, s: String| Ok(crate::text::width(&crate::text::sanitize(&s))))?,
    )?;
    text.set(
        "truncate",
        lua.create_function(|_, (s, n): (String, usize)| Ok(crate::text::truncate(&s, n)))?,
    )?;
    text.set(
        "clip",
        lua.create_function(|lua, (spans, n): (Value, usize)| {
            spans_out(lua, crate::text::clip_groups(&spans_arg(spans)?, n))
        })?,
    )?;
    text.set(
        "shell",
        lua.create_function(|lua, cmd: String| {
            let spans = crate::shellhl::highlight(&cmd)
                .into_iter()
                .map(|(t, g)| (t, g.to_owned()))
                .collect();
            spans_out(lua, spans)
        })?,
    )?;
    // wrap(spans, width, { first = prefix, rest = prefix, pad = "Group" }):
    // word-wrap to lines; `rest` defaults to blanks as wide as `first`, and
    // `pad` fills each line to the full width (for background bands).
    text.set(
        "wrap",
        lua.create_function(|lua, (spans, width, opts): (Value, usize, Option<Table>)| {
            let spans = spans_arg(spans)?;
            let first = opt::<Value>(&opts, "first")?
                .map(spans_arg)
                .transpose()?
                .unwrap_or_default();
            let rest = match opt(&opts, "rest")? {
                Some(v) => spans_arg(v)?,
                None => {
                    let w: usize = first.iter().map(|(t, _)| crate::text::width(t)).sum();
                    if w == 0 {
                        Vec::new()
                    } else {
                        vec![(" ".repeat(w), first[0].1.clone())]
                    }
                }
            };
            let pad: Option<String> = opt(&opts, "pad")?;
            let out = lua.create_table()?;
            for mut line in crate::text::wrap_groups(&spans, &first, &rest, width) {
                if let Some(hl) = &pad {
                    let used: usize = line.iter().map(|(t, _)| crate::text::width(t)).sum();
                    if used < width {
                        line.push((" ".repeat(width - used), hl.clone()));
                    }
                }
                out.push(spans_out(lua, line)?)?;
            }
            Ok(out)
        })?,
    )?;
    bone.set("text", text)?;
    Ok(())
}
