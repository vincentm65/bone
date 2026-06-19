//! Lua snapshot types — config, theme, and keymap tables read from Lua after init.lua runs.
//!
//! Rust snapshots these tables once at boot; the renderer and input handler
//! consume only the Rust copies.

use std::collections::HashMap;

// ── Spinner / text presets ──────────────────────────────────────────────────

/// A spinner style preset (frames + natural frame speed).
#[derive(Debug, Clone, Default)]
pub struct SpinnerPreset {
    pub name: String,
    /// Milliseconds per frame.
    pub speed: u64,
    pub frames: Vec<String>,
}

/// A rotating thinking-text preset.
#[derive(Debug, Clone, Default)]
pub struct TextPreset {
    pub name: String,
    pub phrases: Vec<String>,
}

/// Parse spinner presets, skipping any malformed entry rather than discarding
/// the whole list. A preset needs a `name` and at least one frame to be usable.
fn parse_spinner_presets(table: &mlua::Table) -> Vec<SpinnerPreset> {
    let mut out = Vec::new();
    for pair in table.pairs::<mlua::Value, mlua::Table>() {
        let Ok((_, t)) = pair else {
            continue;
        };
        let Ok(name) = t.get::<String>("name") else {
            eprintln!("bone-lua warn: spinner preset missing name; skipping");
            continue;
        };
        let speed: u64 = t.get::<Option<u64>>("speed").ok().flatten().unwrap_or(80);
        let frames = t
            .get::<Option<mlua::Table>>("frames")
            .ok()
            .flatten()
            .map(|ft| {
                ft.sequence_values::<String>()
                    .filter_map(|f| f.ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if frames.is_empty() {
            eprintln!("bone-lua warn: spinner preset '{name}' has no frames; skipping");
            continue;
        }
        out.push(SpinnerPreset {
            name,
            speed,
            frames,
        });
    }
    out
}

/// Parse rotating-text presets, skipping malformed entries (see
/// [`parse_spinner_presets`]).
fn parse_text_presets(table: &mlua::Table) -> Vec<TextPreset> {
    let mut out = Vec::new();
    for pair in table.pairs::<mlua::Value, mlua::Table>() {
        let Ok((_, t)) = pair else {
            continue;
        };
        let Ok(name) = t.get::<String>("name") else {
            eprintln!("bone-lua warn: text preset missing name; skipping");
            continue;
        };
        let phrases = t
            .get::<Option<mlua::Table>>("phrases")
            .ok()
            .flatten()
            .map(|ft| {
                ft.sequence_values::<String>()
                    .filter_map(|p| p.ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.push(TextPreset { name, phrases });
    }
    out
}

// ── Spinner / text preset collection ────────────────────────────────────────

/// `require("ui.spinners")` and parse its returned table into presets.
/// Returns empty vecs if the module is missing or malformed (never panics).
pub fn collect_presets(lua: &mlua::Lua) -> (Vec<SpinnerPreset>, Vec<TextPreset>) {
    let module: mlua::Table = match lua
        .load(r#"return require("ui.spinners")"#)
        .eval::<mlua::Table>()
    {
        Ok(t) => t,
        Err(_) => return (Vec::new(), Vec::new()),
    };
    let spinners = module
        .get::<Option<mlua::Table>>("spinners")
        .ok()
        .flatten()
        .map(|t| parse_spinner_presets(&t))
        .unwrap_or_default();
    let texts = module
        .get::<Option<mlua::Table>>("texts")
        .ok()
        .flatten()
        .map(|t| parse_text_presets(&t))
        .unwrap_or_default();
    (spinners, texts)
}

// ── Config snapshot ─────────────────────────────────────────────────────────

/// Subset of `bone.config` captured after init.lua.
#[derive(Debug, Clone, Default)]
pub struct LuaConfigSnapshot {
    pub approval_mode: Option<String>,
    pub status_show: HashMap<String, bool>,
    /// Spinner + text presets from `require("ui.spinners")` (boot snapshot).
    pub spinners: Vec<SpinnerPreset>,
    pub texts: Vec<TextPreset>,
}

impl LuaConfigSnapshot {
    /// Build a snapshot from the `bone.config` Lua table (or nil).
    pub fn from_lua_table(_lua: &mlua::Lua, table: &mlua::Table) -> Result<Self, String> {
        let approval_mode: Option<String> = table.get("approval_mode").ok().flatten();

        let status_show = table
            .get::<Option<mlua::Table>>("status_show")
            .ok()
            .flatten()
            .map(|t| {
                let mut map = HashMap::new();
                for (k, v) in t.pairs::<String, bool>().flatten() {
                    map.insert(k, v);
                }
                map
            })
            .unwrap_or_default();

        Ok(Self {
            approval_mode,
            status_show,
            spinners: Vec::new(),
            texts: Vec::new(),
        })
    }
}

// ── Theme snapshot ──────────────────────────────────────────────────────────

/// Subset of `bone.theme` captured after init.lua.
///
/// Colors are stored as raw strings; parsing into `ratatui::style::Color`
/// happens at the UI boundary in `Theme::apply_snapshot`.
#[derive(Debug, Clone, Default)]
pub struct LuaThemeSnapshot {
    pub user_msg: Option<String>,
    pub user_msg_bg: Option<String>,
    pub status_text: Option<String>,
    pub input_border: Option<String>,
    pub system_msg: Option<String>,
    pub approval_safe: Option<String>,
    pub approval_danger: Option<String>,
    pub tool_call: Option<String>,
    pub tool_error: Option<String>,
    pub diff_removed: Option<String>,
    pub diff_added: Option<String>,
    pub thinking: Option<String>,
    pub tab_active: Option<String>,
}

impl LuaThemeSnapshot {
    /// Build a snapshot from the `bone.theme` Lua table (or nil).
    pub fn from_lua_table(_lua: &mlua::Lua, table: &mlua::Table) -> Result<Self, String> {
        let get_color = |key: &str| -> Option<String> {
            let hex: Option<String> = table.get(key).ok().flatten();
            hex
        };

        Ok(Self {
            user_msg: get_color("user_msg"),
            user_msg_bg: get_color("user_msg_bg"),
            status_text: get_color("status_text"),
            input_border: get_color("input_border"),
            system_msg: get_color("system_msg"),
            approval_safe: get_color("approval_safe"),
            approval_danger: get_color("approval_danger"),
            tool_call: get_color("tool_call"),
            tool_error: get_color("tool_error"),
            diff_removed: get_color("diff_removed"),
            diff_added: get_color("diff_added"),
            thinking: get_color("thinking"),
            tab_active: get_color("tab_active"),
        })
    }
}

// ── Keymap snapshot ─────────────────────────────────────────────────────────

/// A single key binding: key string → action name.
#[derive(Debug, Clone)]
pub struct LuaKeyBinding {
    pub key: String,
    pub action: String,
}

/// Snapshot of `bone.keymap` after init.lua.
#[derive(Debug, Clone, Default)]
pub struct LuaKeymapSnapshot {
    pub normal: Vec<LuaKeyBinding>,
    pub insert: Vec<LuaKeyBinding>,
}

impl LuaKeymapSnapshot {
    /// Build a snapshot from the `bone.keymap` Lua table (or nil).
    pub fn from_lua_table(_lua: &mlua::Lua, table: &mlua::Table) -> Result<Self, String> {
        let parse_mode = |key: &str| -> Result<Vec<LuaKeyBinding>, String> {
            let mode_table: Option<mlua::Table> = table.get(key).ok().flatten();
            let mode_table = match mode_table {
                Some(t) => t,
                None => return Ok(Vec::new()),
            };
            let mut bindings = Vec::new();
            for pair in mode_table.pairs::<String, String>() {
                match pair {
                    Ok((k, v)) => bindings.push(LuaKeyBinding { key: k, action: v }),
                    Err(e) => eprintln!("bone-lua warn: invalid keymap entry: {e}"),
                }
            }
            Ok(bindings)
        };

        Ok(Self {
            normal: parse_mode("n")?,
            insert: parse_mode("i")?,
        })
    }
}
