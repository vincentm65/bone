//! Lua snapshot types — config, theme, and keymap tables read from Lua after init.lua runs.
//!
//! Rust snapshots these tables once at boot; the renderer and input handler
//! consume only the Rust copies.

use std::collections::HashMap;

// ── Spinner / text presets ──────────────────────────────────────────────────

/// A spinner style preset (frames + natural frame speed).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SpinnerPreset {
    pub name: String,
    /// Milliseconds per frame.
    pub speed: u64,
    pub frames: Vec<String>,
}

/// A rotating thinking-text preset.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TextPreset {
    pub name: String,
    pub phrases: Vec<String>,
}

fn parse_presets<T>(
    table: &mlua::Table,
    kind: &str,
    build: impl Fn(String, &mlua::Table) -> Option<T>,
) -> Vec<T> {
    let mut out = Vec::new();
    for pair in table.pairs::<mlua::Value, mlua::Table>() {
        let Ok((_, t)) = pair else {
            continue;
        };
        let Ok(name) = t.get::<String>("name") else {
            eprintln!("bone-lua warn: {kind} preset missing name; skipping");
            continue;
        };
        if let Some(preset) = build(name, &t) {
            out.push(preset);
        }
    }
    out
}

/// Parse spinner presets, skipping any malformed entry rather than discarding
/// the whole list. A preset needs a `name` and at least one frame to be usable.
fn parse_spinner_presets(table: &mlua::Table) -> Vec<SpinnerPreset> {
    parse_presets(table, "spinner", |name, t| {
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
            return None;
        }
        Some(SpinnerPreset {
            name,
            speed,
            frames,
        })
    })
}

/// Parse rotating-text presets, skipping malformed entries (see
/// [`parse_spinner_presets`]).
fn parse_text_presets(table: &mlua::Table) -> Vec<TextPreset> {
    parse_presets(table, "text", |name, t| {
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
        Some(TextPreset { name, phrases })
    })
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

/// Optional border-glyph overrides for `bone.config.ui.input`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LuaInputBorderSnapshot {
    pub horizontal: Option<String>,
    pub vertical: Option<String>,
    pub top_left: Option<String>,
    pub top_right: Option<String>,
    pub bottom_left: Option<String>,
    pub bottom_right: Option<String>,
}

/// Declarative input-composer style from `bone.config.ui.input`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LuaInputStyleSnapshot {
    pub preset: Option<String>,
    pub prefix: Option<String>,
    pub show_prefix: Option<bool>,
    pub horizontal_padding: Option<u16>,
    pub vertical_padding: Option<u16>,
    pub fill: Option<bool>,
    #[serde(default)]
    pub border: LuaInputBorderSnapshot,
}

/// Subset of `bone.config` captured after init.lua.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LuaConfigSnapshot {
    pub approval_mode: Option<String>,
    pub status_show: HashMap<String, bool>,
    #[serde(default)]
    pub input: LuaInputStyleSnapshot,
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

        let input = table
            .get::<Option<mlua::Table>>("ui")
            .ok()
            .flatten()
            .and_then(|ui| ui.get::<Option<mlua::Table>>("input").ok().flatten())
            .map(|input| {
                let border = input
                    .get::<Option<mlua::Table>>("border")
                    .ok()
                    .flatten()
                    .map(|border| LuaInputBorderSnapshot {
                        horizontal: border.get("horizontal").ok().flatten(),
                        vertical: border.get("vertical").ok().flatten(),
                        top_left: border.get("top_left").ok().flatten(),
                        top_right: border.get("top_right").ok().flatten(),
                        bottom_left: border.get("bottom_left").ok().flatten(),
                        bottom_right: border.get("bottom_right").ok().flatten(),
                    })
                    .unwrap_or_default();
                LuaInputStyleSnapshot {
                    preset: input.get("preset").ok().flatten(),
                    prefix: input.get("prefix").ok().flatten(),
                    show_prefix: input.get("show_prefix").ok().flatten(),
                    horizontal_padding: input.get("horizontal_padding").ok().flatten(),
                    vertical_padding: input.get("vertical_padding").ok().flatten(),
                    fill: input.get("fill").ok().flatten(),
                    border,
                }
            })
            .unwrap_or_default();

        Ok(Self {
            approval_mode,
            status_show,
            input,
            spinners: Vec::new(),
            texts: Vec::new(),
        })
    }
}

// ── Theme snapshot ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LuaThemePaletteSnapshot {
    pub bg: Option<String>,
    pub fg: Option<String>,
    pub muted: Option<String>,
    pub subtle: Option<String>,
    pub border: Option<String>,
    pub accent: Option<String>,
    pub good: Option<String>,
    pub warn: Option<String>,
    pub error: Option<String>,
    pub selection: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LuaThemeShellSnapshot {
    pub program: Option<String>,
    pub separator: Option<String>,
    pub redirect: Option<String>,
    pub flag: Option<String>,
    pub string: Option<String>,
    pub variable: Option<String>,
    pub comment: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LuaThemeSyntaxSnapshot {
    pub text: Option<String>,
    pub comment: Option<String>,
    pub string: Option<String>,
    pub number: Option<String>,
    pub constant: Option<String>,
    pub escape: Option<String>,
    pub regex: Option<String>,
    pub keyword: Option<String>,
    pub keyword_control: Option<String>,
    pub r#type: Option<String>,
    pub function_name: Option<String>,
    pub variable: Option<String>,
    pub tag: Option<String>,
    pub attribute: Option<String>,
    pub punctuation: Option<String>,
    pub subtle: Option<String>,
    pub markup: Option<String>,
    pub invalid: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum LuaStyleSpec {
    Color(String),
    Style {
        fg: Option<String>,
        bg: Option<String>,
        bold: Option<bool>,
        italic: Option<bool>,
        underline: Option<bool>,
    },
}

/// Subset of `bone.theme` captured after init.lua.
///
/// Colors are stored as raw strings; parsing into `ratatui::style::Color`
/// happens at the UI boundary in `Theme::apply_snapshot`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LuaThemeSnapshot {
    #[serde(default)]
    pub palette: LuaThemePaletteSnapshot,
    #[serde(default)]
    pub shell: LuaThemeShellSnapshot,
    #[serde(default)]
    pub syntax: LuaThemeSyntaxSnapshot,
    #[serde(default)]
    pub highlights: std::collections::BTreeMap<String, LuaStyleSpec>,

    pub user_msg: Option<String>,
    pub user_msg_bg: Option<String>,
    pub status_text: Option<String>,
    pub input_border: Option<String>,
    pub system_msg: Option<String>,
    pub approval_safe: Option<String>,
    pub approval_danger: Option<String>,
    pub tool_call: Option<String>,
    pub tool_error: Option<String>,
    pub shell_program: Option<String>,
    pub shell_separator: Option<String>,
    pub shell_redirect: Option<String>,
    pub shell_flag: Option<String>,
    pub shell_string: Option<String>,
    pub shell_variable: Option<String>,
    pub shell_comment: Option<String>,
    pub shell_path: Option<String>,
    pub diff_removed: Option<String>,
    pub diff_added: Option<String>,
    pub thinking: Option<String>,
    pub tab_active: Option<String>,
    pub syntax_text: Option<String>,
    pub syntax_comment: Option<String>,
    pub syntax_string: Option<String>,
    pub syntax_number: Option<String>,
    pub syntax_constant: Option<String>,
    pub syntax_escape: Option<String>,
    pub syntax_regex: Option<String>,
    pub syntax_keyword: Option<String>,
    pub syntax_keyword_control: Option<String>,
    pub syntax_type: Option<String>,
    pub syntax_function: Option<String>,
    pub syntax_variable: Option<String>,
    pub syntax_tag: Option<String>,
    pub syntax_attribute: Option<String>,
    pub syntax_punctuation: Option<String>,
    pub syntax_subtle: Option<String>,
    pub syntax_markup: Option<String>,
    pub syntax_invalid: Option<String>,
}

impl LuaThemeSnapshot {
    /// Build a snapshot from the `bone.theme` Lua table (or nil).
    pub fn from_lua_table(_lua: &mlua::Lua, table: &mlua::Table) -> Result<Self, String> {
        let get_color = |key: &str| -> Option<String> {
            let hex: Option<String> = table.get(key).ok().flatten();
            hex
        };

        let get_table = |key: &str| -> Option<mlua::Table> { table.get(key).ok().flatten() };
        let table_color = |t: &Option<mlua::Table>, key: &str| -> Option<String> {
            t.as_ref().and_then(|t| t.get(key).ok().flatten())
        };
        let palette_table = get_table("palette");
        let shell_table = get_table("shell");
        let syntax_table = get_table("syntax");
        let highlights_table = get_table("highlights");
        let mut highlights = std::collections::BTreeMap::new();
        if let Some(t) = highlights_table {
            for pair in t.pairs::<String, mlua::Value>() {
                match pair {
                    Ok((name, mlua::Value::String(s))) => {
                        if let Ok(s) = s.to_str() {
                            highlights.insert(name, LuaStyleSpec::Color(s.to_string()));
                        }
                    }
                    Ok((name, mlua::Value::Table(t))) => {
                        let fg = t.get("fg").ok().flatten();
                        let bg = t.get("bg").ok().flatten();
                        let bold = t.get("bold").ok().flatten();
                        let italic = t.get("italic").ok().flatten();
                        let underline = t.get("underline").ok().flatten();
                        highlights.insert(
                            name,
                            LuaStyleSpec::Style {
                                fg,
                                bg,
                                bold,
                                italic,
                                underline,
                            },
                        );
                    }
                    Ok((name, _)) => eprintln!("bone-lua warn: invalid theme highlight: {name}"),
                    Err(e) => eprintln!("bone-lua warn: invalid theme highlight: {e}"),
                }
            }
        }

        Ok(Self {
            palette: LuaThemePaletteSnapshot {
                bg: table_color(&palette_table, "bg"),
                fg: table_color(&palette_table, "fg"),
                muted: table_color(&palette_table, "muted"),
                subtle: table_color(&palette_table, "subtle"),
                border: table_color(&palette_table, "border"),
                accent: table_color(&palette_table, "accent"),
                good: table_color(&palette_table, "good"),
                warn: table_color(&palette_table, "warn"),
                error: table_color(&palette_table, "error"),
                selection: table_color(&palette_table, "selection"),
            },
            shell: LuaThemeShellSnapshot {
                program: table_color(&shell_table, "program"),
                separator: table_color(&shell_table, "separator"),
                redirect: table_color(&shell_table, "redirect"),
                flag: table_color(&shell_table, "flag"),
                string: table_color(&shell_table, "string"),
                variable: table_color(&shell_table, "variable"),
                comment: table_color(&shell_table, "comment"),
                path: table_color(&shell_table, "path"),
            },
            syntax: LuaThemeSyntaxSnapshot {
                text: table_color(&syntax_table, "text"),
                comment: table_color(&syntax_table, "comment"),
                string: table_color(&syntax_table, "string"),
                number: table_color(&syntax_table, "number"),
                constant: table_color(&syntax_table, "constant"),
                escape: table_color(&syntax_table, "escape"),
                regex: table_color(&syntax_table, "regex"),
                keyword: table_color(&syntax_table, "keyword"),
                keyword_control: table_color(&syntax_table, "keyword_control"),
                r#type: table_color(&syntax_table, "type"),
                function_name: table_color(&syntax_table, "function_name"),
                variable: table_color(&syntax_table, "variable"),
                tag: table_color(&syntax_table, "tag"),
                attribute: table_color(&syntax_table, "attribute"),
                punctuation: table_color(&syntax_table, "punctuation"),
                subtle: table_color(&syntax_table, "subtle"),
                markup: table_color(&syntax_table, "markup"),
                invalid: table_color(&syntax_table, "invalid"),
            },
            highlights,
            user_msg: get_color("user_msg"),
            user_msg_bg: get_color("user_msg_bg"),
            status_text: get_color("status_text"),
            input_border: get_color("input_border"),
            system_msg: get_color("system_msg"),
            approval_safe: get_color("approval_safe"),
            approval_danger: get_color("approval_danger"),
            tool_call: get_color("tool_call"),
            tool_error: get_color("tool_error"),
            shell_program: get_color("shell_program"),
            shell_separator: get_color("shell_separator"),
            shell_redirect: get_color("shell_redirect"),
            shell_flag: get_color("shell_flag"),
            shell_string: get_color("shell_string"),
            shell_variable: get_color("shell_variable"),
            shell_comment: get_color("shell_comment"),
            shell_path: get_color("shell_path"),
            diff_removed: get_color("diff_removed"),
            diff_added: get_color("diff_added"),
            thinking: get_color("thinking"),
            tab_active: get_color("tab_active"),
            syntax_text: get_color("syntax_text"),
            syntax_comment: get_color("syntax_comment"),
            syntax_string: get_color("syntax_string"),
            syntax_number: get_color("syntax_number"),
            syntax_constant: get_color("syntax_constant"),
            syntax_escape: get_color("syntax_escape"),
            syntax_regex: get_color("syntax_regex"),
            syntax_keyword: get_color("syntax_keyword"),
            syntax_keyword_control: get_color("syntax_keyword_control"),
            syntax_type: get_color("syntax_type"),
            syntax_function: get_color("syntax_function"),
            syntax_variable: get_color("syntax_variable"),
            syntax_tag: get_color("syntax_tag"),
            syntax_attribute: get_color("syntax_attribute"),
            syntax_punctuation: get_color("syntax_punctuation"),
            syntax_subtle: get_color("syntax_subtle"),
            syntax_markup: get_color("syntax_markup"),
            syntax_invalid: get_color("syntax_invalid"),
        })
    }
}

// ── Keymap snapshot ─────────────────────────────────────────────────────────

/// A single key binding: key string → action name.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LuaKeyBinding {
    pub key: String,
    pub action: String,
}

/// Snapshot of `bone.keymap` after init.lua.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_snapshot_parses_input_style() {
        let lua = mlua::Lua::new();
        let config: mlua::Table = lua
            .load(
                r#"
                return {
                    ui = {
                        input = {
                            preset = "box",
                            prefix = "λ ",
                            show_prefix = true,
                            horizontal_padding = 2,
                            vertical_padding = 1,
                            fill = true,
                            border = {
                                horizontal = "-", vertical = "|",
                                top_left = "+", top_right = "+",
                                bottom_left = "[", bottom_right = "]",
                            },
                        },
                    },
                }
                "#,
            )
            .eval()
            .unwrap();

        let snapshot = LuaConfigSnapshot::from_lua_table(&lua, &config).unwrap();
        assert_eq!(snapshot.input.preset.as_deref(), Some("box"));
        assert_eq!(snapshot.input.prefix.as_deref(), Some("λ "));
        assert_eq!(snapshot.input.show_prefix, Some(true));
        assert_eq!(snapshot.input.horizontal_padding, Some(2));
        assert_eq!(snapshot.input.vertical_padding, Some(1));
        assert_eq!(snapshot.input.fill, Some(true));
        assert_eq!(snapshot.input.border.horizontal.as_deref(), Some("-"));
        assert_eq!(snapshot.input.border.vertical.as_deref(), Some("|"));
        assert_eq!(snapshot.input.border.bottom_left.as_deref(), Some("["));
        assert_eq!(snapshot.input.border.bottom_right.as_deref(), Some("]"));
    }

    #[test]
    fn config_snapshot_defaults_when_input_style_is_absent() {
        let lua = mlua::Lua::new();
        let config = lua.create_table().unwrap();
        let snapshot = LuaConfigSnapshot::from_lua_table(&lua, &config).unwrap();

        assert!(snapshot.input.preset.is_none());
        assert!(snapshot.input.prefix.is_none());
        assert!(snapshot.input.border.horizontal.is_none());
    }
}
