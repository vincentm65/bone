//! Native renderer for the daemon-owned declarative view projection.
//!
//! The daemon broadcasts a [`ViewModel`] (and live `ViewDiff`s) describing
//! Lua-authored panes: `Component::Float` windows, `Component::StatusLine` bars,
//! and named highlight colors. This module maps those wire components onto egui
//! without changing the protocol: floats become anchored overlays, status
//! segments become a colored row, and `fg` references resolve through the
//! mirrored theme palette and the view's highlight map.
//!
//! Character-cell metrics (`FloatRect.width`/`height`, `col`/`row`) are terminal
//! units, so they are approximated to points here; native has no fixed cell
//! grid.

use std::collections::HashMap;

use bone_protocol::view::PanelSlot;
use bone_protocol::{Align, Anchor, Component, PaneLineSpec, StatusSegment, ViewModel};
use eframe::egui;

use crate::theme::{self, Palette};
use crate::workspace::PanelLayout;

/// Approximate point width of one terminal character cell.
const CHAR_WIDTH: f32 = 7.0;
/// Approximate point height of one terminal character cell.
const ROW_HEIGHT: f32 = 18.0;

/// Resolve a color reference to an egui color. Recognizes `#rgb`/`#rrggbb`/
/// `#rrggbbaa`, a named palette role (`fg`, `accent`, …), or a view highlight
/// name whose value is itself a color string. Returns `None` so callers keep the
/// surrounding color when the reference is unknown.
pub fn resolve_color(
    value: &str,
    highlights: &HashMap<String, String>,
    palette: &Palette,
) -> Option<egui::Color32> {
    if let Some(hex) = highlights.get(value)
        && let Some(color) = theme::parse_color(hex)
    {
        return Some(color);
    }
    theme::resolve_color(value, palette)
}

/// Render `PaneLineSpec` rows into the current layout. `Plain` lines become a
/// plain label; `Spans` lines become a row of colored, styled fragments. Each
/// spec is its own row, matching the daemon's line-per-spec content.
pub fn render_lines(
    ui: &mut egui::Ui,
    lines: &[PaneLineSpec],
    highlights: &HashMap<String, String>,
    palette: &Palette,
) {
    for line in lines {
        match line {
            PaneLineSpec::Plain(text) => {
                ui.label(native_pane_text(text));
            }
            PaneLineSpec::Spans { spans, bg } => {
                let bg_color = bg
                    .as_deref()
                    .and_then(|value| resolve_color(value, highlights, palette));
                ui.horizontal_wrapped(|ui| {
                    if spans.is_empty() {
                        ui.label("");
                        return;
                    }
                    for span in spans {
                        let mut rich = egui::RichText::new(native_pane_text(&span.text));
                        if let Some(fg) = span
                            .fg
                            .as_deref()
                            .and_then(|value| resolve_color(value, highlights, palette))
                        {
                            rich = rich.color(fg);
                        }
                        if let Some(bg) = bg_color {
                            rich = rich.background_color(bg);
                        }
                        for modifier in &span.modifiers {
                            rich = match modifier.as_str() {
                                "bold" => rich.strong(),
                                "dim" => rich.weak(),
                                "italic" => rich.italics(),
                                "strike" | "crossed_out" => rich.strikethrough(),
                                _ => rich,
                            };
                        }
                        ui.label(rich);
                    }
                });
            }
        }
    }
}

/// Estimate the height (in points) that [`render_lines`] will occupy when laid
/// out at `width`, mirroring its one-row-per-spec structure: `Spans` rows are
/// measured as a single wrapped run of their concatenated text, which matches
/// the common single-segment case and errs high for exotic multi-segment rows.
/// `Spans` rows carry the [`egui::Spacing::interact_size`] floor that
/// `ui.horizontal_wrapped` reserves; `Plain` rows do not. Returns `0.0` for an
/// empty slice.
///
/// Callers use this to pre-size a host container so an auto-sized container
/// (e.g. `egui::Modal`'s area, which latches onto the measured content height)
/// cannot settle on a too-small placeholder height and clip later content.
pub fn measure_lines(ui: &egui::Ui, lines: &[PaneLineSpec], width: f32) -> f32 {
    if lines.is_empty() {
        return 0.0;
    }
    let font_id = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Body)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(14.0));
    let spacing = ui.spacing().item_spacing.y;
    // `Spans` rows render through `ui.horizontal_wrapped`, which reserves
    // `interact_size.y` as a floor for the row regardless of its text
    // (egui: "Assume there will be something interactive on the horizontal
    // layout"); `Plain` rows are bare labels and take only their text height.
    // Mirror that floor here or the estimate under-reports every styled row.
    let wrapped_floor = ui.spacing().interact_size.y;
    let width = width.max(1.0);
    let mut height = 0.0;
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            height += spacing;
        }
        let text: String = match line {
            PaneLineSpec::Plain(text) => native_pane_text(text).into_owned(),
            PaneLineSpec::Spans { spans, .. } => spans
                .iter()
                .map(|span| native_pane_text(&span.text).into_owned())
                .collect(),
        };
        let row = if text.is_empty() {
            // A label with no text still occupies one row.
            ui.text_style_height(&egui::TextStyle::Body)
        } else {
            ui.fonts_mut(|fonts| fonts.layout(text, font_id.clone(), egui::Color32::WHITE, width))
                .size()
                .y
        };
        height += match line {
            PaneLineSpec::Spans { .. } => row.max(wrapped_floor),
            PaneLineSpec::Plain(_) => row,
        };
    }
    height
}

/// Terminal progress glyphs are absent from the bundled proportional fonts.
/// Keep their filled-marker meaning without showing a missing-glyph box.
fn native_pane_text(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(['◐', '◑']) {
        text.replace(['◐', '◑'], "•").into()
    } else {
        text.into()
    }
}

/// Render a status line as a single row: left segments first, then centered
/// segments, then right-aligned segments, each with its `fg` color.
pub fn render_status_line(
    ui: &mut egui::Ui,
    segments: &[StatusSegment],
    highlights: &HashMap<String, String>,
    palette: &Palette,
) {
    if segments.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        for segment in segments.iter().filter(|s| s.align == Align::Left) {
            render_segment(ui, segment, highlights, palette);
        }
        if segments.iter().any(|s| s.align == Align::Center) {
            ui.with_layout(
                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                |ui| {
                    for segment in segments.iter().filter(|s| s.align == Align::Center) {
                        render_segment(ui, segment, highlights, palette);
                    }
                },
            );
        }
        if segments.iter().any(|s| s.align == Align::Right) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // right_to_left lays out in reverse, so iterate reversed to
                // preserve the daemon's left-to-right segment order.
                for segment in segments.iter().filter(|s| s.align == Align::Right).rev() {
                    render_segment(ui, segment, highlights, palette);
                }
            });
        }
    });
}

fn render_segment(
    ui: &mut egui::Ui,
    segment: &StatusSegment,
    highlights: &HashMap<String, String>,
    palette: &Palette,
) {
    let mut rich = egui::RichText::new(segment.text.as_str());
    if let Some(fg) = segment
        .fg
        .as_deref()
        .and_then(|value| resolve_color(value, highlights, palette))
    {
        rich = rich.color(fg);
    }
    ui.label(rich);
}

/// Render every `Component::Float` in `view` as an anchored overlay. `salt`
/// disambiguates identical component ids across tabs. `scroll` rows are skipped
/// from the top, matching the daemon's pane scroll offset, plus any frontend
/// `scrolls` offset (keyed by float id) from keyboard scrolling. `active`
/// outlines the keyboard-focused float. Renders nothing when `visible` is false.
pub fn render_floats(
    ctx: &egui::Context,
    salt: impl std::hash::Hash + std::fmt::Debug,
    view: &ViewModel,
    palette: &Palette,
    visible: bool,
    active: Option<&str>,
    scrolls: &HashMap<String, i64>,
    panel_layout: Option<&PanelLayout>,
) {
    if !visible {
        return;
    }
    let (_, _, accent) = palette.resolved();
    let screen = ctx.content_rect();
    for component in &view.components {
        let Component::Float {
            id,
            title,
            lines,
            rect,
            border,
            scroll,
            placement,
            ..
        } = component
        else {
            continue;
        };
        let effective_slot = panel_layout
            .and_then(|layout| layout.entry(id).map(|entry| entry.slot))
            .or_else(|| placement.as_ref().map(|placement| placement.slot))
            .unwrap_or(PanelSlot::Overlay);
        if effective_slot != PanelSlot::Overlay {
            continue;
        }
        let pivot = egui_anchor(rect.anchor);
        let anchor_point = pivot.pos_in_rect(&screen);
        let offset = egui::vec2(rect.col as f32 * CHAR_WIDTH, rect.row as f32 * ROW_HEIGHT);
        let mut frame = if *border {
            egui::Frame::window(&ctx.style_of(ctx.theme()))
        } else {
            egui::Frame::NONE
        };
        if active == Some(id.as_str()) {
            frame = frame.stroke(egui::Stroke::new(1.0, accent));
        }
        let client = scrolls.get(id).copied().unwrap_or(0);
        let start = ((*scroll as i64 + client).max(0) as usize).min(lines.len());
        let render = |ui: &mut egui::Ui| {
            if rect.width > 0 {
                ui.set_max_width(rect.width as f32 * CHAR_WIDTH);
            }
            if !title.is_empty() {
                ui.strong(title);
            }
            let mut scroll_area = egui::ScrollArea::vertical();
            if rect.height > 0 {
                scroll_area = scroll_area.max_height(rect.height as f32 * ROW_HEIGHT);
            }
            scroll_area.show(ui, |ui| {
                render_lines(ui, &lines[start..], &view.highlights, palette);
            });
        };
        if placement.is_some() || panel_layout.is_some_and(|layout| layout.entry(id).is_some()) {
            // Explicit overlay panels are native floating windows: egui supplies
            // dragging, resizing, focus stacking, and a stable per-panel state.
            // Legacy floats retain their anchored Area behavior below.
            egui::Window::new(if title.is_empty() { id } else { title })
                .id(egui::Id::new(("bone-floating-panel", &salt, id)))
                .frame(frame)
                .default_pos(anchor_point + offset)
                .default_size(egui::vec2(
                    (rect.width.max(1) as f32) * CHAR_WIDTH,
                    (rect.height.max(1) as f32) * ROW_HEIGHT,
                ))
                .resizable(true)
                .movable(true)
                .show(ctx, render);
        } else {
            egui::Area::new(egui::Id::new(("bone-float", &salt, id)))
                .order(egui::Order::Middle)
                .fixed_pos(anchor_point + offset)
                .pivot(pivot)
                .show(ctx, |ui| {
                    frame.show(ui, render);
                });
        }
    }
}

/// Map a wire anchor onto the egui alignment used for overlay placement.
fn egui_anchor(anchor: Anchor) -> egui::Align2 {
    match anchor {
        Anchor::TopLeft => egui::Align2::LEFT_TOP,
        Anchor::TopRight => egui::Align2::RIGHT_TOP,
        Anchor::BottomLeft => egui::Align2::LEFT_BOTTOM,
        Anchor::BottomRight => egui::Align2::RIGHT_BOTTOM,
        Anchor::Center => egui::Align2::CENTER_CENTER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        serde_json::from_value(serde_json::json!({
            "bg": "#101014",
            "fg": "#e0e0e0",
            "accent": "#4f9cf9",
            "error": "#ff5555"
        }))
        .unwrap()
    }

    #[test]
    fn resolve_color_prefers_highlight_then_palette_then_hex() {
        let mut highlights = HashMap::new();
        highlights.insert("assistant".to_string(), "#8be9fd".to_string());
        let palette = palette();
        // Highlight name wins.
        assert_eq!(
            resolve_color("assistant", &highlights, &palette),
            Some(egui::Color32::from_rgb(0x8b, 0xe9, 0xfd))
        );
        // Palette role.
        assert_eq!(
            resolve_color("accent", &highlights, &palette),
            Some(egui::Color32::from_rgb(0x4f, 0x9c, 0xf9))
        );
        // Explicit hex.
        assert_eq!(
            resolve_color("#112233", &highlights, &palette),
            Some(egui::Color32::from_rgb(0x11, 0x22, 0x33))
        );
        // Unknown reference stays unset so the caller keeps its color.
        assert_eq!(resolve_color("nope", &highlights, &palette), None);
    }

    #[test]
    fn render_view_components_is_panic_free() {
        // Smoke test: drive a headless egui pass through every renderer with a
        // float (border + scroll), a spans line with modifiers, and a status
        // line with all three alignments.
        let view = ViewModel {
            components: vec![
                Component::Float {
                    presentation: bone_protocol::PanePresentation::Overlay,
                    id: "pane".into(),
                    title: "Jobs".into(),
                    lines: vec![
                        PaneLineSpec::Plain("plain".into()),
                        PaneLineSpec::Spans {
                            spans: vec![
                                bone_protocol::PaneSpanSpec {
                                    text: "bold".into(),
                                    fg: Some("accent".into()),
                                    modifiers: vec!["bold".into(), "italic".into()],
                                },
                                bone_protocol::PaneSpanSpec {
                                    text: " plain".into(),
                                    fg: None,
                                    modifiers: vec![],
                                },
                            ],
                            bg: Some("selection".into()),
                        },
                    ],
                    rect: bone_protocol::FloatRect {
                        anchor: Anchor::TopRight,
                        width: 40,
                        height: 6,
                        col: -2,
                        row: 1,
                    },
                    z: 0,
                    border: true,
                    scroll: 1,
                    placement: None,
                    owner: None,
                },
                Component::StatusLine {
                    id: "status".into(),
                    segments: vec![
                        StatusSegment {
                            text: "left".into(),
                            fg: Some("fg".into()),
                            align: Align::Left,
                        },
                        StatusSegment {
                            text: "mid".into(),
                            fg: None,
                            align: Align::Center,
                        },
                        StatusSegment {
                            text: "right".into(),
                            fg: Some("#ff5555".into()),
                            align: Align::Right,
                        },
                    ],
                },
            ],
            highlights: HashMap::new(),
        };
        let palette = palette();
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            for component in &view.components {
                if let Component::StatusLine { segments, .. } = component {
                    render_status_line(ui, segments, &view.highlights, &palette);
                }
            }
            render_lines(
                ui,
                &view.components[0].as_pane_content().unwrap().lines,
                &view.highlights,
                &palette,
            );
            render_floats(
                ui.ctx(),
                7u64,
                &view,
                &palette,
                true,
                None,
                &HashMap::new(),
                None,
            );
        })
        .textures_delta
        .clear();
    }
}
