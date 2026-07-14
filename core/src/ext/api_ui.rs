//! `bone.api.ui.*` — the minimal Lua UI API (Phase 4 slice of `bone.api`).
//!
//! Lua draws UI by emitting [`ViewDiff`]s into a shared [`UiState`]: open a
//! floating window, replace its lines, set the status line, define a highlight,
//! close it. The state holds the canonical [`ViewModel`] (so frontends can read
//! the current view) plus the list of diffs accumulated since the last drain
//! (so a frontend / RPC client can be sent only what changed).
//!
//! This module owns no rendering — it produces data. The TUI turns a `Float`
//! component into a `PanePage` via `Component::as_pane_content`; a remote client
//! receives the diffs over the Phase 5 transport. The broader, always-available
//! `bone.api` surface arrives in Phase 6; this is the UI slice it builds on.
//!
//! **v2 transport.** The [`SharedUi`] handle is a standalone
//! `Arc<Mutex<UiState>>` — it lives on the [`ExtensionManager`] and is also
//! captured by every `ctx.ui.pane` closure. Both Lua entry
//! calls push into the same handle; the TUI drains it on every render tick by
//! locking the `UiState` mutex directly (never the Lua VM mutex), so pane
//! updates render even while a tool blocks on `ctx.ui.key()`.

use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt, Table, Value};
use serde::Deserialize;

use crate::pane_content::PaneLineSpec;
use crate::runtime::view::{Anchor, Component, FloatRect, StatusSegment, ViewDiff, ViewModel};

/// Shared UI state mutated by the Lua API and read by frontends.
#[derive(Default)]
pub struct UiState {
    pub view: ViewModel,
    /// Diffs accumulated since the last [`drain_diffs`](UiState::drain_diffs).
    pub diffs: Vec<ViewDiff>,
    /// Current terminal width in columns, published by the renderer each frame
    /// so Lua panes (`ctx.ui.width`) can wrap text. 0 = not yet known.
    pub terminal_width: u16,
}

impl UiState {
    /// Fold a diff into the canonical view and record it for frontends.
    pub fn apply(&mut self, diff: ViewDiff) {
        self.view.apply(&diff);
        self.diffs.push(diff);
    }

    /// Take and clear the pending diffs (a frontend renders these and acks).
    pub fn drain_diffs(&mut self) -> Vec<ViewDiff> {
        std::mem::take(&mut self.diffs)
    }
}

/// Standalone shared UI-state handle. Lives on the [`ExtensionManager`] and is
/// cloned into every Lua closure that emits view diffs (`bone.api.ui.*`,
/// `ctx.ui.pane`). The TUI drains it without touching the Lua
/// VM mutex.
///
/// [`ExtensionManager`]: crate::ext::ExtensionManager
pub type SharedUi = Arc<Mutex<UiState>>;

/// Create a fresh standalone handle.
pub fn new_shared() -> SharedUi {
    Arc::new(Mutex::new(UiState::default()))
}

fn lock(ui: &SharedUi) -> std::sync::MutexGuard<'_, UiState> {
    ui.lock().unwrap_or_else(|e| e.into_inner())
}
/// Lock a standalone `SharedUi` handle (never touches the Lua VM mutex).
pub fn lock_shared(ui: &SharedUi) -> std::sync::MutexGuard<'_, UiState> {
    lock(ui)
}

fn default_width() -> u16 {
    40
}
fn default_height() -> u16 {
    10
}

#[derive(Deserialize)]
struct FloatOpts {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    lines: Vec<PaneLineSpec>,
    #[serde(default)]
    anchor: Anchor,
    #[serde(default = "default_width")]
    width: u16,
    #[serde(default = "default_height")]
    height: u16,
    #[serde(default)]
    col: i16,
    #[serde(default)]
    row: i16,
    #[serde(default)]
    z: i32,
    #[serde(default)]
    border: bool,
}

/// Convert a Lua value to a serde type via JSON, which fully supports the
/// untagged `PaneLineSpec` enum (mlua's deserializer is flakier with untagged).
fn from_lua<T: serde::de::DeserializeOwned>(lua: &Lua, value: Value) -> mlua::Result<T> {
    let json: serde_json::Value = lua.from_value(value)?;
    serde_json::from_value(json).map_err(|e| mlua::Error::external(format!("ui api: {e}")))
}

/// Register `bone.api.ui.*` against a standalone [`SharedUi`] handle. Each
/// closure captures its own clone of the handle so it never has to look it up
/// from Lua `app_data` (and therefore never touches the VM mutex to emit a diff).
///
/// Idempotent-ish: creates `bone.api` if absent, then sets `bone.api.ui`.
pub fn setup_api_ui(lua: &Lua, bone: &Table, shared_ui: SharedUi) -> Result<(), String> {
    let api: Table = match bone
        .get::<Option<Table>>("api")
        .map_err(crate::util::errstr)?
    {
        Some(t) => t,
        None => {
            let t = lua.create_table().map_err(crate::util::errstr)?;
            bone.set("api", &t).map_err(crate::util::errstr)?;
            t
        }
    };

    let ui = lua.create_table().map_err(crate::util::errstr)?;

    // open_float(opts) -> id
    let ui_state = shared_ui.clone();
    let open_float = lua
        .create_function(move |lua, opts: Value| {
            let o: FloatOpts = from_lua(lua, opts)?;
            let id = o.id.clone();
            let component = Component::Float {
                id: o.id,
                title: o.title,
                lines: o.lines,
                rect: FloatRect {
                    anchor: o.anchor,
                    width: o.width,
                    height: o.height,
                    col: o.col,
                    row: o.row,
                },
                z: o.z,
                border: o.border,
                scroll: 0,
            };
            lock(&ui_state).apply(ViewDiff::Upsert { component });
            Ok(id)
        })
        .map_err(crate::util::errstr)?;
    ui.set("open_float", open_float)
        .map_err(crate::util::errstr)?;

    // set_lines(id, lines) -> bool (true if the float existed and was updated)
    let ui_state = shared_ui.clone();
    let set_lines = lua
        .create_function(move |lua, (id, lines_val): (String, Value)| {
            let lines: Vec<PaneLineSpec> = from_lua(lua, lines_val)?;
            let mut guard = lock(&ui_state);
            // Re-upsert the existing float with new lines, preserving placement.
            let updated = if let Some(Component::Float {
                title,
                rect,
                z,
                border,
                scroll,
                ..
            }) = guard.view.get(&id)
            {
                let component = Component::Float {
                    id: id.clone(),
                    title: title.clone(),
                    lines,
                    rect: *rect,
                    z: *z,
                    border: *border,
                    scroll: *scroll,
                };
                guard.apply(ViewDiff::Upsert { component });
                true
            } else {
                false
            };
            Ok(updated)
        })
        .map_err(crate::util::errstr)?;
    ui.set("set_lines", set_lines)
        .map_err(crate::util::errstr)?;

    // close(id)
    let ui_state = shared_ui.clone();
    let close = lua
        .create_function(move |_, id: String| {
            lock(&ui_state).apply(ViewDiff::Remove { id });
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    ui.set("close", close).map_err(crate::util::errstr)?;

    // set_statusline(id, segments)
    let ui_state = shared_ui.clone();
    let set_statusline = lua
        .create_function(move |lua, (id, segments_val): (String, Value)| {
            let segments: Vec<StatusSegment> = from_lua(lua, segments_val)?;
            lock(&ui_state).apply(ViewDiff::Upsert {
                component: Component::StatusLine { id, segments },
            });
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    ui.set("set_statusline", set_statusline)
        .map_err(crate::util::errstr)?;

    // set_highlight(name, fg|nil)
    let ui_state = shared_ui.clone();
    let set_highlight = lua
        .create_function(move |_, (name, fg): (String, Option<String>)| {
            lock(&ui_state).apply(ViewDiff::SetHighlight { name, fg });
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    ui.set("set_highlight", set_highlight)
        .map_err(crate::util::errstr)?;

    // term_width() -> columns. Queries the live terminal size via ioctl on
    // every call (defaults to 80 when not a tty). Lua is sandboxed so it can't
    // query the kernel itself; this is the one Rust primitive that gives it.
    // The terminal query lives behind the `tui` feature (crossterm is TUI-only);
    // headless/core builds report the 80-column fallback.
    let term_width = lua
        .create_function(|_, _: ()| {
            #[cfg(feature = "tui")]
            let w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
            #[cfg(not(feature = "tui"))]
            let w = 80u16;
            Ok(w)
        })
        .map_err(crate::util::errstr)?;
    ui.set("term_width", term_width)
        .map_err(crate::util::errstr)?;

    api.set("ui", ui).map_err(crate::util::errstr)?;
    Ok(())
}

/// Take the pending [`ViewDiff`]s from a standalone [`SharedUi`] handle
/// (frontend render tick). Locks the `UiState` mutex only — never the Lua VM.
pub fn drain_diffs(ui: &SharedUi) -> Vec<ViewDiff> {
    lock(ui).drain_diffs()
}

/// Snapshot the current [`ViewModel`] from a standalone [`SharedUi`] handle
/// (e.g. for a late-joining frontend that needs full state before receiving
/// diffs). Locks the `UiState` mutex only — never the Lua VM.
pub fn snapshot(ui: &SharedUi) -> ViewModel {
    lock(ui).view.clone()
}

#[cfg(test)]
#[path = "api_ui_tests.rs"]
mod api_ui_tests;
