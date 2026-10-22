//! Keys, for every frontend: from a pressed key to what it does. A focused
//! window's keys come first, then a focused panel's, then raw interceptors,
//! then the keymaps of the context in use (with key sequences and their
//! timeout), then the frontend types an unmapped key. Built-in actions may
//! be taken over by Lua (`bone.ui.actions`); what one finally does to the
//! prompt and chat is the frontend's.

use std::time::{Duration, Instant};

use mlua::{Function, Lua, MultiValue, Table, Value};

use super::{
    Host, Owned, args, context_from_name, drop_callback, emit_focus_changed, err, execute, fire,
    opt, own, panels, ret, store_callback,
};
use crate::keymap::{Action, Builtin, Context};
use crate::keys::{self, Key};
use crate::panel::WHEEL_ROWS;
use crate::popup;

/// A Lua function that sees raw key names before the keymaps.
#[derive(Debug, Clone)]
pub struct RawInterceptor {
    pub id: u64,
    pub callback: u64,
    pub context: Option<Context>,
}

/// Where the keyboard is and what it is in the middle of.
#[derive(Debug, Default)]
pub struct KeyState {
    /// The explicitly focused named context (popup and panel focus still
    /// take precedence).
    pub focused_context: Option<Context>,
    /// A key sequence typed so far, the action its prefix would also run,
    /// the context it started in, and when.
    pub pending_keys: Vec<Key>,
    pub pending_action: Option<Action>,
    pub pending_context: Option<Context>,
    pub pending_since: Option<Instant>,
    pub raw_interceptors: Vec<RawInterceptor>,
    /// Builtin actions whose `bone.ui.actions` handler is running (so a
    /// handler that runs its own action gets the built-in one).
    pub in_actions: Vec<Builtin>,
}

pub(super) fn dispatch<H: Host>(
    h: &mut H,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    Ok(match op {
        "keymap_focus" => args::<Option<String>>(lua, a)
            .and_then(|name| match name {
                Some(name) => focus_context(h, context_from_name(name)?).map_err(err),
                None => clear_context(h).map_err(err),
            })
            .and_then(|()| ret(lua, ())),
        "keymap_clear" => args::<()>(lua, a)
            .and_then(|()| clear_context(h).map_err(err))
            .and_then(|()| ret(lua, ())),
        "keymap_context_del" => args::<String>(lua, a)
            .and_then(context_from_name)
            .and_then(|ctx| delete_context(h, &ctx).map_err(err))
            .and_then(|()| ret(lua, ())),
        "keymap_raw" => keymap_raw(h, lua, a),
        "keymap_raw_del" => keymap_raw_del(h, lua, a),
        "press" => args::<String>(lua, a)
            .and_then(|name| keys::parse(&name).map_err(err))
            .map(|key| handle_key(h, key))
            .and_then(|()| ret(lua, ())),
        "action" => args::<String>(lua, a)
            .and_then(|name| {
                Builtin::from_name(&name).ok_or_else(|| err(format!("unknown action: {name}")))
            })
            .map(|b| run(h, Action::Builtin(b)))
            .and_then(|()| ret(lua, ())),
        _ => return Err(a),
    })
}

/// `opts.context` of a raw interceptor: none for every context.
fn raw_context_arg(opts: &Option<Table>) -> mlua::Result<Option<Context>> {
    opt(opts, "context")?.map(context_from_name).transpose()
}

fn keymap_raw<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (f, opts): (Function, Option<Table>) = args(lua, a)?;
    let context = raw_context_arg(&opts)?;
    let callback = store_callback(h, lua, f)?;
    let next = h.owners().next_callback;
    *next += 1;
    let id = *next;
    h.key_state().raw_interceptors.push(RawInterceptor {
        id,
        callback,
        context,
    });
    own(h, Owned::Raw(id));
    ret(lua, id)
}

fn keymap_raw_del<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let id: u64 = args(lua, a)?;
    let raws = &mut h.key_state().raw_interceptors;
    match raws.iter().position(|raw| raw.id == id) {
        Some(i) => {
            drop_callback(lua, raws.remove(i).callback)?;
            ret(lua, true)
        }
        None => ret(lua, false),
    }
}

/// What has the keyboard: a focused window, a focused panel (its context,
/// or `panel`), a focused named context, else `main`.
pub fn context_of(
    popups: &[popup::Popup],
    panels_: &[crate::panel::Panel],
    panel_focus: &Option<String>,
    keys: &KeyState,
) -> Context {
    if popup::focused(popups).is_some() {
        Context::Popup
    } else if let Some(p) = panels::focused(panels_, panel_focus) {
        p.context.clone().unwrap_or(Context::Panel)
    } else {
        keys.focused_context.clone().unwrap_or(Context::Main)
    }
}

pub fn focus_context<H: Host>(h: &mut H, ctx: Context) -> Result<(), String> {
    match &ctx {
        Context::Main => return clear_context(h),
        Context::Popup => return Err("popup context is controlled by focused windows".into()),
        Context::Panel => return Err("panel context is controlled by focused panels".into()),
        Context::Named(_) if !h.keymap_store().keymaps.has_context(&ctx) => {
            return Err(format!("unknown context {}", ctx.name()));
        }
        Context::Named(_) => {}
    }
    flush_pending(h);
    h.key_state().focused_context = Some(ctx);
    emit_focus_changed(h);
    h.changed();
    Ok(())
}

pub fn clear_context<H: Host>(h: &mut H) -> Result<(), String> {
    flush_pending(h);
    h.key_state().focused_context = None;
    emit_focus_changed(h);
    h.changed();
    Ok(())
}

pub fn delete_context<H: Host>(h: &mut H, ctx: &Context) -> Result<(), String> {
    if !h.keymap_store().keymaps.has_context(ctx) {
        return Err(format!("unknown context {}", ctx.name()));
    }
    if h.key_state().focused_context.as_ref() == Some(ctx) {
        flush_pending(h);
        h.key_state().focused_context = None;
        emit_focus_changed(h);
    }
    h.keymap_store().keymaps.remove_context(ctx)?;
    h.changed();
    Ok(())
}

/// When a pending key sequence gives up waiting (`bone.o.timeoutlen`).
pub fn key_sequence_deadline<H: Host>(h: &mut H) -> Option<Instant> {
    let timeout = h.option_store().options.timeoutlen;
    (h.key_state().pending_since).map(|since| since + Duration::from_millis(timeout))
}

/// Expire a pending key sequence whose deadline has passed; called by the
/// frontend's loop even when nothing is being redrawn.
pub fn expire_key_sequence<H: Host>(h: &mut H) -> bool {
    if key_sequence_deadline(h).is_some_and(|deadline| Instant::now() >= deadline) {
        flush_pending(h);
        h.changed();
        true
    } else {
        false
    }
}

/// A pressed key.
pub fn handle_key<H: Host>(h: &mut H, key: Key) {
    h.key_starting();
    let ctx = h.context();
    fire(
        h,
        "key",
        serde_json::json!({ "key": keys::format(&key), "context": ctx.name() }),
    );
    if h.key_state()
        .pending_context
        .as_ref()
        .is_some_and(|pending| pending != &ctx)
    {
        flush_pending(h);
    }
    dispatch_key(h, key, true);
    h.prompt_changed();
}

/// Run an action as a key would.
pub fn run<H: Host>(h: &mut H, action: Action) {
    h.changed();
    let _ = dispatch_action(h, action);
    h.prompt_changed();
}

fn dispatch_key<H: Host>(h: &mut H, key: Key, allow_raw: bool) {
    let ctx = h.context();
    if ctx == Context::Popup {
        return popup_key(h, key, allow_raw);
    }
    h.dismiss_message();
    // A focused panel's own keys come first (not in the middle of a key
    // sequence).
    if h.key_state().pending_keys.is_empty()
        && let Some(id) = h.focused_panel()
        && panel_key(h, &id, key)
    {
        return;
    }
    let ctx = h.context();
    if ctx == Context::Popup {
        // A panel key opened a popup; the key was used.
        return;
    }
    if allow_raw && raw_intercepts(h, &ctx, key) {
        discard_pending(h);
        return;
    }
    keymap_key(h, ctx, key);
}

fn keymap_key<H: Host>(h: &mut H, ctx: Context, key: Key) {
    let had_pending = !h.key_state().pending_keys.is_empty();
    let mut sequence = h.key_state().pending_keys.clone();
    sequence.push(key);
    let lookup = h.keymap_store().keymaps.lookup(&ctx, &sequence);

    if lookup.prefix {
        set_pending(h, sequence, lookup.action, ctx);
        return;
    }

    if let Some(action) = lookup.action {
        discard_pending(h);
        // A Lua function on ctrl+c that errors (or passes the key on) must
        // not leave bone without a way to quit.
        if let Action::Lua(id) = action
            && sequence == [Key::ctrl_c()]
        {
            match h.call_callback(id, "keymap", |_| Ok(Value::Nil)) {
                None | Some(Value::Boolean(false)) => h.interrupt(),
                Some(_) => {}
            }
            return;
        }
        if !dispatch_action(h, action) {
            replay_literal(h, &ctx, &sequence);
        }
        return;
    }

    if !had_pending {
        // Keys all come from Lua. If its config is broken and nothing maps
        // ctrl+c here, it still interrupts (directly, not through Lua), so
        // bone can be quit.
        if key == Key::ctrl_c() {
            return h.interrupt();
        }
        h.type_key(&ctx, key);
        return;
    }

    let (pending, fallback, pending_ctx) = take_pending(h);
    if let Some(action) = fallback {
        if !dispatch_action(h, action) && h.context() == pending_ctx {
            replay_literal(h, &pending_ctx, &pending);
        }
    } else if h.context() == pending_ctx {
        // A prefix with no exact fallback is literal when the next key
        // proves it was not a sequence.
        replay_literal(h, &pending_ctx, &pending);
    }
    // The mismatching key was not consumed by the failed sequence.
    dispatch_key(h, key, false);
}

fn replay_literal<H: Host>(h: &mut H, ctx: &Context, keys: &[Key]) {
    for &key in keys {
        h.type_key(ctx, key);
    }
}

/// Run `action`; false when a Lua mapping passed the key on.
fn dispatch_action<H: Host>(h: &mut H, action: Action) -> bool {
    h.changed();
    match action {
        Action::Builtin(b) => {
            builtin(h, b);
            true
        }
        Action::Command(cmd) => {
            execute(h, &cmd);
            true
        }
        Action::Lua(id) => !matches!(
            h.call_callback(id, "keymap", |_| Ok(Value::Nil)),
            Some(Value::Boolean(false))
        ),
    }
}

fn set_pending<H: Host>(h: &mut H, keys: Vec<Key>, action: Option<Action>, ctx: Context) {
    let k = h.key_state();
    k.pending_keys = keys;
    k.pending_action = action;
    k.pending_context = Some(ctx);
    k.pending_since = Some(Instant::now());
}

fn take_pending<H: Host>(h: &mut H) -> (Vec<Key>, Option<Action>, Context) {
    let k = h.key_state();
    let keys = std::mem::take(&mut k.pending_keys);
    let action = k.pending_action.take();
    let ctx = k.pending_context.take().unwrap_or(Context::Main);
    k.pending_since = None;
    (keys, action, ctx)
}

pub fn discard_pending<H: Host>(h: &mut H) {
    let k = h.key_state();
    k.pending_keys.clear();
    k.pending_action = None;
    k.pending_context = None;
    k.pending_since = None;
}

/// Finish a pending sequence now (its prefix's action, or its keys typed).
pub fn flush_pending<H: Host>(h: &mut H) {
    if h.key_state().pending_keys.is_empty() {
        discard_pending(h);
        return;
    }
    let (keys, action, ctx) = take_pending(h);
    if let Some(action) = action
        && !dispatch_action(h, action)
        && h.context() == ctx
    {
        replay_literal(h, &ctx, &keys);
    }
    h.prompt_changed();
}

fn raw_intercepts<H: Host>(h: &mut H, ctx: &Context, key: Key) -> bool {
    let name = keys::format(&key);
    let callbacks: Vec<u64> = (h.key_state().raw_interceptors.iter())
        .filter(|raw| raw.context.as_ref().is_none_or(|wanted| wanted == ctx))
        .map(|raw| raw.callback)
        .collect();
    callbacks.into_iter().any(|callback| {
        matches!(
            h.call_callback(callback, "raw key", |lua| {
                Ok(Value::String(lua.create_string(&name)?))
            }),
            Some(Value::Boolean(true))
        )
    })
}

/// A key while a window has the keyboard: its own keys, then its `on_key`,
/// then the `popup` keymaps.
fn popup_key<H: Host>(h: &mut H, key: Key, allow_raw: bool) {
    let Some((guard, own, on_key)) = popup::focused(h.popups()).map(|top| {
        (
            top.opened.elapsed() < top.guard,
            top.keys
                .iter()
                .find(|(mapped, _)| *mapped == key)
                .map(|(_, cb)| *cb),
            top.on_key,
        )
    }) else {
        return;
    };
    if guard {
        discard_pending(h);
        return;
    }
    if let Some(cb) = own {
        discard_pending(h);
        h.call_callback(cb, "popup key", |_| Ok(Value::Nil));
        return;
    }
    if let Some(cb) = on_key {
        let name = keys::format(&key);
        let r = h.call_callback(cb, "popup on_key", |lua| {
            Ok(Value::String(lua.create_string(&name)?))
        });
        h.changed();
        // Returning nil or false passes the key on to the keymaps.
        if !matches!(r, None | Some(Value::Nil | Value::Boolean(false))) {
            discard_pending(h);
            return;
        }
    }
    let ctx = h.context();
    if ctx != Context::Popup {
        return dispatch_key(h, key, false);
    }
    if allow_raw && raw_intercepts(h, &ctx, key) {
        discard_pending(h);
        return;
    }
    keymap_key(h, ctx, key);
}

/// A focused panel's own keys: `keys[name](panel)`, then
/// `on_key(name, panel)`. Returns whether the key was used.
pub fn panel_key<H: Host>(h: &mut H, id: &str, key: Key) -> bool {
    let Some((own, on_key)) = panels::panel(h.panel_store().panels, id).map(|p| {
        (
            p.keys.iter().find(|(k, _)| *k == key).map(|(_, cb)| *cb),
            p.on_key,
        )
    }) else {
        return false;
    };
    if let Some(cb) = own {
        let id = id.to_owned();
        h.call_callback(cb, "panel key", |lua| panels::panel_handle(lua, &id));
        h.changed();
        return true;
    }
    if let Some(cb) = on_key {
        let name = keys::format(&key);
        let id = id.to_owned();
        let r = h.call_callback_multi(cb, "panel on_key", |lua| {
            Ok(vec![
                Value::String(lua.create_string(&name)?),
                panels::panel_handle(lua, &id)?,
            ]
            .into())
        });
        h.changed();
        // nil or false passes the key on to the keymaps.
        return !matches!(
            r.as_ref().and_then(|v| v.front()),
            None | Some(Value::Nil | Value::Boolean(false))
        );
    }
    false
}

/// Scroll and dismiss actions while a panel has the keyboard. Returns
/// whether `b` applied to the panel.
pub fn panel_builtin<H: Host>(h: &mut H, b: Builtin) -> bool {
    use Builtin::*;
    let Some(id) = h.focused_panel() else {
        return false;
    };
    if b == Dismiss {
        let _ = panels::focus_panel(h, None);
        return true;
    }
    let Some(p) = h.panel_store().panels.iter_mut().find(|p| p.id == id) else {
        return false;
    };
    match b {
        Up => p.scroll_by(-1),
        Down => p.scroll_by(1),
        ScrollUp => p.scroll_by(-WHEEL_ROWS),
        ScrollDown => p.scroll_by(WHEEL_ROWS),
        PageUp => p.scroll_by(-p.page()),
        PageDown => p.scroll_by(p.page()),
        ScrollTop => p.scroll_to(false),
        ScrollBottom => p.scroll_to(true),
        _ => return false,
    }
    h.changed();
    true
}

/// Move the keyboard to the next (or previous) focusable panel; the prompt
/// is part of the cycle.
pub fn cycle_focus<H: Host>(h: &mut H, forward: bool) {
    let s = h.panel_store();
    let mut ring: Vec<Option<String>> = vec![None];
    ring.extend(
        (panels::in_order(s.panels).into_iter())
            .filter(|p| p.focusable && !p.hidden)
            .map(|p| Some(p.id.clone())),
    );
    let now = panels::focused(s.panels, s.focus).map(|p| p.id.clone());
    let i = ring.iter().position(|r| *r == now).unwrap_or(0);
    let n = ring.len();
    let next = ring[if forward {
        (i + 1) % n
    } else {
        (i + n - 1) % n
    }]
    .clone();
    let _ = panels::focus_panel(h, next.as_deref());
}

/// A built-in action: a focused panel's scrolling and dismiss, then what the
/// frontend does to its prompt selection, then Lua's `bone.ui.actions`
/// handler, then the frontend's own.
pub fn builtin<H: Host>(h: &mut H, b: Builtin) {
    h.builtin_starting(b);
    if panel_builtin(h, b) {
        return;
    }
    if h.builtin_on_selection(b) {
        return;
    }
    if lua_action(h, b) {
        return;
    }
    h.builtin_action(b);
}

/// Run `bone.ui.actions[name]` for a builtin action, if Lua set one.
/// Returns whether it handled the action (it returned true).
pub fn lua_action<H: Host>(h: &mut H, b: Builtin) -> bool {
    if !h.has_lua() || h.key_state().in_actions.contains(&b) {
        return false;
    }
    h.key_state().in_actions.push(b);
    let name = b.name();
    let r = h.with_api(|lua| {
        let ui: Table = lua.globals().get::<Table>("bone")?.get("ui")?;
        let Some(actions) = ui.get::<Option<Table>>("actions")? else {
            return Ok(false);
        };
        let Some(f) = actions.get::<Option<Function>>(name)? else {
            return Ok(false);
        };
        Ok(matches!(f.call::<Value>(())?, Value::Boolean(true)))
    });
    h.key_state().in_actions.retain(|x| *x != b);
    match r {
        Ok(handled) => handled,
        Err(e) => {
            h.lua_error(&format!("bone.ui.actions.{name}"), &e);
            false
        }
    }
}
