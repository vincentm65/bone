//! Variable-height transcript virtualization with a bounded parse cache.
//!
//! A conversation history grows without bound, so the Stage 2 approach of
//! re-parsing and laying out every row on every frame is O(rows) even when the
//! visible window is small. This module keeps a per-row measured height and
//! parses/lays out only rows near the visible band (`OVERSCAN`), using
//! [`egui::ScrollArea::show_viewport`]. Heights persist across frames so warm
//! frames re-lay-out just the band; width/zoom changes invalidate every
//! measurement (a reflow) but keep the parsed blocks, which are width-
//! independent. Parsed rows are evicted outside a slack margin around the
//! band, so memory stays bounded too.
//!
//! Row rendering:
//! - Tool rows are **preformatted**: the raw output is laid out monospace
//!   with newlines preserved (no Markdown interpretation), so multi-line
//!   logs survive verbatim. Their output and args are collapsible, and the
//!   Copy buttons always copy the *full* text.
//! - Large preformatted content is **chunked**: at most [`CHUNK_LINES`]
//!   lines are laid out per chunk, so a 100k-line output never becomes one
//!   unbounded label; "show more lines" grows the row in bounded steps and
//!   the row is re-measured after each change.
//! - User/assistant/reasoning rows use Markdown with a role hierarchy: users
//!   are accented with the theme link color, reasoning is demoted to small
//!   dim italics, and headings are selectable.
//!
//! Layout notes (egui 0.36):
//! - A row's real height is measured by scoping it into a child ui whose
//!   `max_rect` starts at the row's `content_y`. `region_from_max_rect` seeds
//!   `min_rect` at a point at the first widget position, so the scope's
//!   `min_rect` grows to exactly the placed content regardless of how large
//!   the measurement bound is.
//! - `ui.set_height(total)` (min + max) keeps the scroll content's reported
//!   size equal to the full transcript while only the band is actually drawn,
//!   which preserves the caller's at-bottom math and stick-to-bottom behavior.
use crate::markdown;
use crate::state::ToolCard;
use eframe::egui;
use egui::{Rect, Ui, UiBuilder};

/// Layout pixels rendered above and below the visible viewport so fast scrolls
/// have content before the next repaint.
const OVERSCAN: f32 = 800.0;
/// Rows of parsed slack kept on each side of the built band before eviction.
const PARSE_SLACK: usize = 400;
/// Vertical bound used while measuring an unknown row. Only has to exceed any
/// conceivable row height; the actual measured height comes from the placed
/// content.
const MEASURE_BOUND: f32 = 1.0e7;
/// Output lines laid out per chunk of an expanded tool row. Keeping this
/// bounded caps the tallest row height regardless of output size.
pub const CHUNK_LINES: usize = 200;
/// Characters of the first line shown while a tool row is collapsed.
const COLLAPSED_PREVIEW_CHARS: usize = 160;

/// Per-tab virtualization state, owned by [`crate::Tab`]. One per conversation,
/// kept alive across reconnects via [`Self::reset`].
pub(crate) struct Cache {
    /// Content width the cached measurements were taken at.
    width: f32,
    /// Zoom factor the cached measurements were taken at.
    zoom: f32,
    /// Per-row measured height in points; `0.0` means "not measured yet".
    pub(crate) heights: Vec<f32>,
    /// Per-row parsed markdown; `None` means "not parsed yet or evicted".
    /// Tool rows are preformatted and never occupy one.
    parsed: Vec<Option<Vec<markdown::Block>>>,
    /// Per-row tool output/args expansion; `true` (expanded) by default.
    expanded: Vec<bool>,
    /// Per-row number of output chunks shown while expanded; 1 by default.
    chunks: Vec<usize>,
    /// Number of rows in `heights` that are still `0.0` (unmeasured).
    unknown: usize,
    /// Rows actually laid out during the most recent frame (test/measurement
    /// instrumentation; not read outside `#[cfg(test)]` code).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) last_built: usize,
    /// Markdown parses performed through `layout_rows` (test instrumentation).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) parse_calls: usize,
}

impl Cache {
    pub(crate) fn new() -> Self {
        Self {
            width: 0.0,
            zoom: 0.0,
            heights: Vec::new(),
            parsed: Vec::new(),
            expanded: Vec::new(),
            chunks: Vec::new(),
            unknown: 0,
            last_built: 0,
            parse_calls: 0,
        }
    }

    /// Drop every measurement and parse. Called on (re)attach when the
    /// conversation is about to be replayed wholesale; the stale width/zoom
    /// force a full reflow on the next layout.
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    /// Reconcile the cache with the authoritative row vector.
    ///
    /// `changed_rows` holds every index whose content changed since the last
    /// sync (recorded by [`crate::state::State`] and drained once per frame by
    /// the caller); `rows_len` is the current row count. The caller also calls
    /// this from [`Self::show`] whenever the row count alone changed.
    pub(crate) fn sync(&mut self, rows_len: usize, changed: &[usize]) {
        let old_len = self.heights.len();
        if rows_len > old_len {
            // Appended rows arrive unmeasured.
            self.unknown += rows_len - old_len;
        } else if rows_len < old_len {
            // Rows removed from the tail: forget the unmeasured ones among them
            // (measured rows were already accounted for when measured).
            let zeros = self.heights[rows_len..]
                .iter()
                .filter(|h| **h == 0.0)
                .count();
            self.unknown = self.unknown.saturating_sub(zeros);
        }
        for &i in changed {
            if i < self.heights.len() && i < rows_len {
                if self.heights[i] > 0.0 {
                    self.unknown += 1;
                }
                self.heights[i] = 0.0;
                self.parsed[i] = None;
            }
        }
        self.heights.resize(rows_len, 0.0);
        self.parsed.resize(rows_len, None);
        self.expanded.resize(rows_len, true);
        self.chunks.resize(rows_len, 1);
    }

    /// Show the transcript, laying out only the rows near the visible band.
    /// Returns the scroll-area output so the caller can keep its at-bottom
    /// bookkeeping (`content_size` always reflects the full transcript).
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        tab_id: u64,
        stick_to_bottom: bool,
        rows: &[(String, String)],
        toolcards: &[Option<ToolCard>],
    ) -> egui::containers::scroll_area::ScrollAreaOutput<()> {
        if self.heights.len() != rows.len() {
            self.sync(rows.len(), &[]);
        }
        egui::ScrollArea::vertical()
            .id_salt(("transcript", tab_id))
            .stick_to_bottom(stick_to_bottom)
            .show_viewport(ui, |ui, viewport| {
                self.layout_rows(ui, tab_id, viewport, stick_to_bottom, rows, toolcards)
            })
    }

    fn layout_rows(
        &mut self,
        ui: &mut Ui,
        tab_id: u64,
        viewport: Rect,
        stick_to_bottom: bool,
        rows: &[(String, String)],
        toolcards: &[Option<ToolCard>],
    ) {
        let n = rows.len();
        let width = ui.max_rect().width();
        let zoom = ui.ctx().zoom_factor();
        let reflow = (width - self.width).abs() > 0.5 || (zoom - self.zoom).abs() > 0.001;
        if reflow {
            // Measurements are stale at a new width/zoom: rebuild everything
            // this frame, then future frames render only the visible band.
            // Parsed blocks are width-independent, so they are retained and
            // only re-parsed where eviction already cleared them.
            self.heights.fill(0.0);
            self.unknown = n;
        }
        self.width = width;
        self.zoom = zoom;

        let spacing = ui.spacing().item_spacing.y;
        // Full transcript height from cached measurements (unmeasured rows
        // count 0 here and grow the region as they are built this frame).
        let total: f32 = self.heights.iter().sum::<f32>() + spacing * (n.saturating_sub(1)) as f32;
        ui.set_height(total);

        // Viewport coordinates are content-absolute, so the over-scan band is
        // just the viewport grown by OVERSCAN on each side.
        let band_min = (viewport.min.y - OVERSCAN).max(0.0) as f64;
        let band_max = if reflow {
            f64::INFINITY // measure everything (needed after a reflow)
        } else {
            viewport.max.y as f64 + OVERSCAN as f64
        };

        let mut first_built = usize::MAX;
        let mut last_built_idx = 0;
        let mut count = 0;
        let mut content_y = 0.0f64;

        for i in 0..n {
            let height = self.heights[i];
            let known = height > 0.0;
            if content_y > band_max {
                // Everything from here on starts below the over-scan band.
                if !(stick_to_bottom && self.unknown > 0) {
                    break;
                }
                // Stuck to the bottom while the tail still streams: keep the
                // offset accounting by walking known rows, measuring unknowns.
                if known {
                    content_y += height as f64 + spacing as f64;
                    continue;
                }
            } else if known && content_y + height as f64 <= band_min {
                // Entirely above the over-scan band: nothing to draw.
                content_y += height as f64 + spacing as f64;
                continue;
            }
            // Build (parse if needed, then lay out) row i at content_y.
            let is_tool = rows[i].0.starts_with("tool:");
            if !is_tool && self.parsed[i].is_none() {
                self.parsed[i] = Some(markdown::parse_markdown(&rows[i].1));
                self.parse_calls += 1;
            }
            let blocks = if is_tool {
                &[]
            } else {
                self.parsed[i].as_deref().expect("parsed just above")
            };
            let bound = if known { height } else { MEASURE_BOUND };
            let top = ui.max_rect().top() + content_y as f32;
            let rect = Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + bound);
            let row_expanded = &mut self.expanded[i];
            let row_chunks = &mut self.chunks[i];
            let mut resized = false;
            let out = ui.scope_builder(
                UiBuilder::new().id_salt(("tr", tab_id, i)).max_rect(rect),
                |ui| {
                    ui.group(|ui| {
                        render_row_contents(
                            ui,
                            i,
                            &rows[i].0,
                            toolcards.get(i).and_then(Option::as_ref),
                            &rows[i].1,
                            blocks,
                            &mut RowUi {
                                expanded: row_expanded,
                                chunks: row_chunks,
                                resized: &mut resized,
                            },
                        )
                    })
                },
            );
            let measured = out.response.rect.height();
            if !known {
                self.unknown = self.unknown.saturating_sub(1);
            }
            if resized {
                // The row's expansion changed mid-layout: `measured` describes
                // the previous state, so discard it and re-measure next frame
                // with the new state.
                self.heights[i] = 0.0;
                self.unknown += 1;
            } else {
                self.heights[i] = measured;
            }
            content_y += measured as f64 + spacing as f64;
            if first_built == usize::MAX {
                first_built = i;
            }
            last_built_idx = i;
            count += 1;
        }

        if count > 0 {
            // Bounded parse cache: drop parsed blocks far outside the built
            // band. Heights are never evicted, so skipped rows still advance
            // content_y on later frames without re-measuring.
            for i in 0..first_built.saturating_sub(PARSE_SLACK) {
                self.parsed[i] = None;
            }
            for i in last_built_idx.saturating_add(PARSE_SLACK) + 1..n {
                self.parsed[i] = None;
            }
        }
        self.last_built = count;
    }
}

/// Mutable per-frame state for one row, passed to [`render_row_contents`].
/// Tool rows mutate it when the user collapses/expands the output or asks for
/// the next chunk; any mutation flips `resized` so the virtualizer re-measures
/// the row next frame.
pub(crate) struct RowUi<'a> {
    /// Tool output/args expanded; flipped by the row's toggle button.
    pub expanded: &'a mut bool,
    /// Output chunks shown while expanded; grown by "show more lines".
    pub chunks: &'a mut usize,
    /// Set when a click changed the row's size this frame.
    pub resized: &'a mut bool,
}

/// Body of one transcript row group: a role/tool heading followed by the
/// rendered content. Tool rows render preformatted raw output; everything
/// else renders its parsed Markdown blocks. `card` is the tool overlay state
/// for tool rows.
pub(crate) fn render_row_contents(
    ui: &mut Ui,
    row_index: usize,
    role: &str,
    card: Option<&ToolCard>,
    text: &str,
    blocks: &[markdown::Block],
    row: &mut RowUi,
) {
    if role.starts_with("tool:") {
        render_tool_row(ui, role, card, text, row);
    } else {
        role_heading(ui, role);
        markdown::render_blocks(ui, row_index, blocks);
    }
}

/// Role caption with a visual hierarchy: users are accented with the theme
/// link color, reasoning is demoted to small dim italics, everything else is
/// plain strong. All captions are selectable.
fn role_heading(ui: &mut Ui, role: &str) {
    let rich = match role {
        "user" => egui::RichText::new(role.to_string())
            .strong()
            .color(ui.visuals().hyperlink_color),
        "reasoning" => egui::RichText::new(role.to_string())
            .small()
            .weak()
            .italics(),
        _ => egui::RichText::new(role.to_string()).strong(),
    };
    ui.add(egui::Label::new(rich).selectable(true));
}

/// Draw a tool row: a heading with expand toggle, state, line count and a
/// full-text Copy button, then the (optional) args and the preformatted
/// output body. Newlines in `text` are preserved verbatim — raw logs are
/// never run through Markdown.
fn render_tool_row(ui: &mut Ui, role: &str, card: Option<&ToolCard>, text: &str, row: &mut RowUi) {
    let (state_text, color) = match card.map(|c| c.state) {
        Some(crate::state::ToolState::Running) => {
            ("in progress…", egui::Color32::from_rgb(235, 190, 80))
        }
        Some(crate::state::ToolState::Done) => ("complete", egui::Color32::from_rgb(120, 200, 140)),
        Some(crate::state::ToolState::Error) => ("error", egui::Color32::from_rgb(235, 90, 90)),
        None => ("", egui::Color32::TRANSPARENT),
    };
    let name = card
        .map(|c| c.name.as_str())
        .filter(|n| !n.is_empty())
        .unwrap_or(role);
    let total_lines = text.lines().count();

    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(if *row.expanded { "v" } else { ">" }).small())
            .clicked()
        {
            *row.expanded = !*row.expanded;
            *row.resized = true;
        }
        ui.add(
            egui::Label::new(egui::RichText::new(name.to_string()).strong().color(color))
                .selectable(true),
        );
        if !state_text.is_empty() {
            ui.label(egui::RichText::new(state_text).small().weak());
        }
        if total_lines > 0 {
            ui.weak(format!(
                "{total_lines} line{}",
                if total_lines == 1 { "" } else { "s" }
            ));
        }
        if !text.is_empty() {
            if ui.button(egui::RichText::new("Copy").small()).clicked() {
                ui.ctx().copy_text(text.to_string());
            }
        }
    });

    if let Some(args) = card.and_then(|c| c.args.as_deref()) {
        if !args.is_empty() && args != text {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("args").small().weak());
                if ui
                    .button(egui::RichText::new("Copy args").small())
                    .clicked()
                {
                    ui.ctx().copy_text(args.to_string());
                }
            });
            preformatted_frame(ui, args, CHUNK_LINES, None);
        }
    }

    if text.is_empty() {
        return;
    }
    if *row.expanded {
        let max_lines = row.chunks.saturating_mul(CHUNK_LINES).max(CHUNK_LINES);
        preformatted_frame(ui, text, max_lines, Some(row));
    } else {
        let first = text.lines().next().unwrap_or("");
        let mut shown: String = first.chars().take(COLLAPSED_PREVIEW_CHARS).collect();
        if shown.chars().count() < first.chars().count() {
            shown.push('…');
        }
        ui.add(egui::Label::new(
            egui::RichText::new(shown).monospace().weak(),
        ));
    }
}

/// A bounded preformatted block: at most `max_lines` lines, newlines
/// preserved, long lines horizontally scrollable, theme-derived colors. When
/// `row` is given and more lines exist beyond the bound, a "show more lines"
/// button grows the next chunk and re-measures the row.
fn preformatted_frame(ui: &mut Ui, text: &str, max_lines: usize, row: Option<&mut RowUi>) {
    let (shown, remaining) = markdown::preformatted_prefix(text, max_lines);
    if !shown.is_empty() {
        let visuals = ui.visuals();
        let frame = egui::Frame::default()
            .fill(visuals.code_bg_color)
            .stroke(visuals.widgets.noninteractive.bg_stroke)
            .inner_margin(8.0)
            .corner_radius(4.0);
        frame.show(ui, |ui| {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(shown).monospace())
                        .selectable(true)
                        .wrap_mode(egui::TextWrapMode::Extend),
                );
            });
        });
    }
    if remaining > 0 {
        ui.horizontal(|ui| {
            if let Some(row) = row {
                if ui
                    .button(format!("Show {} more lines…", CHUNK_LINES.min(remaining)))
                    .clicked()
                {
                    *row.chunks = row.chunks.saturating_add(1);
                    *row.resized = true;
                }
            }
            ui.weak(format!("({remaining} more lines; Copy for full output)"));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ToolCard, ToolState};

    const W: f32 = 1264.0;
    const H: f32 = 1412.0;
    const FRAMES: usize = 12;
    const TAB: u64 = 1;

    fn input(frame: usize, width: f32) -> egui::RawInput {
        egui::RawInput {
            time: Some(frame as f64 * 0.016),
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, H))),
            ..Default::default()
        }
    }

    /// Run one headless frame, discarding the font-atlas texture deltas.
    /// Dropping a `FullOutput` with unapplied deltas panics in debug builds
    /// (epaint `TexturesDelta::drop`), and headless frames always produce at
    /// least the initial font-atlas delta.
    fn run_frame(ctx: &egui::Context, input: egui::RawInput, ui_fn: impl FnMut(&mut Ui)) {
        let mut out = ctx.run_ui(input, ui_fn);
        out.textures_delta.clear();
    }

    /// Mixed ordinary/tool rows with short content so the transcript is much
    /// taller than the viewport. Tool rows carry raw multi-line logs (no
    /// Markdown fencing), as real tool output arrives.
    fn sample_rows(n: usize) -> (Vec<(String, String)>, Vec<Option<ToolCard>>) {
        let mut rows = Vec::with_capacity(n);
        let mut cards = Vec::with_capacity(n);
        for i in 0..n {
            match i % 8 {
                3 => {
                    rows.push(("tool: shell".into(), format!("$ echo hello\nhello\nexit 0")));
                    cards.push(Some(ToolCard {
                        name: "shell".into(),
                        state: ToolState::Done,
                        args: None,
                    }));
                }
                6 => {
                    rows.push(("user".into(), format!("> note {i}\n\nplain body **{i}**")));
                    cards.push(None);
                }
                _ => {
                    rows.push((
                        "assistant".into(),
                        format!(
                            "## Message {i}\n{}sample **{i}**\n- item\n",
                            "text ".repeat(8)
                        ),
                    ));
                    cards.push(None);
                }
            }
        }
        (rows, cards)
    }

    /// Full reference render of the same rows through a plain scroll area:
    /// parse each non-tool row once, then lay out every row on every frame
    /// with the given per-row expansion state.
    fn reference_content_height_at(
        rows: &[(String, String)],
        cards: &[Option<ToolCard>],
        mut expanded: Vec<bool>,
        mut chunks: Vec<usize>,
    ) -> f32 {
        let parsed: Vec<Option<Vec<markdown::Block>>> = rows
            .iter()
            .map(|(role, text)| {
                (!role.starts_with("tool:")).then(|| markdown::parse_markdown(text))
            })
            .collect();
        let ctx = egui::Context::default();
        let mut content_h = 0.0;
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                let out = egui::ScrollArea::vertical()
                    .id_salt(("transcript", TAB))
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for (i, ((role, text), blocks)) in rows.iter().zip(&parsed).enumerate() {
                            let blocks = blocks.as_deref().unwrap_or(&[]);
                            let mut resized = false;
                            let mut row = RowUi {
                                expanded: &mut expanded[i],
                                chunks: &mut chunks[i],
                                resized: &mut resized,
                            };
                            ui.group(|ui| {
                                render_row_contents(
                                    ui,
                                    i,
                                    role,
                                    cards[i].as_ref(),
                                    text,
                                    blocks,
                                    &mut row,
                                )
                            });
                        }
                    });
                content_h = out.content_size.y;
            });
        }
        content_h
    }

    /// Reference render with the default expansion state (expanded, 1 chunk).
    fn reference_content_height(rows: &[(String, String)], cards: &[Option<ToolCard>]) -> f32 {
        reference_content_height_at(rows, cards, vec![true; rows.len()], vec![1; rows.len()])
    }

    fn virtual_content_height(
        rows: &[(String, String)],
        cards: &[Option<ToolCard>],
    ) -> (f32, usize) {
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        let mut content_h = 0.0;
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                content_h = cache.show(ui, TAB, true, rows, cards).content_size.y;
            });
        }
        (content_h, cache.last_built)
    }

    /// (a) The virtualized layout must agree with an independent full reference
    /// render to within rounding, while warm frames lay out only the band.
    #[test]
    fn virtual_layout_matches_full_reference_render() {
        let n = 1200;
        let (rows, cards) = sample_rows(n);
        let (virtual_h, last_built) = virtual_content_height(&rows, &cards);
        let reference_h = reference_content_height(&rows, &cards);
        assert!(
            (virtual_h - reference_h).abs() <= 2.0,
            "virtual content height {virtual_h}px != reference {reference_h}px"
        );
        assert!(
            last_built < n / 4,
            "warm frame laid out {last_built} rows; expected < {}",
            n / 4
        );
    }

    /// (b) A width change invalidates every measurement but not the parse
    /// cache: the reflow frame rebuilds all rows without re-parsing any, then
    /// warm behavior resumes. (n=300 keeps everything inside the eviction
    /// slack so retention is exact.)
    #[test]
    fn width_change_reflows_without_reparsing_then_warms_up() {
        let n = 300;
        let (rows, cards) = sample_rows(n);
        let non_tool = rows
            .iter()
            .filter(|(role, _)| !role.starts_with("tool:"))
            .count();
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                cache.show(ui, TAB, true, &rows, &cards);
            });
        }
        assert!(cache.last_built < n / 4, "expected a warm frame first");
        // The cold frame parsed each non-tool row exactly once; warm frames
        // (and tool rows) never parse.
        assert_eq!(
            cache.parse_calls, non_tool,
            "cold parses must happen once per row"
        );
        // Narrower window: stale heights, so this frame measures all rows.
        run_frame(&ctx, input(FRAMES, 1000.0), |ui| {
            cache.show(ui, TAB, true, &rows, &cards);
        });
        assert_eq!(cache.last_built, n, "reflow frame must rebuild every row");
        assert_eq!(
            cache.parse_calls, non_tool,
            "width reflow must retain the parse cache, not reset parsing"
        );
        // Back to band-only rendering at the new width.
        run_frame(&ctx, input(FRAMES + 1, 1000.0), |ui| {
            cache.show(ui, TAB, true, &rows, &cards);
        });
        assert!(
            cache.last_built < n / 4,
            "warm frame laid out {} rows; expected < {}",
            cache.last_built,
            n / 4
        );
    }

    /// (c) sync() keeps the unmeasured-row count exact across growth,
    /// truncation, and content changes.
    #[test]
    fn sync_accounts_for_growth_truncation_and_changes() {
        let mut c = Cache::new();
        c.sync(10, &[]);
        assert_eq!(c.heights.len(), 10);
        assert_eq!(c.parsed.len(), 10);
        assert_eq!(c.unknown, 10);
        // Simulate the top six rows having been measured.
        for h in c.heights.iter_mut().take(6) {
            *h = 24.0;
        }
        c.unknown -= 6;
        // Appending two more rows adds two unknowns and keeps existing ones.
        c.sync(12, &[]);
        assert_eq!(c.heights.len(), 12);
        assert_eq!(c.unknown, 6);
        // Expansion state grows with the rows: expanded, one chunk.
        assert_eq!(c.expanded.len(), 12);
        assert!(c.expanded.iter().all(|&e| e));
        assert_eq!(c.chunks, vec![1; 12]);
        // Truncation: zeros removed from the tail decrement unknown; measured
        // rows removed from the tail are already accounted for.
        c.sync(4, &[]);
        assert_eq!(c.heights.len(), 4);
        assert_eq!(c.unknown, 0);
        assert_eq!(&c.heights[..], &[24.0, 24.0, 24.0, 24.0]);
        // A changed measured row becomes unmeasured again.
        c.sync(4, &[2]);
        assert_eq!(c.unknown, 1);
        assert_eq!(c.heights[2], 0.0);
        // Changing an already-unmeasured row does not double count.
        c.sync(4, &[2]);
        assert_eq!(c.unknown, 1);
        // Out-of-range change indices are ignored.
        c.sync(4, &[9]);
        assert_eq!(c.unknown, 1);
    }

    /// (d) Huge outputs stay bounded: a 1500-line Markdown code block and a
    /// 100k-line raw tool output each lay out at most one [`CHUNK_LINES`]
    /// chunk (no unbounded single label), and the virtual total still matches
    /// the full reference render.
    #[test]
    fn large_code_and_tool_rows_are_bounded() {
        let (mut rows, _) = sample_rows(50);
        let code = 25;
        rows.insert(
            code,
            (
                "assistant".into(),
                format!("```text\n{}```", "tool output line\n".repeat(1500)),
            ),
        );
        let tool = code + 1;
        rows.insert(
            tool,
            ("tool: shell".into(), "tool output line\n".repeat(100_000)),
        );
        let cards: Vec<Option<ToolCard>> = rows
            .iter()
            .map(|(role, _)| {
                (role.starts_with("tool:")).then(|| ToolCard {
                    name: "shell".into(),
                    state: ToolState::Done,
                    args: None,
                })
            })
            .collect();

        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                cache.show(ui, TAB, true, &rows, &cards);
            });
        }
        // Both rows were measured on the cold frame; generous per-line bounds
        // (10..30 px/line) keep the assertion font-independent while still
        // proving the rows are chunk-bounded, not unbounded.
        for (idx, what) in [(code, "code row"), (tool, "tool row")] {
            let h = cache.heights[idx];
            assert!(
                h > CHUNK_LINES as f32 * 10.0,
                "{what} measured {h}px; expected a full first chunk (> {}px)",
                CHUNK_LINES * 10
            );
            assert!(
                h < CHUNK_LINES as f32 * 30.0 + 500.0,
                "{what} measured {h}px; expected bounded by {} lines + chrome",
                CHUNK_LINES
            );
        }
        assert!(
            cache.parsed[tool].is_none(),
            "tool rows are never markdown-parsed"
        );

        let (virtual_h, _) = virtual_content_height(&rows, &cards);
        let reference_h = reference_content_height(&rows, &cards);
        assert!(
            (virtual_h - reference_h).abs() <= 2.0,
            "virtual content height {virtual_h}px != reference {reference_h}px"
        );
    }

    /// (e) A raw multi-line tool log is preformatted, not Markdown: newlines
    /// and Markdown markers survive byte-for-byte, every line occupies its own
    /// row, and the row is never parsed.
    #[test]
    fn raw_multiline_tool_log_is_preformatted() {
        let log = [
            "# this is NOT a heading",
            "- this is NOT a list item",
            "**this is NOT bold**",
            "`neither is this code`",
            "2026-09-08T15:43:53Z [INFO] started pid=4242",
            "2026-09-08T15:43:54Z [WARN] retrying attempt=2",
            "    indented    line with   spaces",
        ]
        .join("\n");
        // The preformatted prefix preserves raw logs byte-for-byte...
        assert_eq!(markdown::preformatted_prefix(&log, 10), (log.clone(), 0));
        // ...while Markdown would have mangled at least two of these lines.
        let parsed = markdown::parse_markdown(&log);
        assert!(
            parsed
                .iter()
                .any(|b| matches!(b, markdown::Block::Heading { .. }))
        );

        let rows = vec![("tool: shell".to_string(), log.clone())];
        let cards = vec![Some(ToolCard {
            name: "shell".into(),
            state: ToolState::Done,
            args: Some(r#"{"command":"tail -f log"}"#.into()),
        })];
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        let mut virtual_h = 0.0;
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                virtual_h = cache.show(ui, TAB, true, &rows, &cards).content_size.y;
            });
        }
        assert!(
            cache.parsed[0].is_none(),
            "tool row must not be markdown-parsed"
        );
        assert_eq!(cache.parse_calls, 0, "tool rows must not parse markdown");
        // Every one of the 7 lines keeps its own row: newlines preserved.
        let h = cache.heights[0];
        assert!(
            h > 7.0 * 10.0 + 20.0,
            "log row height {h}px; 7 lines + heading must each occupy a row"
        );

        // The virtual total matches the full reference render.
        let reference_h = reference_content_height(&rows, &cards);
        assert!(
            (virtual_h - reference_h).abs() <= 2.0,
            "virtual content {virtual_h}px != reference {reference_h}px"
        );
    }

    /// (f) Large tool outputs are chunked: the expanded row lays out exactly
    /// one bounded chunk, and asking for the next chunk re-measures the row to
    /// a new (still bounded) height matching the reference at that expansion.
    #[test]
    fn large_tool_output_is_chunked_and_remeasured() {
        let log = (0..10_000)
            .map(|i| format!("line {i:05}"))
            .collect::<Vec<_>>()
            .join("\n");
        let rows = vec![("tool: shell".to_string(), log.clone())];
        let cards = vec![Some(ToolCard {
            name: "shell".into(),
            state: ToolState::Done,
            args: None,
        })];
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        let mut h1 = 0.0;
        run_frame(&ctx, input(0, W), |ui| {
            h1 = cache.show(ui, TAB, true, &rows, &cards).content_size.y;
        });
        let row_h1 = cache.heights[0];
        // One chunk only: bounded far below the full 10k lines.
        assert!(
            row_h1 < CHUNK_LINES as f32 * 30.0 + 500.0,
            "chunked row height {row_h1}px; expected bounded by one chunk"
        );
        assert!(
            row_h1 > CHUNK_LINES as f32 * 10.0,
            "chunked row height {row_h1}px; expected a full first chunk"
        );
        assert!((h1 - reference_content_height(&rows, &cards)).abs() <= 2.0);

        // Simulate a "Show more lines" click: one more chunk, re-measured.
        cache.chunks[0] = 2;
        cache.heights[0] = 0.0;
        cache.unknown += 1;
        let mut h2 = 0.0;
        run_frame(&ctx, input(1, W), |ui| {
            h2 = cache.show(ui, TAB, true, &rows, &cards).content_size.y;
        });
        let row_h2 = cache.heights[0];
        assert_eq!(cache.unknown, 0, "re-measured row must be accounted for");
        assert!(row_h2 > row_h1, "growing a chunk must grow the row");
        assert!(
            row_h2 < (2 * CHUNK_LINES) as f32 * 30.0 + 500.0,
            "two-chunk row height {row_h2}px; expected bounded by two chunks"
        );
        let mut chunks = vec![1usize];
        chunks[0] = 2;
        assert!((h2 - reference_content_height_at(&rows, &cards, vec![true], chunks)).abs() <= 2.0);
    }

    /// (g) Collapsing a tool row re-measures the virtualized height: the next
    /// frame's cached height is the collapsed one, the transcript total shrinks
    /// to the collapsed reference, and the unknown-row count stays exact.
    #[test]
    fn collapsing_tool_row_remeasures_height() {
        let (mut rows, mut cards) = sample_rows(80);
        let tool = rows.len();
        rows.push((
            "tool: shell".into(),
            (0..120)
                .map(|i| format!("log line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        cards.push(Some(ToolCard {
            name: "shell".into(),
            state: ToolState::Done,
            args: None,
        }));
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        let mut expanded_h = 0.0;
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                expanded_h = cache.show(ui, TAB, true, &rows, &cards).content_size.y;
            });
        }
        let expanded_row_h = cache.heights[tool];
        assert!(
            expanded_row_h > 120.0 * 10.0,
            "expected an expanded 120-line row"
        );

        // Simulate a collapse click (what RowUi does inline while building).
        cache.expanded[tool] = false;
        cache.heights[tool] = 0.0;
        cache.unknown += 1;
        let mut collapsed_h = 0.0;
        run_frame(&ctx, input(FRAMES, W), |ui| {
            collapsed_h = cache.show(ui, TAB, true, &rows, &cards).content_size.y;
        });
        assert!(
            cache.heights[tool] > 0.0,
            "collapsed row must be re-measured next frame"
        );
        assert!(
            cache.heights[tool] < 100.0,
            "collapsed row shows a one-line preview, not 120 lines (got {}px)",
            cache.heights[tool]
        );
        assert!(
            collapsed_h < expanded_h,
            "collapsing must shrink the transcript ({collapsed_h} vs {expanded_h})"
        );
        assert_eq!(cache.unknown, 0, "unknown-row accounting must stay exact");

        // The collapsed total matches a full reference render in the same state.
        let mut expanded = vec![true; rows.len()];
        expanded[tool] = false;
        let reference_h = reference_content_height_at(&rows, &cards, expanded, vec![1; rows.len()]);
        assert!(
            (collapsed_h - reference_h).abs() <= 2.0,
            "collapsed content {collapsed_h}px != reference {reference_h}px"
        );
    }
}
