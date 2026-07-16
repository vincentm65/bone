//! UI snapshot types — input styling, spinner/text presets.
//!
//! Rust snapshots these once at boot; the renderer and input handler consume
//! only the Rust copies. Theme and keymap come from `BoneSettings` directly.

use crate::config::settings::BoneSettings;

/// Complete daemon-owned settings payload sent to frontends. Persisted settings
/// stay canonical in `config.yaml`; renderer preset definitions are resolved
/// from the daemon's booted UI module and travel in the same snapshot.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ResolvedFrontendSettings {
    #[serde(flatten)]
    pub settings: BoneSettings,
    #[serde(default)]
    pub spinner_styles: Vec<SpinnerPreset>,
    #[serde(default)]
    pub spinner_texts: Vec<TextPreset>,
}

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
            super::ctx::runtime_warn_once(format!(
                "bone-lua warn: {kind} preset missing name; skipping"
            ));
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
            super::ctx::runtime_warn_once(format!(
                "bone-lua warn: spinner preset '{name}' has no frames; skipping"
            ));
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

// ── Input style snapshot ────────────────────────────────────────────────────

/// Optional border-glyph overrides for `bone.config.ui.input`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct InputBorderSnapshot {
    pub horizontal: Option<String>,
    pub vertical: Option<String>,
    pub top_left: Option<String>,
    pub top_right: Option<String>,
    pub bottom_left: Option<String>,
    pub bottom_right: Option<String>,
}

/// Declarative input-composer style from `bone.config.ui.input`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct InputStyleSnapshot {
    pub preset: Option<String>,
    pub prefix: Option<String>,
    pub show_prefix: Option<bool>,
    pub horizontal_padding: Option<u16>,
    pub vertical_padding: Option<u16>,
    pub fill: Option<bool>,
    #[serde(default)]
    pub border: InputBorderSnapshot,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_style_defaults() {
        let snapshot = InputStyleSnapshot::default();
        assert!(snapshot.preset.is_none());
        assert!(snapshot.prefix.is_none());
        assert!(snapshot.border.horizontal.is_none());
    }
}
