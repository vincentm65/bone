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

/// Handle stored in Lua app-data so every `bone.api.ui` closure shares one state.
pub type SharedUi = Arc<Mutex<UiState>>;

fn lock(ui: &SharedUi) -> std::sync::MutexGuard<'_, UiState> {
    ui.lock().unwrap_or_else(|e| e.into_inner())
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

fn shared(lua: &Lua) -> mlua::Result<SharedUi> {
    lua.app_data_ref::<SharedUi>()
        .map(|r| r.clone())
        .ok_or_else(|| mlua::Error::external("bone.api.ui not initialized"))
}

/// Register `bone.api.ui.*` against a shared [`UiState`] stored in Lua app-data.
///
/// Idempotent-ish: creates `bone.api` if absent, then sets `bone.api.ui`.
pub fn setup_api_ui(lua: &Lua, bone: &Table) -> Result<(), String> {
    // One shared state per VM, retrievable later via `drain_diffs` / `snapshot`.
    if lua.app_data_ref::<SharedUi>().is_none() {
        lua.set_app_data::<SharedUi>(Arc::new(Mutex::new(UiState::default())));
    }

    let api: Table = match bone
        .get::<Option<Table>>("api")
        .map_err(|e| e.to_string())?
    {
        Some(t) => t,
        None => {
            let t = lua.create_table().map_err(|e| e.to_string())?;
            bone.set("api", &t).map_err(|e| e.to_string())?;
            t
        }
    };

    let ui = lua.create_table().map_err(|e| e.to_string())?;

    // open_float(opts) -> id
    let open_float = lua
        .create_function(|lua, opts: Value| {
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
            };
            lock(&shared(lua)?).apply(ViewDiff::Upsert { component });
            Ok(id)
        })
        .map_err(|e| e.to_string())?;
    ui.set("open_float", open_float)
        .map_err(|e| e.to_string())?;

    // set_lines(id, lines) -> bool (true if the float existed and was updated)
    let set_lines = lua
        .create_function(|lua, (id, lines_val): (String, Value)| {
            let lines: Vec<PaneLineSpec> = from_lua(lua, lines_val)?;
            let ui = shared(lua)?;
            let mut guard = lock(&ui);
            // Re-upsert the existing float with new lines, preserving placement.
            let updated = if let Some(Component::Float {
                title,
                rect,
                z,
                border,
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
                };
                guard.apply(ViewDiff::Upsert { component });
                true
            } else {
                false
            };
            Ok(updated)
        })
        .map_err(|e| e.to_string())?;
    ui.set("set_lines", set_lines).map_err(|e| e.to_string())?;

    // close(id)
    let close = lua
        .create_function(|lua, id: String| {
            lock(&shared(lua)?).apply(ViewDiff::Remove { id });
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    ui.set("close", close).map_err(|e| e.to_string())?;

    // set_statusline(id, segments)
    let set_statusline = lua
        .create_function(|lua, (id, segments_val): (String, Value)| {
            let segments: Vec<StatusSegment> = from_lua(lua, segments_val)?;
            lock(&shared(lua)?).apply(ViewDiff::Upsert {
                component: Component::StatusLine { id, segments },
            });
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    ui.set("set_statusline", set_statusline)
        .map_err(|e| e.to_string())?;

    // set_highlight(name, fg|nil)
    let set_highlight = lua
        .create_function(|lua, (name, fg): (String, Option<String>)| {
            lock(&shared(lua)?).apply(ViewDiff::SetHighlight { name, fg });
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    ui.set("set_highlight", set_highlight)
        .map_err(|e| e.to_string())?;

    api.set("ui", ui).map_err(|e| e.to_string())?;
    Ok(())
}

/// Take the pending [`ViewDiff`]s from this VM's UI state (frontend render tick).
pub fn drain_diffs(lua: &Lua) -> Vec<ViewDiff> {
    match lua.app_data_ref::<SharedUi>() {
        Some(ui) => lock(&ui).drain_diffs(),
        None => Vec::new(),
    }
}

/// Snapshot the current [`ViewModel`] for this VM (e.g. for a late-joining
/// frontend that needs full state before receiving diffs).
pub fn snapshot(lua: &Lua) -> ViewModel {
    match lua.app_data_ref::<SharedUi>() {
        Some(ui) => lock(&ui).view.clone(),
        None => ViewModel::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua_with_api() -> Lua {
        let lua = Lua::new();
        let bone = lua.create_table().unwrap();
        setup_api_ui(&lua, &bone).unwrap();
        lua.globals().set("bone", bone).unwrap();
        lua
    }

    #[test]
    fn open_float_produces_upsert_diff_and_view_component() {
        let lua = lua_with_api();
        let id: String = lua
            .load(
                r#"
                return bone.api.ui.open_float({
                    id = "help",
                    title = "Help",
                    lines = { "first line", "second line" },
                    width = 50, height = 12, border = true,
                })
            "#,
            )
            .eval()
            .unwrap();
        assert_eq!(id, "help");

        // The view now has the float, and a diff was recorded.
        let vm = snapshot(&lua);
        let comp = vm.get("help").expect("float in view");
        let pc = comp.as_pane_content().unwrap();
        assert_eq!(pc.title, "Help");
        assert_eq!(pc.lines.len(), 2);
        assert_eq!(pc.visible_rows, 12);

        let diffs = drain_diffs(&lua);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(
            &diffs[0],
            ViewDiff::Upsert { component } if component.id() == "help"
        ));
        // Drained: a second drain is empty.
        assert!(drain_diffs(&lua).is_empty());
    }

    #[test]
    fn set_lines_updates_existing_float() {
        let lua = lua_with_api();
        lua.load(
            r#"
            bone.api.ui.open_float({ id = "f", lines = { "a" }, width = 20, height = 3 })
            local ok = bone.api.ui.set_lines("f", { "b", "c" })
            assert(ok == true, "set_lines should report success")
            local missing = bone.api.ui.set_lines("nope", { "x" })
            assert(missing == false, "set_lines on missing id is false")
        "#,
        )
        .exec()
        .unwrap();

        let pc = snapshot(&lua).get("f").unwrap().as_pane_content().unwrap();
        assert_eq!(pc.lines.len(), 2);
        assert!(matches!(&pc.lines[1], PaneLineSpec::Plain(s) if s == "c"));
    }

    #[test]
    fn close_removes_and_statusline_and_highlight_apply() {
        let lua = lua_with_api();
        lua.load(
            r##"
            bone.api.ui.open_float({ id = "f", lines = { "a" } })
            bone.api.ui.set_statusline("status", {
                { text = "ready", fg = "green", align = "right" },
            })
            bone.api.ui.set_highlight("error", "#ff0000")
            bone.api.ui.close("f")
        "##,
        )
        .exec()
        .unwrap();

        let vm = snapshot(&lua);
        assert!(vm.get("f").is_none(), "closed float removed");
        assert!(vm.get("status").is_some(), "statusline present");
        assert_eq!(
            vm.highlights.get("error").map(String::as_str),
            Some("#ff0000")
        );
    }
}
