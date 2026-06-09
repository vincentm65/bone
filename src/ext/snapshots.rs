//! Lua snapshot types — config, theme, and keymap tables read from Lua after init.lua runs.
//!
//! Rust snapshots these tables once at boot; the renderer and input handler
//! consume only the Rust copies.

use std::collections::HashMap;

use ratatui::style::Color;

// ── Config snapshot ─────────────────────────────────────────────────────────

/// Subset of `bone.config` captured after init.lua.
#[derive(Debug, Clone)]
pub struct LuaConfigSnapshot {
    pub approval_mode: Option<String>,
    pub auto_compact_tokens: Option<u64>,
    pub auto_compact_keep_messages: Option<usize>,
    pub status_show: HashMap<String, bool>,
    pub subagent: LuaSubagentConfig,
}

impl Default for LuaConfigSnapshot {
    fn default() -> Self {
        Self {
            approval_mode: None,
            auto_compact_tokens: None,
            auto_compact_keep_messages: None,
            status_show: HashMap::new(),
            subagent: LuaSubagentConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LuaSubagentConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub approval: Option<String>,
}

impl LuaConfigSnapshot {
    /// Build a snapshot from the `bone.config` Lua table (or nil).
    pub fn from_lua_table(_lua: &mlua::Lua, table: &mlua::Table) -> Result<Self, String> {
        let approval_mode: Option<String> = table.get("approval_mode").ok().flatten();
        let auto_compact_tokens: Option<u64> = table.get("auto_compact_tokens").ok().flatten();
        let auto_compact_keep_messages: Option<usize> =
            table.get("auto_compact_keep_messages").ok().flatten();

        let subagent = table
            .get::<Option<mlua::Table>>("subagent")
            .ok()
            .flatten()
            .map(|t| LuaSubagentConfig {
                provider: t.get("provider").ok().flatten(),
                model: t.get("model").ok().flatten(),
                approval: t.get("approval").ok().flatten(),
            })
            .unwrap_or_default();

        let status_show = table
            .get::<Option<mlua::Table>>("status_show")
            .ok()
            .flatten()
            .map(|t| {
                let mut map = HashMap::new();
                for pair in t.pairs::<String, bool>() {
                    if let Ok((k, v)) = pair {
                        map.insert(k, v);
                    }
                }
                map
            })
            .unwrap_or_default();

        Ok(Self {
            approval_mode,
            auto_compact_tokens,
            auto_compact_keep_messages,
            status_show,
            subagent,
        })
    }
}

// ── Theme snapshot ──────────────────────────────────────────────────────────

/// Subset of `bone.theme` captured after init.lua.
#[derive(Debug, Clone)]
pub struct LuaThemeSnapshot {
    pub user_msg: Option<Color>,
    pub user_msg_bg: Option<Color>,
    pub status_text: Option<Color>,
    pub input_border: Option<Color>,
    pub system_msg: Option<Color>,
    pub approval_safe: Option<Color>,
    pub approval_danger: Option<Color>,
    pub tool_call: Option<Color>,
    pub tool_error: Option<Color>,
    pub diff_removed: Option<Color>,
    pub diff_added: Option<Color>,
    pub thinking: Option<Color>,
    pub tab_active: Option<Color>,
}

impl Default for LuaThemeSnapshot {
    fn default() -> Self {
        Self {
            user_msg: None,
            user_msg_bg: None,
            status_text: None,
            input_border: None,
            system_msg: None,
            approval_safe: None,
            approval_danger: None,
            tool_call: None,
            tool_error: None,
            diff_removed: None,
            diff_added: None,
            thinking: None,
            tab_active: None,
        }
    }
}

impl LuaThemeSnapshot {
    /// Build a snapshot from the `bone.theme` Lua table (or nil).
    pub fn from_lua_table(_lua: &mlua::Lua, table: &mlua::Table) -> Result<Self, String> {
        let parse_color = |key: &str| -> Result<Option<Color>, String> {
            let hex: Option<String> = table.get(key).ok().flatten();
            match hex {
                Some(h) => Ok(Some(
                    parse_hex_color(&h).ok_or_else(|| {
                        format!("bone-lua warn: invalid theme color for {key}: #{h}")
                    })?,
                )),
                None => Ok(None),
            }
        };

        Ok(Self {
            user_msg: parse_color("user_msg")?,
            user_msg_bg: parse_color("user_msg_bg")?,
            status_text: parse_color("status_text")?,
            input_border: parse_color("input_border")?,
            system_msg: parse_color("system_msg")?,
            approval_safe: parse_color("approval_safe")?,
            approval_danger: parse_color("approval_danger")?,
            tool_call: parse_color("tool_call")?,
            tool_error: parse_color("tool_error")?,
            diff_removed: parse_color("diff_removed")?,
            diff_added: parse_color("diff_added")?,
            thinking: parse_color("thinking")?,
            tab_active: parse_color("tab_active")?,
        })
    }

    /// Apply this snapshot into a Rust `Theme`, overriding defaults with set values.
    pub fn apply_to(&self, theme: &mut crate::ui::theme::Theme) {
        macro_rules! apply {
            ($field:ident) => {
                if let Some(c) = self.$field {
                    theme.$field = c;
                }
            };
        }
        apply!(user_msg);
        apply!(user_msg_bg);
        apply!(status_text);
        apply!(input_border);
        apply!(system_msg);
        apply!(approval_safe);
        apply!(approval_danger);
        apply!(tool_call);
        apply!(tool_error);
        apply!(diff_removed);
        apply!(diff_added);
        apply!(thinking);
        apply!(tab_active);
    }
}

/// Parse a hex color string like "#RRGGBB" or "RRGGBB" into a ratatui Color.
fn parse_hex_color(s: &str) -> Option<Color> {
    let s = s.trim();
    let s = s.strip_prefix('#').unwrap_or(s);
    let s = s.to_ascii_uppercase();
    // Named colors
    match s.as_str() {
        "WHITE" => return Some(Color::White),
        "BLACK" => return Some(Color::Black),
        "RED" => return Some(Color::Red),
        "GREEN" => return Some(Color::Green),
        "YELLOW" => return Some(Color::Yellow),
        "BLUE" => return Some(Color::Blue),
        "MAGENTA" => return Some(Color::Magenta),
        "CYAN" => return Some(Color::Cyan),
        "GRAY" | "DARKGRAY" => return Some(Color::DarkGray),
        "LIGHTRED" => return Some(Color::LightRed),
        "LIGHTGREEN" => return Some(Color::LightGreen),
        "LIGHTYELLOW" => return Some(Color::LightYellow),
        "LIGHTBLUE" => return Some(Color::LightBlue),
        "LIGHTMAGENTA" => return Some(Color::LightMagenta),
        "LIGHTCYAN" => return Some(Color::LightCyan),
        _ => {}
    }
    // Hex colors: #RRGGBB or RRGGBB
    if s.len() == 6 {
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        return Some(Color::Rgb(r, g, b));
    }
    None
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
