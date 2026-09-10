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
//! - Concise tool rows show labels; verbose mode expands arguments and output.
//!   Applied edit diffs stay visible in both modes with added/removed colors.
//! - Raw tool output is **preformatted**: the output is laid out monospace
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
use crate::state::{ToolCard, ToolState};
use crate::theme::ThemeColors;
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
/// Characters of the live reasoning tail shown while it is collapsed.
const COLLAPSED_PREVIEW_CHARS: usize = 160;
/// Compact separation between transcript messages.
const TRANSCRIPT_ROW_GAP: f32 = 20.0;
/// Horizontal inset shared with the composer.
const TRANSCRIPT_HORIZONTAL_PADDING: i8 = crate::theme::CHAT_PADDING;
/// Breathing room above the first row and below the last, matching the
/// horizontal inset so messages never touch the pane edges.
const TRANSCRIPT_VERTICAL_PADDING: f32 = crate::theme::CHAT_PADDING as f32;

/// Concise mode keeps calls compact while making failures immediately visible.
fn default_tool_expanded(card: &ToolCard) -> bool {
    card.state == ToolState::Error
}

/// Per-tab virtualization state, owned by [`crate::Tab`]. One per conversation,
/// kept alive across reconnects via [`Self::reset`].
pub(crate) struct Cache {
    tool_verbosity: crate::layout::ToolVerbosity,
    /// Content width the cached measurements were taken at.
    width: f32,
    /// Zoom factor the cached measurements were taken at.
    zoom: f32,
    /// Per-row measured height in points; `0.0` means "not measured yet".
    pub(crate) heights: Vec<f32>,
    /// Per-row parsed markdown; `None` means "not parsed yet or evicted".
    /// Tool rows are preformatted and never occupy one.
    parsed: Vec<Option<Vec<markdown::Block>>>,
    /// Per-row tool output/args expansion, initialized from the tool card.
    expanded: Vec<bool>,
    /// Explicit disclosure choices survive streaming and lifecycle changes.
    expansion_chosen: Vec<bool>,
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
    /// Live activity line (e.g. "running shell: cargo test") drawn after the
    /// final row so a running turn's progress lives in the chat, not the
    /// toolbar. Owned so the caller does not have to keep it alive per frame.
    status: Option<String>,
    /// Measured height of the status line, reused as its layout bound next
    /// frame (0.0 means "not measured yet").
    status_height: f32,
}

impl Cache {
    pub(crate) fn new() -> Self {
        Self {
            tool_verbosity: crate::layout::ToolVerbosity::Concise,
            width: 0.0,
            zoom: 0.0,
            heights: Vec::new(),
            parsed: Vec::new(),
            expanded: Vec::new(),
            expansion_chosen: Vec::new(),
            chunks: Vec::new(),
            unknown: 0,
            last_built: 0,
            parse_calls: 0,
            status: None,
            status_height: 0.0,
        }
    }

    /// A mode change applies to existing rows as well as future tool calls.
    /// Discard tool heights and disclosure overrides without reparsing prose.
    pub(crate) fn set_tool_verbosity(
        &mut self,
        verbosity: crate::layout::ToolVerbosity,
        cards: &[Option<ToolCard>],
    ) {
        if self.tool_verbosity == verbosity {
            return;
        }
        self.tool_verbosity = verbosity;
        for (i, card) in cards.iter().enumerate().take(self.heights.len()) {
            if let Some(card) = card {
                if self.heights[i] != 0.0 {
                    self.unknown += 1;
                }
                self.heights[i] = 0.0;
                self.expansion_chosen[i] = false;
                self.expanded[i] = verbosity == crate::layout::ToolVerbosity::Verbose
                    || default_tool_expanded(card);
                self.chunks[i] = 1;
            }
        }
    }

    /// Set the live activity line drawn after the final row. `None` hides it;
    /// a changed value discards the stale measurement so the next frame
    /// re-measures at the new text.
    pub(crate) fn set_status(&mut self, status: Option<String>) {
        if self.status != status {
            self.status_height = 0.0;
        }
        self.status = status;
    }

    /// Drop every measurement and parse. Called on (re)attach when the
    /// conversation is about to be replayed wholesale; the stale width/zoom
    /// force a full reflow on the next layout.
    pub(crate) fn reset(&mut self) {
        let verbosity = self.tool_verbosity;
        *self = Self::new();
        self.tool_verbosity = verbosity;
    }

    /// Reconcile the cache with the authoritative row vector.
    ///
    /// `changed_rows` holds every index whose content changed since the last
    /// sync (recorded by [`crate::state::State`] and drained once per frame by
    /// the caller); `rows_len` is the current row count. The caller also calls
    /// this from [`Self::show`] whenever the row count alone changed.
    #[allow(dead_code)]
    pub(crate) fn sync(&mut self, rows_len: usize, changed: &[usize]) {
        self.sync_with_cards(rows_len, changed, &[]);
    }

    /// Reconcile rows and initialize/reinitialize tool expansion from the
    /// authoritative card lifecycle. This must run before `show`: appending a
    /// row otherwise resizes `expanded` with `true`, bypassing the default.
    pub(crate) fn sync_with_cards(
        &mut self,
        rows_len: usize,
        changed: &[usize],
        toolcards: &[Option<ToolCard>],
    ) {
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
        self.expansion_chosen.resize(rows_len, false);
        self.chunks.resize(rows_len, 1);
        // Only automatic expansion follows the tool lifecycle.
        for i in (0..rows_len).filter(|i| *i >= old_len || changed.contains(i)) {
            if !self.expansion_chosen[i]
                && let Some(card) = toolcards.get(i).and_then(Option::as_ref)
            {
                self.expanded[i] = self.tool_verbosity == crate::layout::ToolVerbosity::Verbose
                    || default_tool_expanded(card);
            }
        }
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
        colors: &ThemeColors,
    ) -> egui::containers::scroll_area::ScrollAreaOutput<()> {
        if self.heights.len() != rows.len() {
            self.sync_with_cards(rows.len(), &[], toolcards);
        }
        ui.set_min_width(ui.available_width());
        ui.set_max_width(ui.available_width());
        let status = self.status.clone();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt(("transcript", tab_id))
            .stick_to_bottom(stick_to_bottom)
            .show_viewport(ui, |ui, viewport| {
                self.layout_rows(
                    ui,
                    tab_id,
                    viewport,
                    stick_to_bottom,
                    rows,
                    toolcards,
                    colors,
                    status.as_deref(),
                )
            })
    }

    // Lays out a row band from several borrowed sources; a params struct would
    // add indirection without clarifying the call.
    #[allow(clippy::too_many_arguments)]
    fn layout_rows(
        &mut self,
        ui: &mut Ui,
        tab_id: u64,
        viewport: Rect,
        stick_to_bottom: bool,
        rows: &[(String, String)],
        toolcards: &[Option<ToolCard>],
        colors: &ThemeColors,
        status: Option<&str>,
    ) {
        let n = rows.len();
        // Keep prose comfortable on wide windows without wasting narrow-pane
        // space. Use the same width for measurement and visible row layout.
        let width = ui.max_rect().width().min(crate::theme::CHAT_WIDTH);
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

        let spacing = TRANSCRIPT_ROW_GAP.max(ui.spacing().item_spacing.y);
        // Full transcript height from cached measurements (unmeasured rows
        // count 0 here and grow the region as they are built this frame), plus
        // symmetric vertical padding so the first and last rows never touch the
        // pane edges. A live status line adds one row gap plus its (last-frame)
        // height after the final row.
        let status_gap = if status.is_some() && n > 0 {
            spacing
        } else {
            0.0
        };
        let status_height = if status.is_some() {
            self.status_height
        } else {
            0.0
        };
        let total: f32 = self.heights.iter().sum::<f32>()
            + spacing * (n.saturating_sub(1)) as f32
            + status_gap
            + status_height
            + TRANSCRIPT_VERTICAL_PADDING * 2.0;
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
        // Whether the loop walked every row this frame, so the trailing status
        // line has a valid position (`content_y` past the last row).
        let mut reached_end = true;
        // Rows start below the top padding; the trailing padding is accounted
        // for by `total` above.
        let mut content_y = TRANSCRIPT_VERTICAL_PADDING as f64;

        for (i, row) in rows.iter().enumerate() {
            let height = self.heights[i];
            let known = height > 0.0;
            if content_y > band_max {
                // Everything from here on starts below the over-scan band.
                if !(stick_to_bottom && self.unknown > 0) {
                    reached_end = false;
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
            let is_tool = row.0.starts_with("tool:");
            let is_diff = row.0 == "system" && row.1.starts_with('\n');
            if !is_tool && !is_diff && self.parsed[i].is_none() {
                self.parsed[i] = Some(markdown::parse_markdown(&row.1));
                self.parse_calls += 1;
            }
            let blocks = if is_tool || is_diff {
                &[]
            } else {
                self.parsed[i].as_deref().expect("parsed just above")
            };
            let bound = if known { height } else { MEASURE_BOUND };
            let top = ui.max_rect().top() + content_y as f32;
            let left = ui.max_rect().center().x - width * 0.5;
            let rect = Rect::from_min_size(egui::pos2(left, top), egui::vec2(width, bound));
            let row_expanded = &mut self.expanded[i];
            let was_expanded = *row_expanded;
            let row_chunks = &mut self.chunks[i];
            let mut resized = false;
            let out = ui.scope_builder(
                UiBuilder::new().id_salt(("tr", tab_id, i)).max_rect(rect),
                |ui| {
                    egui::Frame::default()
                        .inner_margin(egui::Margin::symmetric(TRANSCRIPT_HORIZONTAL_PADDING, 0))
                        .show(ui, |ui| {
                            render_row_contents(
                                ui,
                                i,
                                &row.0,
                                toolcards.get(i).and_then(Option::as_ref),
                                &row.1,
                                blocks,
                                &mut RowUi {
                                    expanded: row_expanded,
                                    chunks: row_chunks,
                                    resized: &mut resized,
                                },
                                colors,
                            );
                        });
                },
            );
            let measured = out.response.rect.height();
            if was_expanded != self.expanded[i] {
                self.expansion_chosen[i] = true;
            }
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

        // Trailing status line: rendered as the last element of the chat, so a
        // running turn's progress scrolls with the conversation rather than
        // living in the toolbar. Only laid out when the loop reached the end
        // (so `content_y` is the true tail) and the line meets the band.
        if let Some(text) = status
            && reached_end
        {
            let top = ui.max_rect().top() + content_y as f32;
            if top <= band_max as f32 {
                let left = ui.max_rect().center().x - width * 0.5;
                let bound = if self.status_height > 0.0 {
                    self.status_height
                } else {
                    MEASURE_BOUND
                };
                let rect = Rect::from_min_size(egui::pos2(left, top), egui::vec2(width, bound));
                let out = ui.scope_builder(
                    UiBuilder::new().id_salt(("status", tab_id)).max_rect(rect),
                    |ui| {
                        egui::Frame::default()
                            .inner_margin(egui::Margin::symmetric(TRANSCRIPT_HORIZONTAL_PADDING, 0))
                            .show(ui, |ui| render_status_row(ui, text, colors));
                    },
                );
                self.status_height = out.response.rect.height();
            }
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
        // When the last row was laid out this frame, fold the trailing vertical
        // padding into the content extent explicitly. `set_height(total)` above
        // is only a floor and can be clamped when a row re-measures larger than
        // its stale cached height, which would otherwise leave the content
        // bottom flush against the final row. The padding sits below the status
        // line when one is shown.
        if count > 0 && last_built_idx + 1 == n {
            let rows_bottom = content_y - spacing as f64;
            let content_bottom = if status.is_some() && reached_end {
                rows_bottom + status_gap as f64 + self.status_height as f64
            } else {
                rows_bottom
            };
            let bottom = ui.max_rect().top() + content_bottom as f32;
            ui.expand_to_include_rect(Rect::from_min_size(
                egui::pos2(ui.max_rect().left(), bottom),
                egui::vec2(0.0, TRANSCRIPT_VERTICAL_PADDING),
            ));
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
// Renders one row from several borrowed inputs; a params struct would add
// indirection without clarifying the call.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_row_contents(
    ui: &mut Ui,
    row_index: usize,
    role: &str,
    card: Option<&ToolCard>,
    text: &str,
    blocks: &[markdown::Block],
    row: &mut RowUi,
    colors: &ThemeColors,
) {
    ui.style_mut()
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
    if role.starts_with("tool:") {
        render_tool_row(ui, role, card, text, row, colors);
    } else if role == "user" {
        // A restrained right-aligned prompt surface, measured by the same
        // renderer in both the virtualized and visible passes.
        let width = ui.available_width();
        let cap = (width * 0.88).max(width.min(240.0));
        let bubble = markdown::short_paragraph_width(ui, blocks, colors)
            .map(|text_width| (text_width + 26.0).max(48.0).min(cap))
            .unwrap_or(cap);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space((width - bubble).max(0.0));
            egui::Frame::default()
                .fill(if colors.user_msg_bg == egui::Color32::TRANSPARENT {
                    ui.visuals().code_bg_color
                } else {
                    colors.user_msg_bg
                })
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.visuals_mut().override_text_color = Some(colors.user_msg);
                    ui.set_width((bubble - 24.0).max(1.0));
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        markdown::render_blocks(ui, row_index, blocks, colors);
                    });
                });
        });
    } else if role == "reasoning" {
        render_reasoning_row(ui, row_index, text, blocks, row, colors);
    } else {
        role_heading(ui, role, colors);
        if role == "system" && text.starts_with('\n') {
            markdown::render_diff_preview(ui, text, colors);
        } else {
            markdown::render_blocks(ui, row_index, blocks, colors);
        }
    }
}

/// Live activity line drawn after the last transcript row: a spinner in the
/// tool accent color plus dim, truncated status text (e.g. "running shell:
/// cargo test"). The spinner keeps egui repainting while a turn is active.
fn render_status_row(ui: &mut Ui, text: &str, colors: &ThemeColors) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(12.0).color(colors.tool_call));
        ui.add(
            egui::Label::new(egui::RichText::new(text).small().weak())
                .truncate()
                .selectable(false),
        );
    });
}

/// A live, collapsed thinking affordance: the demoted reasoning heading plus an
/// expand toggle. Collapsed (the default) shows only the reasoning tail — the
/// last non-empty line — so a streaming model signals progress without dumping
/// a potentially huge blob; expanding renders the full reasoning Markdown. The
/// toggle re-measures the row via [`RowUi::resized`].
fn render_reasoning_row(
    ui: &mut Ui,
    row_index: usize,
    text: &str,
    blocks: &[markdown::Block],
    row: &mut RowUi,
    colors: &ThemeColors,
) {
    ui.horizontal(|ui| {
        role_heading(ui, "reasoning", colors);
        if crate::icons::button(
            ui,
            if *row.expanded {
                crate::icons::Icon::ChevronDown
            } else {
                crate::icons::Icon::ChevronRight
            },
            if *row.expanded {
                "Collapse thinking"
            } else {
                "Expand thinking"
            },
        )
        .clicked()
        {
            *row.expanded = !*row.expanded;
            *row.resized = true;
        }
    });
    if *row.expanded {
        markdown::render_blocks(ui, row_index, blocks, colors);
    } else if let Some(tail) = text.lines().rev().find(|line| !line.trim().is_empty()) {
        let mut shown: String = tail.chars().take(COLLAPSED_PREVIEW_CHARS).collect();
        if shown.chars().count() < tail.chars().count() {
            shown.push('…');
        }
        ui.add(egui::Label::new(
            egui::RichText::new(shown)
                .small()
                .weak()
                .italics()
                .color(colors.thinking),
        ));
    }
}

/// Quiet, selectable role captions; message content carries the visual emphasis.
fn role_heading(ui: &mut Ui, role: &str, colors: &ThemeColors) {
    let rich = match role {
        "user" => egui::RichText::new("You").small().color(colors.user_msg),
        "reasoning" => egui::RichText::new("Thinking")
            .small()
            .weak()
            .italics()
            .color(colors.thinking),
        "system" => egui::RichText::new("System")
            .small()
            .color(colors.system_msg),
        "assistant" => egui::RichText::new("Bone").small().weak(),
        _ => egui::RichText::new(role.to_string()).small().weak(),
    };
    ui.add(egui::Label::new(rich).selectable(true));
}

/// Draw a quiet tool heading, then expanded actions, args and preformatted
/// output body. Newlines in `text` are preserved verbatim — raw logs are
/// never run through Markdown.
fn render_tool_row(
    ui: &mut Ui,
    role: &str,
    card: Option<&ToolCard>,
    text: &str,
    row: &mut RowUi,
    colors: &ThemeColors,
) {
    let is_error = matches!(card.map(|c| c.state), Some(crate::state::ToolState::Error));
    let name = match card.and_then(|c| c.label.as_deref()) {
        Some(label) => label,
        None => card
            .map(|c| c.name.as_str())
            .filter(|n| !n.is_empty())
            .unwrap_or(role),
    };
    let total_lines = text.lines().count();

    ui.horizontal_wrapped(|ui| {
        if matches!(
            card.map(|c| c.state),
            Some(crate::state::ToolState::Running)
        ) {
            // The spinner is part of the tool heading, so it should carry the
            // same disclosure affordance as the label instead of becoming a
            // dead click target while a call is running.
            let spinner = ui
                .add(egui::Spinner::new().size(12.0).color(colors.warn))
                .interact(egui::Sense::click());
            if spinner.clicked() {
                *row.expanded = !*row.expanded;
                *row.resized = true;
            }
        }
        if !name.is_empty() {
            let label = ui.add(
                egui::Label::new(
                    egui::RichText::new(name.replace(['\n', '\r'], " "))
                        .size(14.0)
                        .color(if is_error {
                            colors.tool_error
                        } else {
                            colors.tool_call
                        }),
                )
                .truncate()
                .selectable(false)
                .sense(egui::Sense::click()),
            );
            if label.clicked() {
                *row.expanded = !*row.expanded;
                *row.resized = true;
            }
            label
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(name);
        }
    });

    // Applied edits remain inspectable in either mode, independently of the
    // disclosure for arguments and raw output. The daemon result is authoritative.
    let edit_diff = card
        .is_some_and(|card| card.name == "edit_file" && card.state == ToolState::Done)
        && !text.is_empty();
    if edit_diff {
        render_edit_diff(ui, text, row, colors);
    }

    // A collapsed call is a single summary; shell logs require explicit details.
    if !*row.expanded {
        return;
    }

    if !text.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.weak(format!(
                "{total_lines} line{}",
                if total_lines == 1 { "" } else { "s" }
            ));
            if ui
                .add(egui::Button::new(egui::RichText::new("Copy").small().weak()).frame(false))
                .clicked()
            {
                ui.ctx().copy_text(text.to_string());
            }
        });
    }

    if let Some(args) = card.and_then(|c| c.args.as_deref())
        && !args.is_empty()
        && args != text
    {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("args").small().weak());
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Copy args").small().weak()).frame(false),
                )
                .clicked()
            {
                ui.ctx().copy_text(args.to_string());
            }
        });
        preformatted_frame(ui, args, CHUNK_LINES, "tool-args", None, false);
    }

    // Explicitly hidden results stay hidden, even when details are expanded.
    if edit_diff || card.and_then(|c| c.show_result) == Some(false) {
        return;
    }

    if text.is_empty() {
        return;
    }
    if is_shell_card(card) {
        let is_error = matches!(card.map(|c| c.state), Some(crate::state::ToolState::Error));
        render_shell_body(ui, text, row, is_error, colors);
        return;
    }
    let max_lines = row.chunks.saturating_mul(CHUNK_LINES).max(CHUNK_LINES);
    let is_error = matches!(card.map(|c| c.state), Some(ToolState::Error));
    preformatted_frame(ui, text, max_lines, "tool-result", Some(row), is_error);
}

/// Bounded, colored edit hunks. Large diffs grow on demand in either mode.
fn render_edit_diff(ui: &mut Ui, text: &str, row: &mut RowUi, colors: &ThemeColors) {
    let text = text.trim_start_matches('\n');
    let body = text
        .split_once('\n')
        .filter(|(header, _)| header.trim_start().starts_with("edit_file "))
        .map_or(text, |(_, body)| body);
    let limit = row.chunks.saturating_mul(CHUNK_LINES).max(CHUNK_LINES);
    let (shown, remaining) = markdown::preformatted_prefix(body, limit);
    ui.push_id("edit-diff", |ui| {
        markdown::render_diff_preview(ui, &shown, colors)
    });
    if remaining > 0
        && ui
            .add(egui::Button::new(format!("Show more diff lines ({remaining} remaining)")).wrap())
            .clicked()
    {
        *row.chunks = row.chunks.saturating_add(1);
        *row.resized = true;
    }
}

/// A bounded preformatted block: at most `max_lines` lines, newlines
/// preserved, theme-derived colors. Errors wrap for readability; other output
/// and arguments retain horizontal scrolling. When `row` is given and more lines
/// exist beyond the bound, a "show more lines" button grows the next chunk and
/// re-measures the row.
fn preformatted_frame(
    ui: &mut Ui,
    text: &str,
    max_lines: usize,
    id_salt: &'static str,
    row: Option<&mut RowUi>,
    wrap: bool,
) {
    let (shown, remaining) = markdown::preformatted_prefix(text, max_lines);
    if !shown.is_empty() {
        let visuals = ui.visuals();
        let frame = egui::Frame::default()
            .fill(visuals.code_bg_color)
            .inner_margin(10.0)
            .corner_radius(6.0);
        frame.show(ui, |ui| {
            let label = egui::Label::new(egui::RichText::new(shown).monospace()).selectable(true);
            if wrap {
                ui.add(label.wrap());
            } else {
                egui::ScrollArea::horizontal()
                    .id_salt(id_salt)
                    .show(ui, |ui| {
                        ui.add(label.wrap_mode(egui::TextWrapMode::Extend));
                    });
            }
        });
    }
    output_chunk_footer(ui, remaining, row);
}

/// Wrap footer controls within the pane instead of widening the transcript's
/// measured column (which also displaces subsequent, centered message rows).
fn output_chunk_footer(ui: &mut Ui, remaining: usize, row: Option<&mut RowUi>) {
    if remaining > 0 {
        // Lay out the hint against the whole pane so it moves below the button
        // as one item when needed, rather than splitting around the button.
        let hint = egui::WidgetText::from(
            egui::RichText::new(format!("({remaining} more lines; Copy for full output)")).weak(),
        )
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Wrap),
            ui.available_width(),
            egui::TextStyle::Body,
        );
        ui.horizontal_wrapped(|ui| {
            if let Some(row) = row
                && ui
                    .button(format!("Show {} more lines…", CHUNK_LINES.min(remaining)))
                    .clicked()
            {
                *row.chunks = row.chunks.saturating_add(1);
                *row.resized = true;
            }
            ui.add(egui::Label::new(hint));
        });
    }
}

/// True when this tool row is the shell tool, whose output gets the TUI's
/// gutter-and-preview treatment instead of the generic preformatted block.
fn is_shell_card(card: Option<&ToolCard>) -> bool {
    card.is_some_and(|c| c.name == "shell")
}

/// Split a shell tool result into the lines to display. When the result carries
/// the `exit code: …` / `stdout:` / `stderr:` framing produced by the shell
/// tool, merge stdout and stderr and drop trailing blank lines (an empty stdout
/// still occupies a wire line); otherwise return the raw lines. Mirrors the TUI
/// `shell_output_lines`.
fn shell_output_lines(content: &str) -> Vec<String> {
    let mut lines = content.lines();
    if matches!(lines.next(), Some(line) if line.starts_with("exit code: "))
        && matches!(lines.next(), Some("stdout:"))
    {
        let rest = lines.collect::<Vec<_>>();
        let (stdout, stderr) = match rest.iter().position(|line| *line == "stderr:") {
            Some(pos) => (&rest[..pos], &rest[pos + 1..]),
            None => (&rest[..], &[][..]),
        };
        let mut out = stdout.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        while out.last().is_some_and(|line| line.is_empty()) {
            out.pop();
        }
        out.extend(stderr.iter().map(|s| s.to_string()));
        while out.last().is_some_and(|line| line.is_empty()) {
            out.pop();
        }
        return out;
    }
    content.lines().map(str::to_string).collect()
}

/// Render shell tool output the way the TUI does: a `│`/`╰` gutter, a collapsed
/// preview of the first and last two lines, and chunk-bounded expansion so a
/// huge output never becomes one unbounded label.
fn render_shell_body(
    ui: &mut Ui,
    text: &str,
    row: &mut RowUi,
    is_error: bool,
    colors: &ThemeColors,
) {
    let lines = shell_output_lines(text);
    if lines.is_empty() {
        return;
    }
    let gutter_color = if is_error {
        colors.tool_error
    } else {
        colors.tool_call
    };

    let max_lines = row.chunks.saturating_mul(CHUNK_LINES).max(CHUNK_LINES);
    let remaining = lines.len().saturating_sub(max_lines);
    let shown = &lines[..lines.len().min(max_lines)];

    let font = egui::TextStyle::Monospace.resolve(ui.style().as_ref());
    let space = ui.fonts_mut(|fonts| fonts.glyph_width(&font, ' '));
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(8.0 * space);
        let galley = ui.fonts_mut(|fonts| {
            fonts.layout_job(egui::text::LayoutJob::simple(
                shown.join("\n"),
                font.clone(),
                colors.tool_call,
                ui.available_width().max(1.0),
            ))
        });
        // Keep the raw output in one selectable label. Paint the gutter beside
        // its measured visual rows, so every continuation stays in the text
        // column and decoration never becomes part of a copied selection.
        let response = ui.add(egui::Label::new(galley.clone()).selectable(true));
        for (idx, line) in galley.rows.iter().enumerate() {
            let pos = response.rect.min + egui::vec2(-space, line.pos.y);
            ui.painter().text(
                pos,
                egui::Align2::RIGHT_TOP,
                if idx + 1 == galley.rows.len() {
                    "╰"
                } else {
                    "│"
                },
                font.clone(),
                gutter_color,
            );
        }
    });

    output_chunk_footer(ui, remaining, Some(row));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ToolCard, ToolState};

    const W: f32 = 1264.0;
    const H: f32 = 1412.0;
    const FRAMES: usize = 12;
    #[test]
    fn verbosity_switches_existing_calls_and_keeps_colored_edit_diffs() {
        let ctx = egui::Context::default();
        let colors = ThemeColors::default();
        let rows = vec![
            (
                "tool: shell".into(),
                "exit code: 0\nstdout:\nSHELL_OUTPUT_SENTINEL".into(),
            ),
            ("tool: read_file".into(), "FILE_CONTENT_SENTINEL".into()),
            (
                "tool: edit_file".into(),
                "\n    edit_file src/app.rs (-1 | +1)\n    4 - old value\n    4 + new value".into(),
            ),
        ];
        let cards = vec![
            Some(ToolCard {
                name: "shell".into(),
                state: ToolState::Done,
                args: Some(r#"{"command":"cargo test"}"#.into()),
                label: Some("shell cargo test".into()),
                show_result: Some(true),
                eager: Some(true),
            }),
            Some(ToolCard {
                name: "read_file".into(),
                state: ToolState::Done,
                args: Some(r#"{"path":"src/app.rs"}"#.into()),
                label: Some("read_file src/app.rs".into()),
                show_result: None,
                eager: None,
            }),
            Some(ToolCard {
                name: "edit_file".into(),
                state: ToolState::Done,
                args: Some(
                    r#"{"path":"src/app.rs","old_text":"old value","new_text":"new value"}"#.into(),
                ),
                label: Some("edit_file src/app.rs".into()),
                show_result: Some(false),
                eager: None,
            }),
        ];
        let mut cache = Cache::new();
        let mut concise_height = 0.0;
        for (mode, verbose) in [
            (crate::layout::ToolVerbosity::Concise, false),
            (crate::layout::ToolVerbosity::Verbose, true),
            (crate::layout::ToolVerbosity::Concise, false),
        ] {
            cache.set_tool_verbosity(mode, &cards);
            let mut texts = vec![];
            let mut height = 0.0;
            for frame in 0..3 {
                let mut out = ctx.run_ui(input(frame, 600.0), |ui| {
                    height = cache
                        .show(ui, TAB, true, &rows, &cards, &colors)
                        .content_size
                        .y;
                });
                texts = painted_text(&out);
                out.textures_delta.clear();
            }
            let content = texts
                .iter()
                .map(|t| t.galley.job.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(content.contains("read_file src/app.rs"));
            assert_eq!(content.contains("SHELL_OUTPUT_SENTINEL"), verbose);
            assert_eq!(content.contains("FILE_CONTENT_SENTINEL"), verbose);
            assert_eq!(content.contains("old_text"), verbose);
            assert!(content.contains("old value") && content.contains("new value"));
            for color in [colors.diff_removed, colors.diff_added] {
                assert!(
                    texts.iter().any(|t| t
                        .galley
                        .job
                        .sections
                        .iter()
                        .any(|section| section.format.background == color)),
                    "diff must retain both colors in {mode:?}"
                );
            }
            assert!(!content.contains("Done"));
            if verbose {
                assert!(height > concise_height);
            } else {
                concise_height = height;
            }
        }
    }

    #[test]
    fn completed_tools_are_collapsed_by_default_but_live_errors_are_visible() {
        let card = |state| ToolCard {
            name: "shell".into(),
            state,
            args: None,
            label: None,
            show_result: None,
            eager: None,
        };
        assert!(!default_tool_expanded(&card(ToolState::Done)));
        assert!(!default_tool_expanded(&card(ToolState::Running)));
        assert!(default_tool_expanded(&card(ToolState::Error)));
    }

    #[test]
    fn concise_mode_overrides_eager_tool_defaults() {
        let base = |show_result, eager| ToolCard {
            name: "task_loop".into(),
            state: ToolState::Done,
            args: None,
            label: None,
            show_result,
            eager,
        };
        assert!(!default_tool_expanded(&base(Some(true), None)));
        assert!(!default_tool_expanded(&base(Some(false), Some(true))));
        assert!(!default_tool_expanded(&base(None, Some(true))));
        assert!(!default_tool_expanded(&base(None, Some(false))));
    }

    #[test]
    fn prompt_surfaces_fit_content_and_stay_inside_narrow_panes() {
        for width in [320.0, 800.0] {
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx);
            ctx.set_style_of(ctx.theme(), crate::theme::ThemeSettings::default().style());
            let colors = ThemeColors::default();
            let mut bubble_widths = Vec::new();
            for text in [
                "Hello",
                "Please review **this change**.",
                &"long prompt ".repeat(150),
            ] {
                let blocks = markdown::parse_markdown(text);
                let mut right = 0.0;
                let mut output = ctx.run_ui(input(0, width), |ui| {
                    right = ui.max_rect().right();
                    let (mut expanded, mut chunks, mut resized) = (false, 1, false);
                    render_row_contents(
                        ui,
                        0,
                        "user",
                        None,
                        text,
                        &blocks,
                        &mut RowUi {
                            expanded: &mut expanded,
                            chunks: &mut chunks,
                            resized: &mut resized,
                        },
                        &colors,
                    );
                    assert!(ui.min_rect().height() < 10_000.0);
                });
                let bubble = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect) if rect.fill == colors.user_msg_bg => {
                            assert_eq!(rect.corner_radius, egui::CornerRadius::same(6));
                            Some(rect.rect)
                        }
                        _ => None,
                    })
                    .expect("prompt surface");
                assert!(
                    (bubble.right() - right).abs() < 2.0,
                    "{bubble:?}, right={right}"
                );
                assert!(bubble.left() >= 0.0 && bubble.width() <= width);
                if text == "Hello" {
                    assert!(bubble.height() < 48.0, "short prompt should stay compact");
                }
                bubble_widths.push(bubble.width());
                output.textures_delta.clear();
            }
            assert!(
                bubble_widths[0] < 100.0,
                "short prompts must hug their text"
            );
            assert!(bubble_widths[0] < bubble_widths[1]);
            assert!(bubble_widths[1] <= bubble_widths[2]);
        }
    }

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

    /// Run one headless frame and return every text color actually emitted into
    /// the frame's shapes, walking nested shape lists. This is how a render test
    /// proves a theme color change alters what egui draws, rather than only
    /// checking the resolved values in isolation.
    fn frame_text_colors(
        ctx: &egui::Context,
        input: egui::RawInput,
        ui_fn: impl FnMut(&mut Ui),
    ) -> Vec<egui::Color32> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<egui::Color32>) {
            match shape {
                egui::epaint::Shape::Text(t) => {
                    for section in &t.galley.job.sections {
                        out.push(section.format.color);
                    }
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = ctx.run_ui(input, ui_fn);
        let mut colors = Vec::new();
        for clipped in &out.shapes {
            walk(&clipped.shape, &mut colors);
        }
        out.textures_delta.clear();
        colors
    }

    /// Like [`frame_text_colors`] but collects each text section's background
    /// fill, so a render test can prove diff rows carry their `+`/`-` bands.
    fn frame_text_backgrounds(
        ctx: &egui::Context,
        input: egui::RawInput,
        ui_fn: impl FnMut(&mut Ui),
    ) -> Vec<egui::Color32> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<egui::Color32>) {
            match shape {
                egui::epaint::Shape::Text(t) => {
                    for section in &t.galley.job.sections {
                        out.push(section.format.background);
                    }
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = ctx.run_ui(input, ui_fn);
        let mut backgrounds = Vec::new();
        for clipped in &out.shapes {
            walk(&clipped.shape, &mut backgrounds);
        }
        out.textures_delta.clear();
        backgrounds
    }

    /// Collect the full text of every text galley drawn this frame, so a render
    /// test can prove which content the collapsed/expanded affordance shows.
    fn frame_texts(
        ctx: &egui::Context,
        input: egui::RawInput,
        ui_fn: impl FnMut(&mut Ui),
    ) -> Vec<String> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(t) => out.push(t.galley.job.text.clone()),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = ctx.run_ui(input, ui_fn);
        let mut texts = Vec::new();
        for clipped in &out.shapes {
            walk(&clipped.shape, &mut texts);
        }
        out.textures_delta.clear();
        texts
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
                    rows.push((
                        "tool: shell".into(),
                        "$ echo hello\nhello\nexit 0".to_string(),
                    ));
                    cards.push(Some(ToolCard {
                        name: "shell".into(),
                        state: ToolState::Done,
                        args: None,
                        label: None,
                        show_result: None,
                        eager: None,
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
                        egui::Frame::default()
                            .inner_margin(egui::Margin::symmetric(
                                0,
                                TRANSCRIPT_VERTICAL_PADDING as i8,
                            ))
                            .show(ui, |ui| {
                                let inner_spacing = ui.spacing().item_spacing.y;
                                ui.spacing_mut().item_spacing.y =
                                    TRANSCRIPT_ROW_GAP.max(inner_spacing);
                                for (i, ((role, text), blocks)) in
                                    rows.iter().zip(&parsed).enumerate()
                                {
                                    let blocks = blocks.as_deref().unwrap_or(&[]);
                                    let mut resized = false;
                                    let mut row = RowUi {
                                        expanded: &mut expanded[i],
                                        chunks: &mut chunks[i],
                                        resized: &mut resized,
                                    };
                                    egui::Frame::default()
                                        .inner_margin(egui::Margin::symmetric(
                                            TRANSCRIPT_HORIZONTAL_PADDING,
                                            0,
                                        ))
                                        .show(ui, |ui| {
                                            ui.spacing_mut().item_spacing.y = inner_spacing;
                                            render_row_contents(
                                                ui,
                                                i,
                                                role,
                                                cards[i].as_ref(),
                                                text,
                                                blocks,
                                                &mut row,
                                                &ThemeColors::default(),
                                            );
                                        });
                                }
                            });
                    });
                content_h = out.content_size.y;
            });
        }
        content_h
    }

    /// Reference render with the same default expansion state as the virtualizer.
    fn reference_content_height(rows: &[(String, String)], cards: &[Option<ToolCard>]) -> f32 {
        let expanded = cards
            .iter()
            .map(|card| card.as_ref().map(default_tool_expanded).unwrap_or(true))
            .collect();
        reference_content_height_at(rows, cards, expanded, vec![1; rows.len()])
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
                content_h = cache
                    .show(ui, TAB, true, rows, cards, &ThemeColors::default())
                    .content_size
                    .y;
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

    /// (a2) The transcript keeps symmetric vertical padding: the first and last
    /// rows never sit flush against the pane edges. A single row has no
    /// inter-row spacing, so its content height exceeds its own height by
    /// exactly the top plus bottom inset.
    #[test]
    fn transcript_pads_both_ends_vertically() {
        let rows = vec![("user".to_string(), "only message".to_string())];
        let cards = vec![None];
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        let mut content_h = 0.0;
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                content_h = cache
                    .show(ui, TAB, true, &rows, &cards, &ThemeColors::default())
                    .content_size
                    .y;
            });
        }
        let row_h = cache.heights[0];
        assert!(row_h > 0.0, "the row must be measured");
        let pad = TRANSCRIPT_VERTICAL_PADDING * 2.0;
        assert!(
            (content_h - row_h - pad).abs() <= 1.0,
            "content height {content_h}px != row {row_h}px + 2*{TRANSCRIPT_VERTICAL_PADDING}px inset"
        );
    }

    /// A live status line ("running shell: …") is drawn after the final row and
    /// participates in the scrollable content extent, so a running turn's
    /// progress scrolls with the conversation instead of living in the toolbar.
    #[test]
    fn live_status_line_renders_after_the_last_row_and_grows_content() {
        let rows = vec![("user".to_string(), "hello".to_string())];
        let cards = vec![None];
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        let mut plain_h = 0.0;
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                plain_h = cache
                    .show(ui, TAB, true, &rows, &cards, &ThemeColors::default())
                    .content_size
                    .y;
            });
        }
        let status = "running shell: cargo test";
        cache.set_status(Some(status.to_string()));
        let mut with_h = 0.0;
        let mut last_texts = Vec::new();
        for frame in 0..FRAMES {
            last_texts = frame_texts(&ctx, input(frame, W), |ui| {
                with_h = cache
                    .show(ui, TAB, true, &rows, &cards, &ThemeColors::default())
                    .content_size
                    .y;
            });
        }
        assert!(
            last_texts.iter().any(|t| t == status),
            "status line must render inside the transcript: {last_texts:?}"
        );
        assert!(
            with_h > plain_h,
            "status line must grow the transcript content ({with_h} vs {plain_h})"
        );
        // Clearing the status restores the original content extent.
        cache.set_status(None);
        let mut cleared_h = 0.0;
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                cleared_h = cache
                    .show(ui, TAB, true, &rows, &cards, &ThemeColors::default())
                    .content_size
                    .y;
            });
        }
        assert!(
            (cleared_h - plain_h).abs() <= 1.0,
            "cleared status must restore content height ({cleared_h} vs {plain_h})"
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
                cache.show(ui, TAB, true, &rows, &cards, &ThemeColors::default());
            });
        }
        assert!(cache.last_built < n / 4, "expected a warm frame first");
        // The cold frame parsed each non-tool row exactly once; warm frames
        // (and tool rows) never parse.
        assert_eq!(
            cache.parse_calls, non_tool,
            "cold parses must happen once per row"
        );
        // Resizing above the reading-width cap retains measured heights.
        run_frame(&ctx, input(FRAMES, 1000.0), |ui| {
            cache.show(ui, TAB, true, &rows, &cards, &ThemeColors::default());
        });
        assert!(cache.last_built < n / 4);
        // Below the cap: stale heights, so this frame measures all rows.
        run_frame(&ctx, input(FRAMES + 1, 600.0), |ui| {
            cache.show(ui, TAB, true, &rows, &cards, &ThemeColors::default());
        });
        assert_eq!(cache.last_built, n, "reflow frame must rebuild every row");
        assert_eq!(
            cache.parse_calls, non_tool,
            "width reflow must retain the parse cache, not reset parsing"
        );
        // Back to band-only rendering at the new width.
        run_frame(&ctx, input(FRAMES + 2, 600.0), |ui| {
            cache.show(ui, TAB, true, &rows, &cards, &ThemeColors::default());
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
                    label: None,
                    show_result: Some(true),
                    eager: None,
                })
            })
            .collect();

        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        cache.tool_verbosity = crate::layout::ToolVerbosity::Verbose;
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                cache.show(ui, TAB, true, &rows, &cards, &ThemeColors::default());
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
            (virtual_h - reference_h).abs() <= 4.0,
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
            label: None,
            show_result: Some(true),
            eager: None,
        })];
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        cache.tool_verbosity = crate::layout::ToolVerbosity::Verbose;
        let mut virtual_h = 0.0;
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                virtual_h = cache
                    .show(ui, TAB, true, &rows, &cards, &ThemeColors::default())
                    .content_size
                    .y;
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
        let reference_h =
            reference_content_height_at(&rows, &cards, vec![true; rows.len()], vec![1; rows.len()]);
        assert!(
            (virtual_h - reference_h).abs() <= 2.0,
            "virtual content {virtual_h}px != reference {reference_h}px"
        );
    }

    /// Expanded shell output keeps gutters and merges the framed output streams.
    #[test]
    fn expanded_shell_rows_keep_gutters_and_output() {
        // Wire framing: exit code + stdout + stderr, with a trailing blank
        // stdout line that must not become a leading blank gutter row.
        let framed = "exit code: 0\nstdout:\nout line 1\n\nstderr:\nerr line 1\n";
        assert_eq!(
            shell_output_lines(framed),
            vec!["out line 1".to_string(), "err line 1".to_string()]
        );

        // No framing: raw lines are returned verbatim.
        assert_eq!(
            shell_output_lines("plain a\nplain b"),
            vec!["plain a".to_string(), "plain b".to_string()]
        );

        // Expanded render includes all ten output lines.
        let text = (0..10)
            .map(|i| format!("row {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let colors = ThemeColors::default();
        let card = ToolCard {
            name: "shell".into(),
            state: ToolState::Done,
            args: Some(r#"{"command":"synthetic"}"#.into()),
            label: None,
            show_result: None,
            eager: None,
        };
        let ctx = egui::Context::default();
        let mut expanded = true;
        let mut chunks = 1usize;
        let mut resized = false;
        let mut row = RowUi {
            expanded: &mut expanded,
            chunks: &mut chunks,
            resized: &mut resized,
        };
        let texts = frame_texts(&ctx, input(0, W), |ui| {
            render_tool_row(ui, "tool: shell", Some(&card), &text, &mut row, &colors);
        });
        let joined = texts.join("");
        assert!(joined.contains("row 0"), "first line shown: {joined:?}");
        assert!(joined.contains("row 9"), "last line shown: {joined:?}");
        assert!(
            !joined.contains("⋮ +6 terminal lines"),
            "expanded output must not contain the old preview hint: {joined:?}"
        );
        assert!(
            joined.contains('│') && joined.contains('╰'),
            "gutter drawn: {joined:?}"
        );
        assert!(
            joined.contains("row 5"),
            "expanded output must include middle lines: {joined:?}"
        );

        // An explicit display rule still hides the shell result completely.
        let hidden = ToolCard {
            show_result: Some(false),
            ..card
        };
        let hidden_texts = frame_texts(&egui::Context::default(), input(0, W), |ui| {
            render_tool_row(ui, "tool: shell", Some(&hidden), &text, &mut row, &colors);
        });
        assert!(!hidden_texts.join("").contains("row 0"));
    }

    #[test]
    fn tool_disclosure_and_copy_buttons_accept_pointer_input() {
        fn text_shapes(shape: &egui::Shape, found: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(text) => found.push((
                    text.galley.job.text.clone(),
                    text.galley.rect.translate(text.pos.to_vec2()),
                )),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text_shapes(shape, found);
                    }
                }
                _ => {}
            }
        }

        for width in [320.0, 800.0] {
            for show_result in [None, Some(false)] {
                let ctx = egui::Context::default();
                crate::theme::install_fonts(&ctx);
                ctx.set_style_of(ctx.theme(), crate::theme::ThemeSettings::default().style());
                let card = ToolCard {
                    name: "diagnostics".into(),
                    state: ToolState::Done,
                    args: Some(r#"{"path":"synthetic/example.rs"}"#.into()),
                    label: Some("Inspect synthetic output with a descriptive tool label".into()),
                    show_result,
                    eager: None,
                };
                let text = (0..301)
                    .map(|i| format!("synthetic-output-{i:03}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut expanded = false;
                let mut resized = false;
                let mut chunks = 1;
                let mut frame = 0;
                // Drive actual egui pointer events, not cache-state mutations.
                // Clipboard commands remain headless; no system clipboard is used.
                let mut render = |events| {
                    let mut raw = input(frame, width);
                    frame += 1;
                    raw.events = events;
                    let mut disclosure = egui::Pos2::ZERO;
                    let mut out = ctx.run_ui(raw, |ui| {
                        disclosure = ui.cursor().min + egui::vec2(13.0, 12.0);
                        render_tool_row(
                            ui,
                            "tool: diagnostics",
                            Some(&card),
                            &text,
                            &mut RowUi {
                                expanded: &mut expanded,
                                chunks: &mut chunks,
                                resized: &mut resized,
                            },
                            &ThemeColors::default(),
                        );
                    });
                    let mut texts = Vec::new();
                    for shape in &out.shapes {
                        text_shapes(&shape.shape, &mut texts);
                    }
                    out.textures_delta.clear();
                    (out, texts, disclosure, expanded, resized)
                };
                let pointer = |pos, pressed| {
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::default(),
                        },
                    ]
                };
                for _ in 0..3 {
                    render(Vec::new());
                }
                let (_, texts, disclosure, open, _) = render(Vec::new());
                assert!(!open);
                assert!(
                    !texts
                        .iter()
                        .any(|(s, _)| s == "Done" || s == "Copy" || s == "Copy args")
                );
                assert!(!texts.iter().any(|(s, _)| s.contains("synthetic-output-")));
                render(pointer(disclosure, true));
                let (_, texts, _, open, resized) = render(pointer(disclosure, false));
                assert!(open && resized, "disclosure failed at width {width}");
                assert!(texts.iter().any(|(s, _)| s == card.args.as_ref().unwrap()));
                assert_eq!(
                    texts
                        .iter()
                        .any(|(s, _)| s.contains("synthetic-output-000")),
                    show_result != Some(false)
                );
                assert!(
                    !texts
                        .iter()
                        .any(|(s, _)| s.contains("synthetic-output-300"))
                );

                for (label, expected) in [
                    ("Copy", text.as_str()),
                    ("Copy args", card.args.as_deref().unwrap()),
                ] {
                    let (_, texts, _, _, _) = render(Vec::new());
                    let pos = texts.iter().find(|(s, _)| s == label).unwrap().1.center();
                    render(pointer(pos, true));
                    let (out, _, _, _, _) = render(pointer(pos, false));
                    assert!(out.platform_output.commands.iter().any(|command| {
                        matches!(command, egui::OutputCommand::CopyText(copied) if copied == expected)
                    }), "{label} failed at width {width}");
                }
                render(pointer(disclosure, true));
                let (_, texts, _, open, _) = render(pointer(disclosure, false));
                assert!(!open);
                assert!(
                    !texts
                        .iter()
                        .any(|(s, _)| s == "Done" || s == "Copy" || s == "Copy args")
                );
                assert!(!texts.iter().any(|(s, _)| s.contains("synthetic-output-")));
                assert!(!texts.iter().any(|(s, _)| s == card.args.as_ref().unwrap()));
            }
        }
    }

    fn painted_text(output: &egui::FullOutput) -> Vec<egui::epaint::TextShape> {
        fn collect(shape: &egui::Shape, texts: &mut Vec<egui::epaint::TextShape>) {
            match shape {
                egui::Shape::Text(text) => texts.push(text.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, texts);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut texts);
        }
        texts
    }

    #[test]
    fn tool_args_and_result_use_distinct_scroll_area_ids() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let card = ToolCard {
            name: "diagnostics".into(),
            state: ToolState::Done,
            args: Some(r#"{"path":"src/main.rs"}"#.into()),
            label: None,
            show_result: None,
            eager: None,
        };
        let (mut expanded, mut chunks, mut resized) = (true, 1, false);
        let mut output = ctx.run_ui(input(0, 800.0), |ui| {
            render_tool_row(
                ui,
                "tool: diagnostics",
                Some(&card),
                "diagnostic output",
                &mut RowUi {
                    expanded: &mut expanded,
                    chunks: &mut chunks,
                    resized: &mut resized,
                },
                &ThemeColors::default(),
            );
        });
        let clashes: Vec<_> = painted_text(&output)
            .into_iter()
            .map(|text| text.galley.job.text.clone())
            .filter(|text| text.contains("use of ScrollArea ID"))
            .collect();
        output.textures_delta.clear();
        assert!(clashes.is_empty(), "{clashes:?}");
    }

    #[test]
    fn chunk_footers_fit_narrow_panes_and_accept_show_more_clicks() {
        for name in ["diagnostics", "shell"] {
            for width in [240.0, 320.0, 800.0] {
                let ctx = egui::Context::default();
                crate::theme::install_fonts(&ctx);
                ctx.set_style_of(ctx.theme(), crate::theme::ThemeSettings::default().style());
                let rows = vec![
                    (
                        format!("tool: {name}"),
                        (0..451).map(|i| format!("line {i:03}\n")).collect(),
                    ),
                    ("assistant".into(), "Following answer".into()),
                ];
                let cards = vec![
                    Some(ToolCard {
                        name: name.into(),
                        state: ToolState::Done,
                        args: None,
                        label: None,
                        show_result: Some(true),
                        eager: None,
                    }),
                    None,
                ];
                let mut cache = Cache::new();
                cache.tool_verbosity = crate::layout::ToolVerbosity::Verbose;
                let mut frame = 0;
                let mut render = |events| {
                    let mut raw = input(frame, width);
                    frame += 1;
                    raw.events = events;
                    let mut pane = Rect::NOTHING;
                    let mut output = ctx.run_ui(raw, |ui| {
                        pane = ui.max_rect();
                        let out = cache.show(ui, TAB, true, &rows, &cards, &ThemeColors::default());
                        assert!(
                            out.content_size.x <= pane.width() + 1.0,
                            "{name} at {width}: content widened to {}",
                            out.content_size.x
                        );
                    });
                    output.textures_delta.clear();
                    let texts = painted_text(&output);
                    for text in &texts {
                        if text.galley.job.text.starts_with("Show ")
                            || text.galley.job.text.contains("Copy for full output")
                            || text.galley.job.text == "Following answer"
                        {
                            let rect = text.galley.rect.translate(text.pos.to_vec2());
                            assert!(
                                rect.left() >= pane.left() && rect.right() <= pane.right() + 1.0,
                                "{name} at {width}: {:?} outside {pane:?}: {rect:?}",
                                text.galley.job.text
                            );
                        }
                    }
                    if let Some(answer) = texts
                        .iter()
                        .find(|t| t.galley.job.text == "Following answer")
                    {
                        let expected = pane.left() + f32::from(TRANSCRIPT_HORIZONTAL_PADDING);
                        assert!(
                            (answer.pos.x - expected).abs() <= 1.0,
                            "{name} at {width}: following answer shifted to {} from {expected}",
                            answer.pos.x
                        );
                    }
                    (texts, cache.chunks[0], cache.heights[0])
                };
                let pointer = |pos, pressed| {
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::default(),
                        },
                    ]
                };
                for _ in 0..FRAMES {
                    render(Vec::new());
                }
                let (_, _, first_height) = render(Vec::new());
                for (label, expected_chunks) in
                    [("Show 200 more lines…", 2), ("Show 51 more lines…", 3)]
                {
                    let (texts, _, _) = render(Vec::new());
                    let button = texts
                        .iter()
                        .find(|t| t.galley.job.text == label)
                        .expect("show more button");
                    let button_rect = button.galley.rect.translate(button.pos.to_vec2());
                    let hint = texts
                        .iter()
                        .find(|t| t.galley.job.text.contains("Copy for full output"))
                        .expect("remaining output hint");
                    if width <= 320.0 {
                        assert!(
                            hint.pos.y >= button_rect.bottom(),
                            "narrow footer hint must stack below button"
                        );
                    } else {
                        assert!(
                            (hint.pos.y - button.pos.y).abs() < 4.0,
                            "wide footer should stay on one row"
                        );
                    }
                    let pos = button_rect.center();
                    render(pointer(pos, true));
                    let (_, chunks, height) = render(pointer(pos, false));
                    assert_eq!(chunks, expected_chunks);
                    assert_eq!(height, 0.0, "click must invalidate the measured height");
                    for _ in 0..FRAMES {
                        render(Vec::new());
                    }
                    let (_, _, height) = render(Vec::new());
                    assert!(height > first_height, "expanded chunk must be remeasured");
                }
                let (texts, _, _) = render(Vec::new());
                assert!(!texts.iter().any(|t| t.galley.job.text.starts_with("Show ")));
                assert!(texts.iter().any(|t| t.galley.job.text.contains("line 450")));
            }
        }
    }

    #[test]
    fn tool_errors_wrap_without_losing_raw_text_or_newlines() {
        let text = format!(
            "could not resolve `{}`: No such file or directory (os error 2)\n\n    **literal** error details ✓",
            "native/long_missing_path_".repeat(12)
        );
        for width in [240.0, 320.0, 800.0] {
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx);
            ctx.set_style_of(ctx.theme(), crate::theme::ThemeSettings::default().style());
            let card = ToolCard {
                name: "read_file".into(),
                state: ToolState::Error,
                args: None,
                label: None,
                show_result: None,
                eager: None,
            };
            let mut output = ctx.run_ui(input(0, width), |ui| {
                let (mut expanded, mut chunks, mut resized) = (true, 1, false);
                render_tool_row(
                    ui,
                    "tool: read_file",
                    Some(&card),
                    &text,
                    &mut RowUi {
                        expanded: &mut expanded,
                        chunks: &mut chunks,
                        resized: &mut resized,
                    },
                    &ThemeColors::default(),
                );
            });
            output.textures_delta.clear();
            let (body, clip) = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(body) if body.galley.text() == text => {
                        Some((body, shape.clip_rect))
                    }
                    _ => None,
                })
                .expect("error remains a single raw selectable label");
            let rect = body.galley.rect.translate(body.pos.to_vec2());
            assert!(
                rect.left() >= clip.left() && rect.right() <= clip.right() + 1.0,
                "error extends beyond its viewport at {width}: {rect:?}, clip={clip:?}"
            );
            assert!(body.galley.rows.len() > 3, "long error should wrap");
            assert_eq!(
                body.galley
                    .rows
                    .iter()
                    .filter(|row| row.ends_with_newline)
                    .count(),
                2
            );
        }
    }

    #[test]
    fn wrapped_shell_output_keeps_raw_text_aligned_beside_gutter() {
        let text = format!(
            "{}\n\n    indented **literal** output\n{}",
            "wrapped output ".repeat(12),
            "x".repeat(160)
        );
        for width in [240.0, 320.0, 800.0] {
            for is_error in [false, true] {
                let ctx = egui::Context::default();
                crate::theme::install_fonts(&ctx);
                ctx.set_style_of(ctx.theme(), crate::theme::ThemeSettings::default().style());
                let colors = ThemeColors::default();
                let mut output = ctx.run_ui(input(0, width), |ui| {
                    let (mut expanded, mut chunks, mut resized) = (true, 1, false);
                    let right = ui.max_rect().right();
                    render_shell_body(
                        ui,
                        &text,
                        &mut RowUi {
                            expanded: &mut expanded,
                            chunks: &mut chunks,
                            resized: &mut resized,
                        },
                        is_error,
                        &colors,
                    );
                    assert!(ui.min_rect().right() <= right + 1.0);
                });
                output.textures_delta.clear();
                let texts = painted_text(&output);
                let body = texts.iter().find(|t| t.galley.job.text == text).expect(
                    "shell output must remain one selectable raw label, without gutter characters",
                );
                assert!(body.galley.rows.len() > 4, "fixture must wrap");
                assert!(body.galley.rows.iter().all(|row| row.pos.x.abs() < 1.0));
                assert_eq!(
                    body.galley
                        .rows
                        .iter()
                        .filter(|row| row.ends_with_newline)
                        .count(),
                    3
                );
                let gutters: Vec<_> = texts
                    .iter()
                    .filter(|t| matches!(t.galley.job.text.as_str(), "│" | "╰"))
                    .collect();
                assert_eq!(gutters.len(), body.galley.rows.len());
                for (idx, (gutter, row)) in gutters.iter().zip(&body.galley.rows).enumerate() {
                    assert!(gutter.pos.x + gutter.galley.size().x < body.pos.x);
                    assert!((gutter.pos.y - body.pos.y - row.pos.y).abs() < 1.0);
                    assert_eq!(
                        gutter.galley.job.text,
                        if idx + 1 == gutters.len() {
                            "╰"
                        } else {
                            "│"
                        }
                    );
                    assert_eq!(
                        gutter.galley.job.sections[0].format.color,
                        if is_error {
                            colors.tool_error
                        } else {
                            colors.tool_call
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn collapsed_shell_render_hides_args_and_output() {
        let card = ToolCard {
            name: "shell".into(),
            state: ToolState::Done,
            args: Some("echo hidden args".into()),
            label: Some("shell command".into()),
            show_result: None,
            eager: None,
        };
        let text = "exit code: 0\nstdout:\nvisible shell output";
        let blocks = Vec::new();
        let mut expanded = false;
        let mut chunks = 1;
        let mut resized = false;
        let rendered = frame_texts(&egui::Context::default(), input(0, W), |ui| {
            render_row_contents(
                ui,
                0,
                "tool: shell",
                Some(&card),
                text,
                &blocks,
                &mut RowUi {
                    expanded: &mut expanded,
                    chunks: &mut chunks,
                    resized: &mut resized,
                },
                &ThemeColors::default(),
            );
        })
        .join("\n");
        assert!(
            rendered.contains("shell command"),
            "summary missing: {rendered:?}"
        );
        assert!(
            !rendered.contains("echo hidden args"),
            "args leaked: {rendered:?}"
        );
        assert!(
            !rendered.contains("visible shell output"),
            "collapsed shell output leaked: {rendered:?}"
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
            label: None,
            show_result: Some(true),
            eager: None,
        })];
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        cache.tool_verbosity = crate::layout::ToolVerbosity::Verbose;
        let mut h1 = 0.0;
        run_frame(&ctx, input(0, W), |ui| {
            h1 = cache
                .show(ui, TAB, true, &rows, &cards, &ThemeColors::default())
                .content_size
                .y;
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
        assert!(
            (h1 - reference_content_height_at(
                &rows,
                &cards,
                vec![true; rows.len()],
                vec![1; rows.len()]
            ))
            .abs()
                <= 2.0
        );

        // Simulate a "Show more lines" click: one more chunk, re-measured.
        cache.chunks[0] = 2;
        cache.heights[0] = 0.0;
        cache.unknown += 1;
        let mut h2 = 0.0;
        run_frame(&ctx, input(1, W), |ui| {
            h2 = cache
                .show(ui, TAB, true, &rows, &cards, &ThemeColors::default())
                .content_size
                .y;
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

    #[test]
    fn disclosure_choice_survives_stream_updates_and_completion() {
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        cache.tool_verbosity = crate::layout::ToolVerbosity::Verbose;
        let mut rows = vec![("tool: inspect".into(), "first output".into())];
        let mut cards = vec![Some(ToolCard {
            name: "inspect".into(),
            state: ToolState::Running,
            args: None,
            label: None,
            show_result: None,
            eager: None,
        })];
        let mut disclosure = egui::Pos2::ZERO;
        for frame in 0..3 {
            let mut output = ctx.run_ui(input(frame, W), |ui| {
                cache.show(ui, TAB, true, &rows, &cards, &ThemeColors::default());
            });
            output.textures_delta.clear();
            // The run heading is the clickable disclosure; a running tool also
            // paints a leading spinner, so derive the target from the painted
            // label rather than a fixed offset.
            let heading = painted_text(&output)
                .into_iter()
                .find(|shape| shape.galley.job.text == "inspect")
                .expect("tool heading is painted");
            disclosure = heading
                .galley
                .rect
                .translate(heading.pos.to_vec2())
                .center();
        }
        assert!(cache.expanded[0]);
        for (frame, pressed) in [(3, true), (4, false)] {
            let mut raw = input(frame, W);
            raw.events = vec![
                egui::Event::PointerMoved(disclosure),
                egui::Event::PointerButton {
                    pos: disclosure,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ];
            run_frame(&ctx, raw, |ui| {
                cache.show(ui, TAB, true, &rows, &cards, &ThemeColors::default());
            });
        }
        assert!(!cache.expanded[0], "pointer must collapse the running tool");
        rows[0].1.push_str("\nsecond output");
        cache.sync_with_cards(rows.len(), &[0], &cards);
        assert!(
            !cache.expanded[0],
            "streaming must preserve the disclosure choice"
        );
        cards[0].as_mut().unwrap().state = ToolState::Done;
        cache.sync_with_cards(rows.len(), &[0], &cards);
        assert!(!cache.expanded[0]);
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
            label: None,
            show_result: Some(true),
            eager: None,
        }));
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        cache.tool_verbosity = crate::layout::ToolVerbosity::Verbose;
        let mut expanded_h = 0.0;
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                expanded_h = cache
                    .show(ui, TAB, true, &rows, &cards, &ThemeColors::default())
                    .content_size
                    .y;
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
            collapsed_h = cache
                .show(ui, TAB, true, &rows, &cards, &ThemeColors::default())
                .content_size
                .y;
        });
        assert!(
            cache.heights[tool] > 0.0,
            "collapsed row must be re-measured next frame"
        );
        assert!(
            cache.heights[tool] < 200.0,
            "collapsed row shows a bounded preview (first/last lines), not 120 lines (got {}px)",
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

    /// The parsed theme must reach the renderers: a role's configured color has
    /// to appear in the shapes egui actually draws, and a different theme must
    /// change that styling. This is the N-04 done-condition at the render level.
    #[test]
    fn theme_colors_drive_rendered_role_styling() {
        let user = egui::Color32::from_rgb(0x11, 0x22, 0x33);
        let thinking = egui::Color32::from_rgb(0x44, 0x55, 0x66);
        let colors = ThemeColors {
            user_msg: user,
            thinking,
            ..Default::default()
        };

        let ctx = egui::Context::default();
        let seen = frame_text_colors(&ctx, input(0, W), |ui| {
            role_heading(ui, "user", &colors);
            role_heading(ui, "reasoning", &colors);
        });
        assert!(
            seen.contains(&user),
            "user heading must render in user_msg color, saw {seen:?}"
        );
        assert!(
            seen.contains(&thinking),
            "reasoning heading must render in thinking color, saw {seen:?}"
        );

        // A different theme produces different styling for the same role.
        let other = egui::Color32::from_rgb(0xaa, 0xbb, 0xcc);
        let recolored = ThemeColors {
            user_msg: other,
            ..Default::default()
        };
        let ctx2 = egui::Context::default();
        let seen2 = frame_text_colors(&ctx2, input(0, W), |ui| {
            role_heading(ui, "user", &recolored);
        });
        assert!(
            seen2.contains(&other),
            "recolored user heading must render in the new color, saw {seen2:?}"
        );
        assert!(
            !seen2.contains(&user),
            "old user_msg color must not survive a theme change"
        );
    }

    /// A failed tool call marks its heading with the theme's error color while a
    /// successful call keeps the quiet tool-call color. The heading is the only
    /// error affordance — there is no separate `error` tag.
    #[test]
    fn error_tool_heading_uses_error_color() {
        let colors = ThemeColors::default();
        assert_ne!(
            colors.tool_error, colors.tool_call,
            "the default theme must distinguish error and tool-call colors"
        );

        let render = |card: &ToolCard| {
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx);
            ctx.set_style_of(ctx.theme(), crate::theme::ThemeSettings::default().style());
            frame_text_colors(&ctx, input(0, W), |ui| {
                let (mut expanded, mut chunks, mut resized) = (false, 1usize, false);
                render_tool_row(
                    ui,
                    "tool: read_file",
                    Some(card),
                    "boom",
                    &mut RowUi {
                        expanded: &mut expanded,
                        chunks: &mut chunks,
                        resized: &mut resized,
                    },
                    &colors,
                );
            })
        };

        let error = render(&ToolCard {
            name: "read_file".into(),
            state: ToolState::Error,
            args: None,
            label: None,
            show_result: None,
            eager: None,
        });
        assert!(
            error.contains(&colors.tool_error),
            "error heading must render in tool_error, saw {error:?}"
        );

        let done = render(&ToolCard {
            name: "read_file".into(),
            state: ToolState::Done,
            args: None,
            label: None,
            show_result: None,
            eager: None,
        });
        assert!(
            done.contains(&colors.tool_call),
            "successful heading keeps the quiet tool color, saw {done:?}"
        );
        assert!(
            !done.contains(&colors.tool_error),
            "successful heading must not be red, saw {done:?}"
        );
    }

    /// N-06 done-condition at the render level: the reasoning affordance is live
    /// and collapsed — collapsed shows only the tail line, expanding reveals the
    /// full reasoning body.
    #[test]
    fn reasoning_row_is_collapsed_by_default_and_expands() {
        let colors = ThemeColors::default();
        let text = "first reasoning line\nmiddle reasoning line\ntail reasoning line";
        let blocks = markdown::parse_markdown(text);

        let collapsed = frame_texts(&egui::Context::default(), input(0, W), |ui| {
            let (mut expanded, mut chunks, mut resized) = (false, 0usize, false);
            render_row_contents(
                ui,
                0,
                "reasoning",
                None,
                text,
                &blocks,
                &mut RowUi {
                    expanded: &mut expanded,
                    chunks: &mut chunks,
                    resized: &mut resized,
                },
                &colors,
            );
        });
        let collapsed = collapsed.join("\n");
        assert!(
            collapsed.contains("tail reasoning line"),
            "collapsed shows the live tail, saw {collapsed:?}"
        );
        assert!(
            !collapsed.contains("first reasoning line"),
            "collapsed hides the reasoning body, saw {collapsed:?}"
        );

        let expanded = frame_texts(&egui::Context::default(), input(0, W), |ui| {
            let (mut expanded, mut chunks, mut resized) = (true, 0usize, false);
            render_row_contents(
                ui,
                0,
                "reasoning",
                None,
                text,
                &blocks,
                &mut RowUi {
                    expanded: &mut expanded,
                    chunks: &mut chunks,
                    resized: &mut resized,
                },
                &colors,
            );
        });
        let expanded = expanded.join("\n");
        assert!(
            expanded.contains("first reasoning line"),
            "expanded shows the reasoning body, saw {expanded:?}"
        );
        assert!(
            expanded.contains("tail reasoning line"),
            "expanded keeps the tail, saw {expanded:?}"
        );
    }

    /// N-05 done-condition end-to-end: a single message carrying a table, a task
    /// list, and a highlighted code block renders together, with the table
    /// header painted in its theme color and the code block emitting more than
    /// one token color.
    #[test]
    fn table_tasklist_and_code_render_together() {
        let mut colors = ThemeColors::default();
        let header = egui::Color32::from_rgb(0x0a, 0x0b, 0x0c);
        colors.markdown_table_header = header;

        let message = "## Report\n\n| Name | Qty |\n| --- | ---: |\n| a | 1 |\n\n- [x] done\n- [ ] todo\n\n```rust\nfn main() { let x = 42; }\n```\n";
        let blocks = markdown::parse_markdown(message);

        let ctx = egui::Context::default();
        let mut expanded = false;
        let mut chunks = 0usize;
        let mut resized = false;
        let seen = frame_text_colors(&ctx, input(0, W), |ui| {
            let mut row = RowUi {
                expanded: &mut expanded,
                chunks: &mut chunks,
                resized: &mut resized,
            };
            render_row_contents(
                ui,
                0,
                "assistant",
                None,
                message,
                &blocks,
                &mut row,
                &colors,
            );
        });
        assert!(
            seen.contains(&header),
            "table header must render in markdown_table_header, saw {seen:?}"
        );
        let distinct: std::collections::HashSet<_> = seen.iter().collect();
        assert!(
            distinct.len() >= 3,
            "expected heading, table and code token colors, saw {distinct:?}"
        );
    }

    /// N-05 done-condition at the render level: a diff row (`system` content
    /// starting with `\n`) must paint `+` lines with the added background and
    /// `-` lines with the removed background.
    #[test]
    fn diff_rows_render_added_and_removed_backgrounds() {
        let colors = ThemeColors::default();
        let content = "\n    1 +added line\n    2 -removed line\n    3  context line\n";
        let ctx = egui::Context::default();
        let backgrounds = frame_text_backgrounds(&ctx, input(0, W), |ui| {
            let blocks = markdown::parse_markdown(content);
            let mut expanded = false;
            let mut chunks = 0;
            let mut resized = false;
            let mut row = RowUi {
                expanded: &mut expanded,
                chunks: &mut chunks,
                resized: &mut resized,
            };
            render_row_contents(ui, 0, "system", None, content, &blocks, &mut row, &colors);
        });
        assert!(
            backgrounds.contains(&colors.diff_added),
            "expected an added-line background, got {backgrounds:?}"
        );
        assert!(
            backgrounds.contains(&colors.diff_removed),
            "expected a removed-line background, got {backgrounds:?}"
        );
    }
}
