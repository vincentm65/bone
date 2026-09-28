//! The status bar, drawn by the shared renderer exactly like the TUI's from
//! the daemon's resolved settings (`ui.status_show_*`, spinner presets) and
//! the Lua status segments.
use std::collections::HashMap;

use bone_protocol::{Component, TokenStats, ViewModel};
use bone_render::status::{FALLBACK_SPINNER_FRAMES, FALLBACK_SPINNER_SPEED_MS, StatusInfo};
use eframe::egui;
use serde_json::Value;

const STATUS_KEYS: [&str; 9] = [
    "status_show_model",
    "status_show_approval",
    "status_show_tokens_curr",
    "status_show_tokens_in",
    "status_show_tokens_out",
    "status_show_tokens_total",
    "status_show_queue",
    "status_show_spinner",
    "status_show_timer",
];

pub(crate) struct Inputs<'a> {
    pub model: String,
    pub incognito: bool,
    pub approval_danger: bool,
    pub token_stats: TokenStats,
    pub queue_len: usize,
    pub busy: bool,
    /// Elapsed time of the running turn.
    pub turn_elapsed: Option<std::time::Duration>,
    pub settings: Option<&'a Value>,
    pub view: &'a ViewModel,
}

/// The shared status bar's input, resolved from the daemon's settings the way
/// the TUI resolves its config.
pub(crate) fn info(inputs: Inputs<'_>) -> StatusInfo {
    let ui = |key: &str| {
        inputs
            .settings
            .and_then(|settings| settings.pointer(&format!("/ui/{key}")))
    };
    let ui_str = |key: &str| {
        ui(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let ui_u64 = |key: &str| ui(key).and_then(Value::as_u64).unwrap_or(0);
    let status_show: HashMap<String, bool> = STATUS_KEYS
        .iter()
        .map(|key| {
            (
                key.to_string(),
                ui(key).and_then(Value::as_bool).unwrap_or(true),
            )
        })
        .collect();
    let presets = |name: &str| {
        inputs
            .settings
            .and_then(|settings| settings.get(name))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let strings = |value: Option<&Value>| -> Vec<String> {
        value
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (spinner_frames, spinner_speed_ms) = spinner_style(inputs.settings);
    let custom = ui_str("spinner_custom");
    let spinner_texts = if custom.trim().is_empty() {
        let text = ui_str("spinner_text");
        presets("spinner_texts")
            .iter()
            .find(|preset| preset.get("name").and_then(Value::as_str) == Some(text.as_str()))
            .map(|preset| strings(preset.get("phrases")))
            .unwrap_or_default()
    } else {
        custom
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };
    let lua_status = inputs
        .view
        .components
        .iter()
        .flat_map(|component| match component {
            Component::StatusLine { segments, .. } => segments.clone(),
            Component::Float { .. } => Vec::new(),
        })
        .collect();
    StatusInfo {
        model: inputs.model,
        token_stats: inputs.token_stats,
        streaming_completion_tokens: None,
        streaming: inputs.busy,
        approval_label: if inputs.approval_danger {
            "Danger"
        } else {
            "Safe"
        }
        .into(),
        approval_danger: inputs.approval_danger,
        queue_len: inputs.queue_len,
        incognito: inputs.incognito,
        status_show,
        elapsed: inputs.turn_elapsed.map(|elapsed| {
            let secs = elapsed.as_secs();
            format!("{}:{:02}", secs / 60, secs % 60)
        }),
        lua_status,
        spinner_frames,
        spinner_speed_ms,
        spinner_texts,
        spinner_text_rotate: ui("spinner_text_rotate")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        spinner_text_speed_ms: ui_u64("spinner_text_speed"),
        spinner_elapsed_ms: inputs
            .turn_elapsed
            .map_or(0, |elapsed| elapsed.as_millis() as u64),
    }
}

/// A spinner's frames and milliseconds per frame.
pub(crate) type Spinner = (Vec<String>, u64);

/// The daemon-resolved spinner style, falling back to the braille frames so a
/// running turn is never unmarked.
fn spinner_style(settings: Option<&Value>) -> Spinner {
    let ui = |key: &str| settings.and_then(|settings| settings.pointer(&format!("/ui/{key}")));
    let style = ui("spinner_style")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let speed_override = ui("spinner_speed").and_then(Value::as_u64).unwrap_or(0);
    let preset = settings
        .and_then(|settings| settings.get("spinner_styles"))
        .and_then(Value::as_array)
        .and_then(|presets| {
            presets
                .iter()
                .find(|preset| preset.get("name").and_then(Value::as_str) == Some(style))
        });
    let mut frames: Vec<String> = preset
        .and_then(|preset| preset.get("frames"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mut speed_ms = preset.map_or(0, |preset| match speed_override {
        0 => preset.get("speed").and_then(Value::as_u64).unwrap_or(0),
        speed => speed,
    });
    if frames.is_empty() {
        frames = FALLBACK_SPINNER_FRAMES
            .iter()
            .map(|s| s.to_string())
            .collect();
        if speed_ms == 0 {
            speed_ms = FALLBACK_SPINNER_SPEED_MS;
        }
    }
    (frames, speed_ms)
}

/// The spinner for tabs and history rows, which have room for one glyph: the
/// configured style when every frame is a single character, else braille.
pub(crate) fn indicator_style(settings: Option<&Value>) -> Spinner {
    let (frames, speed_ms) = spinner_style(settings);
    if frames.iter().all(|frame| frame.chars().count() == 1) {
        (frames, speed_ms)
    } else {
        (
            FALLBACK_SPINNER_FRAMES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            FALLBACK_SPINNER_SPEED_MS,
        )
    }
}

/// Paint the current spinner frame centred on `center` and keep repainting.
pub(crate) fn paint_indicator(
    ui: &egui::Ui,
    center: egui::Pos2,
    color: egui::Color32,
    (frames, speed_ms): &Spinner,
) {
    let speed_ms = (*speed_ms).max(16);
    let tick = (ui.input(|input| input.time) * 1000.0) as u64 / speed_ms;
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        &frames[tick as usize % frames.len()],
        egui::TextStyle::Monospace.resolve(ui.style()),
        color,
    );
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(speed_ms));
}

/// Draw the status bar as one terminal row. Touch screens on narrow layouts
/// use the compact renderer so safety/activity indicators win over token detail.
pub(crate) fn show(
    ui: &mut egui::Ui,
    info: &StatusInfo,
    theme: &bone_render::theme::Theme,
    compact: bool,
) {
    let metrics = crate::grid::metrics(ui);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), metrics.row_height),
        egui::Sense::hover(),
    );
    let _ = crate::grid::paint_screen(ui, rect, |frame| {
        let area = frame.area();
        if compact {
            bone_render::status::draw_status_bar_compact(frame, info, theme, area);
        } else {
            bone_render::status::draw_status_bar(frame, info, theme, area);
        }
    });
    if info.streaming {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(
                info.spinner_speed_ms.max(16),
            ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_gate_segments_and_resolve_spinner_presets_like_the_tui() {
        let settings = serde_json::json!({
            "ui": {"status_show_model": false, "spinner_style": "dots", "spinner_text": "calm"},
            "spinner_styles": [{"name": "dots", "speed": 120, "frames": [".", ".."]}],
            "spinner_texts": [{"name": "calm", "phrases": ["pondering"]}],
        });
        let view = ViewModel::default();
        let info = info(Inputs {
            model: "qwen".into(),
            incognito: false,
            approval_danger: true,
            token_stats: TokenStats::default(),
            queue_len: 0,
            busy: true,
            turn_elapsed: Some(std::time::Duration::from_secs(83)),
            settings: Some(&settings),
            view: &view,
        });
        assert!(!info.show("status_show_model"));
        assert!(info.show("status_show_timer"));
        assert_eq!(info.spinner_frames, vec![".".to_string(), "..".to_string()]);
        assert_eq!(info.spinner_speed_ms, 120);
        assert_eq!(info.spinner_texts, vec!["pondering".to_string()]);
        assert_eq!(info.elapsed.as_deref(), Some("1:23"));
        assert_eq!(info.approval_label, "Danger");
    }
}
