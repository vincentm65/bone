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
//! - User/assistant rows use Markdown with a role hierarchy: users are
//!   accented with the theme link color and headings are selectable.
//! - Reasoning is off by default and, when surfaced, is never a peer row.
//!   In-progress reasoning shows as a trailing "✻ Thinking…" badge that scrolls
//!   with the chat; settled reasoning attaches to the row it produced as a faint
//!   "✻" that reveals a small, dim, headings-flattened body. A reasoning-only
//!   message (no answer, no tool call) keeps a quiet "Thinking" disclosure.
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
/// Compact separation between transcript messages.
const TRANSCRIPT_ROW_GAP: f32 = 20.0;
/// Footprint of the live reasoning badge's painted marker. The marker is drawn
/// with the painter only, inside an allocation of exactly this size, so the
/// animation can never change a laid-out dimension.
const LIVE_MARKER_SIZE: f32 = 16.0;
/// Gap between the live marker and its "Thinking" caption.
const LIVE_MARKER_GAP: f32 = 6.0;
/// Seconds per full turn of the live marker (its "✻" has three-fold symmetry
/// per axis, so a turn reads as a steady spin rather than a snap).
const LIVE_TURN_SECONDS: f32 = 3.2;
/// Seconds per pulse of the live marker's brightness.
const LIVE_PULSE_SECONDS: f32 = 1.6;
/// Fixed height of the expanded live panel. The stream scrolls inside this box,
/// so the badge occupies one constant height while text streams in: a growing
/// badge would push the transcript (pinned to the bottom) around every frame.
const LIVE_PANEL_HEIGHT: f32 = 160.0;
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
    /// Per-row disclosure for the thinking attached to a row (independent of the
    /// tool output `expanded` state). Collapsed by default.
    thinking_expanded: Vec<bool>,
    /// Whether the trailing live-reasoning badge is expanded into its stream.
    /// Measured height of the live-reasoning badge, reused as its layout bound
    /// next frame (0.0 means "not measured yet").
    live_height: f32,
    live_expanded: bool,
    /// Layer and `[start, end)` shape-index range the transcript rows painted
    /// into this frame, so `show_with` can nudge exactly those shapes when the
    /// pinned scroll offset lags a content change (see `cancel_bottom_growth`).
    paint: Option<(egui::LayerId, usize, usize)>,
    /// Scroll offset (`viewport.min.y`) the rows were painted with this frame.
    /// egui applies the stick-to-bottom correction in its scroll area's `end()`,
    /// after the rows are placed, so a pinned transcript is painted with the
    /// previous frame's offset and snaps to the new bottom one frame later.
    /// `cancel_bottom_growth` translates the painted rows by the difference.
    paint_offset: f32,
    /// The scroll offset of the previous frame's *pinned* bottom, when it stuck
    /// there. A pinned transcript lays its rows out with the previous frame's
    /// (stale) offset, so the bottom rows can sit below the viewport, where egui
    /// culls them from painting entirely. `layout_rows` widens the clip for those
    /// rows only while the offset being laid out with still matches this (a
    /// drag-scroll updates the offset before layout, so the widened clip is not
    /// applied once the user has dragged away). `cancel_bottom_growth` then lands
    /// the rows in place and re-clips the band to the viewport, which is what
    /// actually guarantees nothing leaks when the user scrolls away (wheel scroll
    /// is applied *after* layout, so the gate alone cannot cover it).
    pinned_offset: Option<f32>,
    /// Height (relative to the content top) the rows were actually laid out to
    /// this frame, recorded only when the final row was placed so it is exact.
    /// `ui.set_height(total)` runs *before* the rows are built, so a row that
    /// shrinks mid-layout (an expanded thinking body collapsing) leaves the
    /// content region reporting the previous, taller size. `cancel_bottom_growth`
    /// uses this true height so a pinned view lands on the new bottom the same
    /// frame instead of jolting the rows below the shrunk row for one frame.
    layout_bottom: Option<f32>,
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
            thinking_expanded: Vec::new(),
            live_height: 0.0,
            live_expanded: false,
            paint: None,
            paint_offset: 0.0,
            pinned_offset: None,
            layout_bottom: None,
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
    /// authoritative card lifecycle. This must run before `show`: new rows are
    /// created collapsed, and only tool cards may open automatically here.
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
        // New rows start collapsed. Attached thinking stays a faint "✻" until the
        // user opens it, instead of dumping every thought block into the chat;
        // tool rows open per their lifecycle.
        self.expanded.resize(rows_len, false);
        self.expansion_chosen.resize(rows_len, false);
        self.chunks.resize(rows_len, 1);
        self.thinking_expanded.resize(rows_len, false);
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

    /// Test-friendly entry point: a transcript with no reasoning. No-reasoning
    /// tests keep their call sites; reasoning-specific tests use
    /// [`Self::show_with`] directly.
    #[cfg(test)]
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        tab_id: u64,
        stick_to_bottom: bool,
        rows: &[(String, String)],
        toolcards: &[Option<ToolCard>],
        colors: &ThemeColors,
    ) -> egui::containers::scroll_area::ScrollAreaOutput<()> {
        self.show_with(
            ui,
            tab_id,
            stick_to_bottom,
            rows,
            toolcards,
            &[],
            None,
            colors,
        )
    }

    /// Show the transcript, laying out only the rows near the visible band.
    /// Returns the scroll-area output so the caller can keep its at-bottom
    /// bookkeeping (`content_size` always reflects the full transcript).
    ///
    /// `thinking` is index-aligned with `rows` (the reasoning that produced each
    /// row); `live_reasoning` is the running turn's in-progress reasoning, drawn
    /// as a trailing badge after the last row.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn show_with(
        &mut self,
        ui: &mut Ui,
        tab_id: u64,
        stick_to_bottom: bool,
        rows: &[(String, String)],
        toolcards: &[Option<ToolCard>],
        thinking: &[Option<String>],
        live_reasoning: Option<&str>,
        colors: &ThemeColors,
    ) -> egui::containers::scroll_area::ScrollAreaOutput<()> {
        if self.heights.len() != rows.len() {
            self.sync_with_cards(rows.len(), &[], toolcards);
        }
        ui.set_min_width(ui.available_width());
        ui.set_max_width(ui.available_width());
        let status = self.status.clone();
        let out = egui::ScrollArea::vertical()
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
                    thinking,
                    live_reasoning,
                    colors,
                    status.as_deref(),
                )
            });
        self.cancel_bottom_growth(&out, ui, stick_to_bottom);
        out
    }

    /// Undo egui's one-frame sticky-bottom lag. When the transcript is pinned to
    /// the end, egui corrects the scroll offset in its scroll area's `end()`,
    /// *after* the rows are placed, so a pinned transcript is painted with the
    /// previous frame's offset and only reaches the new bottom one frame later.
    /// That one-frame lag is what makes a streaming turn lurch. egui tells us the
    /// offset it actually painted with through the viewport it hands the layout
    /// (`paint_offset`); the offset it will paint with next frame is the new
    /// bottom (`max_scroll`), so translating this frame's rows by the difference
    /// makes the pinned view hold still. Does nothing unless the caller asked to
    /// pin and egui actually stuck to the end, so an ordinary scrolled-up view is
    /// left untouched.
    fn cancel_bottom_growth(
        &mut self,
        out: &egui::containers::scroll_area::ScrollAreaOutput<()>,
        ui: &mut Ui,
        stick_to_bottom: bool,
    ) {
        // egui reports the content size it will pin the offset to. `ui.set_height`
        // ran with the pre-layout (previous frame's) heights, so a row that shrank
        // mid-layout leaves the reported size one row too tall: egui pins the offset
        // to that stale bottom while the rows below the shrunk row were already
        // placed at the new, smaller height, jolting them for a frame. Use the true
        // height the rows were actually laid out to (recorded only when the final
        // row was placed) to compute the pinned target, but keep `stuck` keyed to
        // the reported size — that is the offset egui just pinned the state to.
        let content_height = out.content_size.y;
        let reported_scroll = (content_height - out.inner_rect.height()).max(0.0);
        // egui rewrites the offset to the true bottom (in `end()`) only when it
        // stuck; an equality check tells us the pinned view was corrected this
        // frame, so next frame it will paint at the bottom.
        let stuck = stick_to_bottom
            && reported_scroll > 0.0
            && (out.state.offset.y - reported_scroll).abs() <= 0.5;
        // Remember for next frame: a pinned layout culls the rows the stale offset
        // pushes below the viewport unless `layout_rows` widens the clip. The
        // stored offset lets the next frame confirm it is still pinned (the user
        // has not scrolled away) before widening.
        self.pinned_offset = stuck.then_some(out.state.offset.y);
        let paint_offset = self.paint_offset;
        let paint = self.paint.take();
        // The offset egui will actually paint next frame is the *true* bottom.
        let true_height = match self.layout_bottom {
            Some(bottom) => content_height.min(bottom),
            None => content_height,
        };
        let max_scroll = (true_height - out.inner_rect.height()).max(0.0);
        let shift_y = paint_offset - max_scroll;
        if let Some((layer, start, end)) = paint {
            let translate = stuck && shift_y.abs() > 0.25;
            let shift = egui::emath::TSTransform::from_translation(egui::vec2(0.0, shift_y));
            // `layout_rows` widened the clip's vertical edges by `OVERSCAN` so egui
            // would not cull the rows the stale offset pushed past the viewport.
            // Now that they are either translated back into place (still pinned) or
            // must stay hidden (the user scrolled away this frame), restore the
            // viewport's edges so nothing paints above or below it. Only the vertical
            // edges were widened, so clamp just those (an `intersect` would also
            // shrink the sides, which the vertical scroll area intentionally leaves
            // unbounded). The lower edge covers growth; the upper edge covers a
            // shrink, which pulls the rows above the stale viewport down into view.
            let viewport_top = out.inner_rect.min.y;
            let viewport_bottom = out.inner_rect.max.y;
            ui.ctx().graphics_mut(|graphics| {
                if let Some(list) = graphics.get_mut(layer) {
                    for i in start..end {
                        list.mutate_shape(egui::layers::ShapeIdx(i), |cs| {
                            if translate {
                                cs.shape.transform(shift);
                            }
                            cs.clip_rect.min.y = cs.clip_rect.min.y.max(viewport_top);
                            cs.clip_rect.max.y = cs.clip_rect.max.y.min(viewport_bottom);
                        });
                    }
                }
            });
        }
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
        thinking: &[Option<String>],
        live_reasoning: Option<&str>,
        colors: &ThemeColors,
        status: Option<&str>,
    ) {
        let n = rows.len();
        // Reset each frame; only the last-row branch below sets it (to the exact
        // height the rows were placed at), so a stale value can never leak into a
        // frame whose loop stopped early.
        self.layout_bottom = None;
        // Record the layer and the first shape index this layout will paint into,
        // so `show_with` can nudge exactly those shapes (see `cancel_bottom_growth`).
        let layer = ui.layer_id();
        let paint_start = ui
            .ctx()
            .graphics(|graphics| graphics.get(layer).map_or(0, |l| l.next_idx().0));
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

        // A pinned transcript is laid out with the previous frame's (stale) offset
        // and reaches the new bottom only via `cancel_bottom_growth`, which runs
        // after the rows are placed. That translation cannot recover a row egui
        // declined to paint because its stale rect missed the viewport (`Label`
        // culls via `is_rect_visible`), so widen the clip to the over-scan band:
        // the translation lands every painted row inside the real viewport. Both
        // edges must widen. Growth shifts rows up (the new bottom rows come from
        // below), while a shrink (e.g. the live-reasoning badge collapsing at the
        // end of a turn) shifts rows down and pulls the rows above the stale
        // viewport into view. Only do so while the offset being laid out with still
        // matches the recorded pinned offset (a drag-scroll moves it before
        // layout). Wheel scroll is applied after layout, so this gate cannot see
        // it; `cancel_bottom_growth` re-clips the band to the viewport to guarantee
        // nothing leaks either way.
        let pinned = self
            .pinned_offset
            .is_some_and(|offset| (viewport.min.y - offset).abs() < 0.5);
        if stick_to_bottom && pinned {
            let mut clip = ui.clip_rect();
            clip.min.y -= OVERSCAN;
            clip.max.y += OVERSCAN;
            ui.set_clip_rect(clip);
        }

        let spacing = TRANSCRIPT_ROW_GAP.max(ui.spacing().item_spacing.y);
        // Full transcript height from cached measurements (unmeasured rows
        // count 0 here and grow the region as they are built this frame), plus
        // symmetric vertical padding so the first and last rows never touch the
        // pane edges. The live-reasoning badge and the status line each add one
        // row gap plus their (last-frame) height after the final row.
        let live_gap = if live_reasoning.is_some() && n > 0 {
            spacing
        } else {
            0.0
        };
        let live_height = if live_reasoning.is_some() {
            self.live_height
        } else {
            0.0
        };
        let status_gap = if status.is_some() && (n > 0 || live_reasoning.is_some()) {
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
            + live_gap
            + live_height
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
            let row_thinking = if row.0 == "assistant" || is_tool {
                thinking.get(i).and_then(Option::as_deref)
            } else {
                None
            };
            let attached = row_thinking.map(|text| RowThinking {
                text,
                expanded: &mut self.thinking_expanded[i],
            });
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
                                attached,
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

        // Trailing live-reasoning badge: the running turn's in-progress thinking,
        // drawn as the last element of the chat so it scrolls with the
        // conversation. `content_y` already carries the inter-row gap after the
        // last row, so the badge starts there; the status line then follows the
        // badge's (last-frame) measured height.
        let mut trailing_y = content_y;
        if let Some(text) = live_reasoning
            && reached_end
        {
            let top = ui.max_rect().top() + trailing_y as f32;
            if top <= band_max as f32 {
                let left = ui.max_rect().center().x - width * 0.5;
                // Known by construction, so the badge never reflows the rows
                // pinned above it (not even on the frame it first appears).
                let bound = live_badge_height(ui, self.live_expanded);
                let rect = Rect::from_min_size(egui::pos2(left, top), egui::vec2(width, bound));
                let out = ui.scope_builder(
                    UiBuilder::new()
                        .id_salt(("live-thinking", tab_id))
                        .max_rect(rect),
                    |ui| {
                        egui::Frame::default()
                            .inner_margin(egui::Margin::symmetric(TRANSCRIPT_HORIZONTAL_PADDING, 0))
                            .show(ui, |ui| {
                                render_live_badge(ui, text, &mut self.live_expanded, colors)
                            });
                    },
                );
                self.live_height = out.response.rect.height();
                trailing_y += self.live_height as f64 + spacing as f64;
            }
        }

        // Trailing status line: rendered as the last element of the chat, so a
        // running turn's progress scrolls with the conversation rather than
        // living in the toolbar. Only laid out when the loop reached the end
        // (so `content_y` is the true tail) and the line meets the band.
        if let Some(text) = status
            && reached_end
        {
            let top = ui.max_rect().top() + trailing_y as f32;
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
        // bottom flush against the final row. The padding sits below the live
        // badge and status line when shown.
        if count > 0 && last_built_idx + 1 == n {
            let rows_bottom = content_y - spacing as f64;
            let mut content_bottom = rows_bottom;
            if live_reasoning.is_some() && reached_end {
                content_bottom += live_gap as f64 + self.live_height as f64;
            }
            if status.is_some() && reached_end {
                content_bottom += status_gap as f64 + self.status_height as f64;
            }
            let bottom = ui.max_rect().top() + content_bottom as f32;
            ui.expand_to_include_rect(Rect::from_min_size(
                egui::pos2(ui.max_rect().left(), bottom),
                egui::vec2(0.0, TRANSCRIPT_VERTICAL_PADDING),
            ));
            // Exact content height the rows were placed at, so a pinned view can
            // absorb a mid-layout shrink (see `layout_bottom`).
            self.layout_bottom = Some(content_bottom as f32 + TRANSCRIPT_VERTICAL_PADDING);
        }
        self.last_built = count;
        let paint_end = ui
            .ctx()
            .graphics(|graphics| graphics.get(layer).map_or(paint_start, |l| l.next_idx().0));
        self.paint = Some((layer, paint_start, paint_end));
        self.paint_offset = viewport.min.y;
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

/// Reasoning attached to the row it produced: the text plus its own disclosure
/// flag. Passed to [`render_row_contents`] so a settled answer or tool call
/// carries a faint "✻" that expands into a small, dim, flattened body rather
/// than occupying a peer transcript row.
pub(crate) struct RowThinking<'a> {
    /// The reasoning stream that led to this row.
    pub text: &'a str,
    /// Whether the attached body is expanded; flipped by the "✻" affordance.
    pub expanded: &'a mut bool,
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
    mut thinking: Option<RowThinking<'_>>,
    row: &mut RowUi,
    colors: &ThemeColors,
) {
    ui.style_mut()
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
    if role.starts_with("tool:") {
        render_tool_row(ui, role, card, text, thinking, row, colors);
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
        render_reasoning_row(ui, row_index, blocks, row, colors);
    } else {
        if let Some(t) = &mut thinking {
            // The heading and its attached "✻" sit on one line: a settled answer
            // keeps its reasoning one quiet click away, never as a peer row.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                role_heading(ui, role, colors);
                if thinking_affordance(ui, *t.expanded, colors) {
                    *t.expanded = !*t.expanded;
                    *row.resized = true;
                }
            });
        } else {
            role_heading(ui, role, colors);
        }
        if role == "system" && text.starts_with('\n') {
            markdown::render_diff_preview(ui, text, colors);
        } else {
            markdown::render_blocks(ui, row_index, blocks, colors);
        }
        if let Some(t) = &thinking
            && *t.expanded
        {
            render_thinking_body(ui, t.text, colors);
        }
    }
}

/// Live activity line drawn after the last transcript row. The activity pane
/// owns animation; the transcript uses a quiet dot to avoid duplicate spinners.
fn render_status_row(ui: &mut Ui, text: &str, colors: &ThemeColors) {
    ui.horizontal(|ui| {
        ui.colored_label(colors.tool_call, "●");
        ui.add(
            egui::Label::new(egui::RichText::new(text).small().weak())
                .truncate()
                .selectable(false),
        );
    });
}

/// A single, quiet thinking affordance. Collapsed (the default) is only a dim
/// "Thinking" caption and a chevron — no body — so a long reasoning stream never
/// floods the transcript. Clicking either the chevron or the caption expands the
/// reasoning into a small, dim body with headings flattened, keeping it clearly
/// secondary to the answer. The toggle re-measures the row via [`RowUi::resized`].
fn render_reasoning_row(
    ui: &mut Ui,
    row_index: usize,
    blocks: &[markdown::Block],
    row: &mut RowUi,
    colors: &ThemeColors,
) {
    let clicked = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let toggle = crate::icons::button(
                ui,
                if *row.expanded {
                    crate::icons::Icon::ChevronDown
                } else {
                    crate::icons::Icon::ChevronRight
                },
                if *row.expanded {
                    "Hide thinking"
                } else {
                    "Show thinking"
                },
            );
            let caption = ui.add(
                egui::Label::new(
                    egui::RichText::new("Thinking")
                        .small()
                        .weak()
                        .italics()
                        .color(colors.thinking),
                )
                .selectable(false)
                .sense(egui::Sense::click()),
            );
            toggle.clicked() || caption.clicked()
        })
        .inner;
    if clicked {
        *row.expanded = !*row.expanded;
        *row.resized = true;
    }
    if *row.expanded {
        ui.scope(|ui| {
            ui.visuals_mut().override_text_color = Some(colors.thinking);
            ui.style_mut()
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
            let demoted = demote_reasoning(blocks);
            markdown::render_blocks(ui, row_index, &demoted, colors);
        });
    }
}

/// A six-point "✻" drawn as three crossing diameters, so the affordance never
/// depends on a font shipping the glyph. `color` is expected to be pre-dimmed.
fn paint_asterisk(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.4, color);
    for degrees in [90.0_f32, 150.0, 210.0] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let arm = egui::vec2(cos, sin) * radius;
        painter.line_segment([center - arm, center + arm], stroke);
    }
}

/// A faint, hover-brightened "✻" that toggles attached reasoning. It always
/// allocates one fixed footprint so the virtualizer's measured and drawn heights
/// agree across hover/expand states — only the color changes. Returns whether the
/// affordance was clicked this frame.
fn thinking_affordance(ui: &mut Ui, expanded: bool, colors: &ThemeColors) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    let lit = expanded || response.hovered();
    let color = if lit {
        colors.thinking
    } else {
        colors.thinking.gamma_multiply(0.4)
    };
    paint_asterisk(ui.painter(), rect.center(), rect.height() * 0.3, color);
    let clicked = response.clicked();
    if lit {
        response.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    clicked
}

/// The dim, flattened reasoning stream attached to a row. Headings are demoted
/// so a model's `### Step` markers do not dominate; a fresh auto-id salts its
/// widgets so they never clash with the row body's `row_index` salt.
fn render_thinking_body(ui: &mut Ui, text: &str, colors: &ThemeColors) {
    ui.scope(|ui| {
        ui.visuals_mut().override_text_color = Some(colors.thinking);
        ui.style_mut()
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        let salt = ui.next_auto_id();
        let blocks = markdown::parse_markdown(text);
        let demoted = demote_reasoning(&blocks);
        markdown::render_blocks(ui, salt, &demoted, colors);
    });
}

/// Every height the live badge occupies, known before it is drawn. The
/// virtualizer reserves this instead of feeding a measurement back one frame
/// later, so the transcript never reflows because of the badge.
fn live_badge_height(ui: &Ui, expanded: bool) -> f32 {
    let caption = ui
        .text_style_height(&egui::TextStyle::Small)
        .max(LIVE_MARKER_SIZE);
    if expanded {
        caption + ui.spacing().item_spacing.y + LIVE_PANEL_HEIGHT
    } else {
        caption
    }
}

/// Paint the live marker: the same six-point "✻" as a settled row, but turning
/// and breathing while the turn runs. Painted into a fixed footprint, so the
/// animation only ever repaints the same rectangle.
fn paint_live_marker(painter: &egui::Painter, center: egui::Pos2, time: f32, color: egui::Color32) {
    let turn = std::f32::consts::TAU * time / LIVE_TURN_SECONDS;
    let breath = std::f32::consts::TAU * time / LIVE_PULSE_SECONDS;
    let stroke = egui::Stroke::new(1.4, color.gamma_multiply(0.8 + 0.2 * breath.sin()));
    let radius = LIVE_MARKER_SIZE * 0.3;
    for degrees in [90.0_f32, 150.0, 210.0] {
        let (sin, cos) = (degrees.to_radians() + turn).sin_cos();
        let arm = egui::vec2(cos, sin) * radius;
        painter.line_segment([center - arm, center + arm], stroke);
    }
}

/// Paint the caption's trailing dots, each brightening in turn, so the badge
/// reads as mid-sentence rather than as a finished thought.
fn paint_live_dots(painter: &egui::Painter, origin: egui::Pos2, time: f32, color: egui::Color32) {
    const DOT_COUNT: usize = 3;
    const DOT_GAP: f32 = 4.0;
    for i in 0..DOT_COUNT {
        let phase = time / LIVE_PULSE_SECONDS - i as f32 * 0.18;
        let glow = 0.5 + 0.5 * (std::f32::consts::TAU * phase).sin();
        painter.circle_filled(
            egui::pos2(origin.x + i as f32 * DOT_GAP, origin.y),
            1.5,
            color.gamma_multiply(0.3 + 0.7 * glow),
        );
    }
}

/// The badge's caption row: a fixed-footprint strip holding the animated marker,
/// the dim italic "Thinking" label and the trailing dots. Nothing in it goes
/// through egui's layout, so its size — and every row's position beside it — is
/// identical on every frame no matter what the animation is doing. Returns
/// whether the strip was clicked.
fn live_badge_caption(ui: &mut Ui, colors: &ThemeColors) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), live_badge_height(ui, false)),
        egui::Sense::click(),
    );
    let time = ui.input(|i| i.time) as f32;
    let font = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Small)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(11.0));
    let painter = ui.painter();
    let center = rect.center().y;
    paint_live_marker(
        painter,
        egui::pos2(rect.left() + LIVE_MARKER_SIZE * 0.5, center),
        time,
        colors.thinking,
    );
    let galley = painter.layout_job(egui::text::LayoutJob::single_section(
        "Thinking".to_owned(),
        egui::TextFormat {
            font_id: font,
            color: colors.thinking,
            italics: true,
            ..Default::default()
        },
    ));
    let text_x = rect.left() + LIVE_MARKER_SIZE + LIVE_MARKER_GAP;
    painter.galley(
        egui::pos2(text_x, center - galley.size().y * 0.5),
        galley.clone(),
        colors.thinking,
    );
    paint_live_dots(
        painter,
        egui::pos2(text_x + galley.size().x + LIVE_MARKER_GAP, center),
        time,
        colors.thinking,
    );
    let clicked = response.clicked();
    if response.hovered() {
        response.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    clicked
}

/// The expanded live stream: a fixed-height, dim, flattened panel that keeps the
/// newest reasoning in view. The fixed height is what stops a growing stream
/// from shifting the transcript pinned above it.
fn live_badge_body(ui: &mut Ui, text: &str, colors: &ThemeColors) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), LIVE_PANEL_HEIGHT),
        egui::Sense::hover(),
    );
    let mut body = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
    egui::ScrollArea::vertical()
        .id_salt("body")
        .max_height(LIVE_PANEL_HEIGHT)
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(&mut body, |ui| render_thinking_body(ui, text, colors));
}

/// The running turn's in-progress reasoning, drawn as a trailing "✻ Thinking"
/// badge so it scrolls with the conversation instead of sitting in a peer row.
/// Clicking the badge expands a bounded, dim, flattened stream. Repaints while
/// live so the marker animates — every part of it is painted inside a fixed
/// footprint, so "live" never means "moving" for anything else on screen.
fn render_live_badge(ui: &mut Ui, text: &str, expanded: &mut bool, colors: &ThemeColors) {
    ui.ctx().request_repaint();
    if live_badge_caption(ui, colors) {
        *expanded = !*expanded;
    }
    if *expanded {
        live_badge_body(ui, text, colors);
    }
}

/// Flatten reasoning headings into body paragraphs so a model that emits
/// `### Step` markers does not dominate the chat; every other block is kept so
/// code, lists and emphasis still read as written.
fn demote_reasoning(blocks: &[markdown::Block]) -> Vec<markdown::Block> {
    blocks
        .iter()
        .map(|block| match block {
            markdown::Block::Heading { runs, .. } => markdown::Block::Paragraph {
                runs: runs.clone(),
                marker: None,
                depth: 0,
            },
            other => other.clone(),
        })
        .collect()
}

/// Quiet, selectable role captions; message content carries the visual emphasis.
fn role_heading(ui: &mut Ui, role: &str, colors: &ThemeColors) {
    let rich = match role {
        "user" => egui::RichText::new("You").small().color(colors.user_msg),
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
    mut thinking: Option<RowThinking<'_>>,
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
        // Attached reasoning rides the tool heading, independent of the output
        // disclosure: the call keeps the thinking that produced it one click away.
        if let Some(t) = &mut thinking
            && thinking_affordance(ui, *t.expanded, colors)
        {
            *t.expanded = !*t.expanded;
            *row.resized = true;
        }
    });

    // Drawn before the collapsed-summary early return below, so attached
    // reasoning is reachable even while the tool output stays collapsed.
    if let Some(t) = &thinking
        && *t.expanded
    {
        render_thinking_body(ui, t.text, colors);
    }

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
                        None,
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
                                                None,
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

    /// Reference render with the same default expansion state as the virtualizer:
    /// tool rows follow their card lifecycle, everything else starts collapsed.
    fn reference_content_height(rows: &[(String, String)], cards: &[Option<ToolCard>]) -> f32 {
        let expanded = cards
            .iter()
            .map(|card| card.as_ref().map(default_tool_expanded).unwrap_or(false))
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
        // Expansion state grows with the rows: collapsed by default, one chunk.
        assert_eq!(c.expanded.len(), 12);
        assert!(c.expanded.iter().all(|&e| !e));
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
            render_tool_row(
                ui,
                "tool: shell",
                Some(&card),
                &text,
                None,
                &mut row,
                &colors,
            );
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
            render_tool_row(
                ui,
                "tool: shell",
                Some(&hidden),
                &text,
                None,
                &mut row,
                &colors,
            );
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
                            None,
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
                None,
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
                    None,
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
                None,
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
            let (mut expanded, mut chunks, mut resized) = (false, 0usize, false);
            render_reasoning_row(
                ui,
                0,
                &[],
                &mut RowUi {
                    expanded: &mut expanded,
                    chunks: &mut chunks,
                    resized: &mut resized,
                },
                &colors,
            );
        });
        assert!(
            seen.contains(&user),
            "user heading must render in user_msg color, saw {seen:?}"
        );
        assert!(
            seen.contains(&thinking),
            "thinking caption must render in thinking color, saw {seen:?}"
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
                    None,
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

    /// A reasoning row is a single quiet disclosure: collapsed (the default)
    /// paints only the "Thinking" caption and hides the body; expanding reveals
    /// the full reasoning text.
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
                None,
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
            collapsed.contains("Thinking"),
            "collapsed shows the thinking caption, saw {collapsed:?}"
        );
        assert!(
            !collapsed.contains("first reasoning line"),
            "collapsed hides the reasoning body, saw {collapsed:?}"
        );
        assert!(
            !collapsed.contains("tail reasoning line"),
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
                None,
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

    /// The virtualizer opens new reasoning rows collapsed, so the *painted*
    /// transcript never contains the reasoning body until the user expands it.
    #[test]
    fn new_reasoning_rows_start_collapsed() {
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        let rows = vec![(
            "reasoning".to_string(),
            "secret reasoning body\nsecond line".to_string(),
        )];
        let cards = vec![None];
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                cache.show(ui, TAB, true, &rows, &cards, &ThemeColors::default());
            });
        }
        assert!(!cache.expanded[0], "reasoning must start collapsed");
        let texts = frame_texts(&ctx, input(FRAMES + 1, W), |ui| {
            cache.show(ui, TAB, true, &rows, &cards, &ThemeColors::default());
        });
        assert!(
            texts.iter().any(|t| t == "Thinking"),
            "the caption must be painted, saw {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("secret reasoning body")),
            "the collapsed row must not paint the reasoning body, saw {texts:?}"
        );
    }

    /// Reasoning attached to a row stays hidden until the row's "✻" affordance is
    /// toggled; when expanded it paints the flattened, dim stream beneath the row
    /// body. Driven through `render_row_contents` so the disclosure wiring — not
    /// the virtualizer's height cache — is under test.
    #[test]
    fn attached_thinking_is_hidden_until_expanded() {
        let colors = ThemeColors::default();
        let body = "attached reasoning stream\nsecond line";
        let blocks = markdown::parse_markdown("the answer body");

        let render = |expand: bool| {
            frame_texts(&egui::Context::default(), input(0, W), |ui| {
                let mut expanded = expand;
                let mut row_expanded = expand;
                let mut chunks = 1usize;
                let mut resized = false;
                render_row_contents(
                    ui,
                    0,
                    "assistant",
                    None,
                    "the answer body",
                    &blocks,
                    Some(RowThinking {
                        text: body,
                        expanded: &mut row_expanded,
                    }),
                    &mut RowUi {
                        expanded: &mut expanded,
                        chunks: &mut chunks,
                        resized: &mut resized,
                    },
                    &colors,
                );
            })
            .join("\n")
        };

        let collapsed = render(false);
        assert!(
            collapsed.contains("the answer body"),
            "the row body still paints, saw {collapsed:?}"
        );
        assert!(
            !collapsed.contains("attached reasoning stream"),
            "collapsed attached thinking must stay hidden, saw {collapsed:?}"
        );

        let expanded = render(true);
        assert!(
            expanded.contains("attached reasoning stream"),
            "expanded attached thinking must paint its body, saw {expanded:?}"
        );
    }

    /// The running turn's reasoning draws as a trailing "Thinking…" badge. It
    /// shows only the caption while collapsed and reveals the stream when the
    /// badge is expanded.
    #[test]
    fn live_thinking_badge_hides_body_until_expanded() {
        let colors = ThemeColors::default();
        let live = "streaming reasoning body";

        let collapsed = frame_texts(&egui::Context::default(), input(0, W), |ui| {
            let mut expanded = false;
            render_live_badge(ui, live, &mut expanded, &colors);
        })
        .join("\n");
        assert!(
            collapsed.contains("Thinking"),
            "the badge caption must paint, saw {collapsed:?}"
        );
        assert!(
            !collapsed.contains("streaming reasoning body"),
            "the collapsed badge must not paint the stream, saw {collapsed:?}"
        );

        let expanded = frame_texts(&egui::Context::default(), input(0, W), |ui| {
            let mut expanded = true;
            render_live_badge(ui, live, &mut expanded, &colors);
        })
        .join("\n");
        assert!(
            expanded.contains("streaming reasoning body"),
            "the expanded badge must paint the stream, saw {expanded:?}"
        );
    }

    /// End-to-end through `show_with`: `thinking` stays index-aligned with `rows`,
    /// so an expanded attached body paints under its own row (assistant or tool),
    /// while the trailing live badge draws collapsed by default.
    #[test]
    fn show_with_attaches_thinking_per_row_and_a_live_badge() {
        let colors = ThemeColors::default();
        let mut cache = Cache::new();
        let rows = vec![
            ("assistant".to_string(), "answer one".to_string()),
            ("user".to_string(), "a question".to_string()),
            ("tool: shell".to_string(), "log line".to_string()),
        ];
        let cards: Vec<Option<ToolCard>> = vec![None, None, None];
        let thinking = vec![
            Some("thought for the answer".to_string()),
            None,
            Some("thought for the tool call".to_string()),
        ];
        // Expand both attached bodies before the first frame so the reflow
        // measures them in place.
        cache.thinking_expanded = vec![true, false, true];

        let texts = frame_texts(&egui::Context::default(), input(0, W), |ui| {
            cache.show_with(
                ui,
                TAB,
                true,
                &rows,
                &cards,
                &thinking,
                Some("live running thought"),
                &colors,
            );
        })
        .join("\n");

        assert!(
            texts.contains("thought for the answer"),
            "the assistant row's attached thinking expands in place, saw {texts:?}"
        );
        assert!(
            texts.contains("thought for the tool call"),
            "the tool row's attached thinking expands even while its output stays \
             collapsed, saw {texts:?}"
        );
        assert!(
            texts.contains("Thinking"),
            "the trailing live badge caption is painted, saw {texts:?}"
        );
        assert!(
            !texts.contains("live running thought"),
            "the live badge starts collapsed and hides its stream, saw {texts:?}"
        );
    }

    /// Demoting reasoning flattens headings to body paragraphs (so `### Step`
    /// markers cannot dominate the chat) while leaving every other block intact.
    #[test]
    fn demote_reasoning_flattens_headings_only() {
        let blocks = markdown::parse_markdown("### Step\n\nbody text\n\n```\ncode\n```");
        let demoted = demote_reasoning(&blocks);
        assert!(
            !demoted
                .iter()
                .any(|b| matches!(b, markdown::Block::Heading { .. })),
            "headings must be flattened"
        );
        assert!(
            demoted
                .iter()
                .any(|b| matches!(b, markdown::Block::Code { .. })),
            "code blocks must survive"
        );
        assert_eq!(demoted.len(), blocks.len());
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
                None,
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
            render_row_contents(
                ui, 0, "system", None, content, &blocks, None, &mut row, &colors,
            );
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

    /// Anti-jitter regression: while the turn's reasoning streams, the live badge
    /// keeps a fixed footprint, so the caption, the status line and the last
    /// transcript row never move — in either expanded state — and the badge's
    /// reserved height is a constant function of `expanded` alone. Before the fix
    /// the badge grew with the stream (up to `auto_shrink` unbounded) and pushed
    /// every row pinned above it; this pins that shut.
    #[test]
    fn live_badge_never_moves_the_transcript_while_streaming() {
        let colors = ThemeColors::default();
        // Reserved badge height per `expanded` state, filled as each pass settles.
        let mut settled_height = [0.0_f32; 2];
        let mut item_gap = 0.0_f32;
        for (slot, expanded) in [false, true].into_iter().enumerate() {
            let mut cache = Cache::new();
            cache.live_expanded = expanded;
            let rows: Vec<(String, String)> = (0..40)
                .map(|i| {
                    (
                        "assistant".to_string(),
                        format!("paragraph number {i} with some words"),
                    )
                })
                .collect();
            let cards: Vec<Option<ToolCard>> = rows.iter().map(|_| None).collect();
            let ctx = egui::Context::default();
            // Reference geometry from the first frame that paints the badge; every
            // later frame must reproduce it exactly even as the stream grows.
            let mut reference: Option<(f32, f32, f32, f32)> = None;
            for frame in 0..10 {
                // The stream lengthens and the status text churns every frame.
                let live = format!("thought {} extra words to wrap", "x".repeat(frame * 2));
                let status = format!("working on it {frame}");
                let mut out = ctx.run_ui(input(frame, W), |ui| {
                    item_gap = ui.spacing().item_spacing.y;
                    cache.set_status(Some(status.clone()));
                    cache.show_with(
                        ui,
                        TAB,
                        true,
                        &rows,
                        &cards,
                        &[],
                        Some(live.as_str()),
                        &colors,
                    );
                });
                let texts = painted_text(&out);
                out.textures_delta.clear();
                let caption_y = texts
                    .iter()
                    .find(|t| t.galley.text() == "Thinking")
                    .map(|t| t.pos.y);
                let status_y = texts
                    .iter()
                    .find(|t| t.galley.text().contains("working on it"))
                    .map(|t| t.pos.y);
                let last_para_y = texts
                    .iter()
                    .filter(|t| t.galley.text().contains("paragraph number"))
                    .map(|t| t.pos.y)
                    .fold(f32::NEG_INFINITY, f32::max);
                let (Some(caption_y), Some(status_y)) = (caption_y, status_y) else {
                    // Warm-up frames (before the height cache is primed) do not
                    // paint the badge; skip until it is on screen.
                    continue;
                };
                let current = (caption_y, status_y, last_para_y, cache.live_height);
                match reference {
                    None => reference = Some(current),
                    Some(reference) => assert_eq!(
                        reference, current,
                        "badge geometry drifted on frame {frame} (expanded={expanded})"
                    ),
                }
                settled_height[slot] = cache.live_height;
            }
            assert!(
                reference.is_some(),
                "the live badge never painted (expanded={expanded})"
            );
        }
        // Collapsed reserves only the caption; expanded adds one row gap plus the
        // fixed panel, so a growing stream can never push the transcript.
        assert!(
            (settled_height[1] - settled_height[0] - (item_gap + LIVE_PANEL_HEIGHT)).abs() < 0.01,
            "expanded badge must reserve exactly one gap plus LIVE_PANEL_HEIGHT, \
             saw collapsed={} expanded={}",
            settled_height[0],
            settled_height[1]
        );
        assert!(
            settled_height[0] >= LIVE_MARKER_SIZE,
            "collapsed badge must reserve at least the marker footprint, saw {}",
            settled_height[0]
        );
    }

    /// Rows for the streaming jitter tests:
    /// `[3 filler] [streaming tool] [MIDMARKER] [20 filler] [BELOW_MARKER]`.
    /// The tool row's attached thinking grows every frame, pushing every row
    /// after it down; there is enough content to overflow the viewport, so the
    /// view can pin to the bottom. `tool_index` is the growing row.
    fn streaming_bottom_fixture() -> (Vec<(String, String)>, Vec<Option<ToolCard>>, usize) {
        let mut rows: Vec<(String, String)> = (0..3)
            .map(|i| {
                (
                    "assistant".to_string(),
                    format!("lead filler row {i} with enough words to take one line"),
                )
            })
            .collect();
        let mut cards: Vec<Option<ToolCard>> = rows.iter().map(|_| None).collect();
        let tool_index = rows.len();
        rows.push(("tool: shell".to_string(), "SHELL_LOG".to_string()));
        cards.push(Some(ToolCard {
            name: "shell".into(),
            state: ToolState::Running,
            args: None,
            label: Some("shell cargo test".into()),
            show_result: Some(true),
            eager: Some(true),
        }));
        rows.push((
            "assistant".to_string(),
            "MIDMARKER pushed by the stream".to_string(),
        ));
        cards.push(None);
        for i in 0..20 {
            rows.push((
                "assistant".to_string(),
                format!("trail filler row {i} with enough words to take one line"),
            ));
            cards.push(None);
        }
        rows.push((
            "assistant".to_string(),
            "BELOW_MARKER settled text at the tail".to_string(),
        ));
        cards.push(None);
        (rows, cards, tool_index)
    }

    /// One frame of streaming reasoning: a fresh paragraph every frame, so the
    /// attached thinking grows by at least one line each call.
    fn streaming_thinking(frame: usize) -> String {
        let mut body = String::new();
        for i in 0..=frame {
            body.push_str(&format!("thought paragraph {i} of the stream\n\n"));
        }
        body.push_str("```rust\nfn main() {}\n```\n");
        body
    }

    /// Y position of the painted text containing `needle` this frame, if any.
    fn marker_y(output: &egui::FullOutput, needle: &str) -> Option<f32> {
        painted_text(output)
            .iter()
            .find(|t| t.galley.text().contains(needle))
            .map(|t| t.pos.y)
    }

    /// Anti-jitter regression: while a bottom-pinned transcript streams, a row
    /// above the viewport bottom grows, but egui applies the sticky bottom offset
    /// one frame late. Without a correction the trailing marker is painted one
    /// growth step too low and snaps back on the next frame (visible jitter); the
    /// transcript cancels exactly that shift, so the marker never moves.
    #[test]
    fn bottom_pinned_growth_does_not_jitter_the_transcript() {
        let colors = ThemeColors::default();
        let (rows, cards, tool) = streaming_bottom_fixture();
        let mut cache = Cache::new();
        cache.thinking_expanded = vec![false; rows.len()];
        cache.thinking_expanded[tool] = true;
        cache.expanded = vec![false; rows.len()];
        let ctx = egui::Context::default();
        let mut below_ys: Vec<f32> = Vec::new();
        for frame in 0..30usize {
            let mut thinking = vec![None; rows.len()];
            thinking[tool] = Some(streaming_thinking(frame));
            let mut out = ctx.run_ui(input(frame, W), |ui| {
                cache.show_with(ui, TAB, true, &rows, &cards, &thinking, None, &colors);
            });
            if let Some(y) = marker_y(&out, "BELOW_MARKER") {
                below_ys.push(y);
            }
            out.textures_delta.clear();
        }
        assert!(below_ys.len() >= 20, "marker rarely painted: {below_ys:?}");
        let first = below_ys[0];
        assert!(
            below_ys.iter().all(|y| (y - first).abs() < 0.01),
            "bottom-pinned marker jittered while streaming: {below_ys:?}"
        );
    }

    /// Guard: the growth correction only fires while the view is pinned to the
    /// bottom. A transcript scrolled to the top keeps its normal monotone growth —
    /// the marker slides down as the row above it grows — so it must not be pinned.
    #[test]
    fn scrolled_transcript_growth_stays_monotone() {
        let colors = ThemeColors::default();
        let (rows, cards, tool) = streaming_bottom_fixture();
        let mut cache = Cache::new();
        cache.thinking_expanded = vec![false; rows.len()];
        cache.thinking_expanded[tool] = true;
        cache.expanded = vec![false; rows.len()];
        let ctx = egui::Context::default();
        let mut marker_ys: Vec<f32> = Vec::new();
        for frame in 0..30usize {
            let mut thinking = vec![None; rows.len()];
            thinking[tool] = Some(streaming_thinking(frame));
            let mut out = ctx.run_ui(input(frame, W), |ui| {
                cache.show_with(ui, TAB, false, &rows, &cards, &thinking, None, &colors);
            });
            if let Some(y) = marker_y(&out, "MIDMARKER") {
                marker_ys.push(y);
            }
            out.textures_delta.clear();
        }
        assert!(
            marker_ys.len() >= 20,
            "marker rarely painted: {marker_ys:?}"
        );
        // Past warm-up the marker only ever slides down; a small tolerance absorbs
        // sub-pixel reflow.
        let tail = &marker_ys[3..];
        assert!(
            tail.windows(2).all(|w| w[1] >= w[0] - 0.5),
            "scrolled transcript moved upward: {marker_ys:?}"
        );
        assert!(
            tail.last().unwrap() - tail[0] > 10.0,
            "scrolled transcript was pinned (marker never slid): {marker_ys:?}"
        );
    }

    /// Anti-jitter regression, wobbling-viewport variant. The real app's bottom
    /// panel (live-activity pane + composer) reflows while a turn streams, so the
    /// transcript's viewport height `V` changes frame to frame. The fix anchors
    /// the pinned view by the exact offset egui painted with (`paint_offset`
    /// minus `max_scroll`), so the trailing marker keeps a *constant distance from
    /// the viewport bottom* however `V` moves. The previous delta-of-content-height
    /// mechanism cancelled only a constant-`V` residual, leaving a `ΔV` error that
    /// this invariant would expose.
    #[test]
    fn bottom_pinned_growth_with_a_wobbling_viewport_stays_put() {
        let colors = ThemeColors::default();
        let (rows, cards, tool) = streaming_bottom_fixture();
        let mut cache = Cache::new();
        cache.thinking_expanded = vec![false; rows.len()];
        cache.thinking_expanded[tool] = true;
        cache.expanded = vec![false; rows.len()];
        let ctx = egui::Context::default();
        let mut distances: Vec<f32> = Vec::new();
        for frame in 0..30usize {
            let mut thinking = vec![None; rows.len()];
            thinking[tool] = Some(streaming_thinking(frame));
            // Alternate the transcript viewport height as a live bottom panel
            // reflowing under it would.
            let viewport_h = H - 120.0 - (frame % 2) as f32 * 40.0;
            let mut inner_bottom = None;
            let mut out = ctx.run_ui(input(frame, W), |ui| {
                ui.scope_builder(
                    UiBuilder::new().max_rect(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(W, viewport_h),
                    )),
                    |ui| {
                        let shown =
                            cache.show_with(ui, TAB, true, &rows, &cards, &thinking, None, &colors);
                        inner_bottom = Some(shown.inner_rect.bottom());
                    },
                );
            });
            if let (Some(bottom), Some(y)) = (inner_bottom, marker_y(&out, "BELOW_MARKER")) {
                distances.push(bottom - y);
            }
            out.textures_delta.clear();
        }
        assert!(
            distances.len() >= 25,
            "marker rarely painted: {distances:?}"
        );
        let first = distances[0];
        assert!(
            distances.iter().all(|d| (d - first).abs() < 0.5),
            "bottom-pinned marker drifted from the viewport bottom while the \
             viewport wobbled: {distances:?}"
        );
    }

    /// The clip widening that lets a pinned frame paint its below-viewport rows
    /// (so `cancel_bottom_growth` can translate them into place) must never leak
    /// those rows once the user scrolls away from the pinned bottom. egui applies
    /// wheel scroll in its scroll area's `end()`, *after* the layout closure, so
    /// the very first wheel-up frame still lays out with the stale pinned offset
    /// and would paint the widened band unshifted. This drives a real wheel event
    /// and asserts nothing paints below the viewport.
    #[test]
    fn scrolling_up_from_a_pinned_bottom_never_paints_below_the_viewport() {
        let colors = ThemeColors::default();
        let (rows, cards, tool) = streaming_bottom_fixture();
        let mut cache = Cache::new();
        cache.thinking_expanded = vec![false; rows.len()];
        cache.thinking_expanded[tool] = true;
        cache.expanded = vec![false; rows.len()];
        let ctx = egui::Context::default();
        let viewport_h = H - 120.0;
        let hover = egui::Pos2::new(W * 0.5, viewport_h * 0.5);
        let show = |ui: &mut Ui, cache: &mut Cache, thinking: &[Option<String>]| {
            ui.scope_builder(
                UiBuilder::new().max_rect(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(W, viewport_h),
                )),
                |ui| cache.show_with(ui, TAB, true, &rows, &cards, thinking, None, &colors),
            )
            .inner
            .inner_rect
            .bottom()
        };

        // Settle several pinned, streaming frames so rows really sit below the
        // viewport and the widened band is active.
        for frame in 0..8usize {
            let mut thinking = vec![None; rows.len()];
            thinking[tool] = Some(streaming_thinking(frame));
            let mut out = ctx.run_ui(input(frame, W), |ui| {
                show(ui, &mut cache, &thinking);
            });
            out.textures_delta.clear();
        }

        // Wheel-scroll up. egui applies the scroll after the layout closure, so
        // this frame still lays out at the stale pinned offset.
        let mut thinking = vec![None; rows.len()];
        thinking[tool] = Some(streaming_thinking(8));
        let mut raw = input(8, W);
        raw.events.push(egui::Event::PointerMoved(hover));
        raw.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 0.0),
            phase: egui::TouchPhase::Start,
            modifiers: Default::default(),
        });
        raw.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 320.0),
            phase: egui::TouchPhase::Move,
            modifiers: Default::default(),
        });
        let mut inner_bottom = 0.0;
        let mut out = ctx.run_ui(raw, |ui| {
            inner_bottom = show(ui, &mut cache, &thinking);
        });
        out.textures_delta.clear();
        // Nothing visible may paint below the viewport: the widened band must be
        // clipped back once the user has scrolled away (no translation is applied).
        // Clipping shows up in each shape's `clip_rect`, not its own bounds, so
        // measure the clipped bottom.
        fn clipped_text_bottoms(shape: &egui::Shape, clip: Rect, out: &mut Vec<f32>) {
            match shape {
                egui::Shape::Text(text) => {
                    out.push(text.visual_bounding_rect().bottom().min(clip.bottom()));
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        clipped_text_bottoms(shape, clip, out);
                    }
                }
                _ => {}
            }
        }
        let mut bottoms: Vec<f32> = Vec::new();
        for clipped in &out.shapes {
            clipped_text_bottoms(&clipped.shape, clipped.clip_rect, &mut bottoms);
        }
        let leaked: Vec<f32> = bottoms
            .into_iter()
            .filter(|b| *b > inner_bottom + 0.5)
            .collect();
        assert!(
            leaked.is_empty(),
            "rows leaked below the viewport after scrolling up: {leaked:?}"
        );
    }

    /// Anti-jitter regression, *closing* variant. A turn ends by clearing the
    /// trailing live-reasoning badge (`state::settle_reasoning`), so the transcript
    /// content *shrinks* by the badge's height. That is a positive `shift_y`: the
    /// rows that must move down into the viewport come from *above* the stale
    /// viewport (the previous frame's pinned offset). Before the fix only the
    /// clip's lower edge was widened, so those rows stayed culled and a blank strip
    /// flashed at the top of the viewport for one frame. This streams a tall live
    /// badge while pinned, drops it, and asserts the visible content stays flush to
    /// the viewport top.
    #[test]
    fn bottom_pinned_shrink_on_turn_end_keeps_content_flush() {
        let colors = ThemeColors::default();
        let (rows, cards, _tool) = streaming_bottom_fixture();
        let mut cache = Cache::new();
        cache.thinking_expanded = vec![false; rows.len()];
        cache.expanded = vec![false; rows.len()];
        // A tall (expanded) live badge makes the turn-end shrink large and obvious.
        cache.live_expanded = true;
        let ctx = egui::Context::default();
        let viewport_h = H - 120.0;

        // Topmost *visible* text edge in the frame, in screen coordinates.
        fn topmost_visible_text(output: &egui::FullOutput) -> Option<f32> {
            fn walk(shape: &egui::Shape, clip: Rect, out: &mut Option<f32>) {
                match shape {
                    egui::Shape::Text(text) => {
                        let vis = text.visual_bounding_rect().intersect(clip);
                        if vis.is_positive() {
                            let top = vis.top();
                            *out = Some(out.map_or(top, |m: f32| m.min(top)));
                        }
                    }
                    egui::Shape::Vec(shapes) => {
                        for shape in shapes {
                            walk(shape, clip, out);
                        }
                    }
                    _ => {}
                }
            }
            let mut out = None;
            for clipped in &output.shapes {
                walk(&clipped.shape, clipped.clip_rect, &mut out);
            }
            out
        }

        let mut top_gaps: Vec<f32> = Vec::new();
        for frame in 0..14usize {
            // A running turn: a tall trailing live badge. Final frame: the turn
            // settled, so the badge is gone and the content shrinks.
            let live = (frame < 12).then(|| streaming_thinking(frame.min(6)));
            let mut inner_top = 0.0;
            let mut out = ctx.run_ui(input(frame, W), |ui| {
                ui.scope_builder(
                    UiBuilder::new().max_rect(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(W, viewport_h),
                    )),
                    |ui| {
                        let shown = cache.show_with(
                            ui,
                            TAB,
                            true,
                            &rows,
                            &cards,
                            &[] as &[Option<String>],
                            live.as_deref(),
                            &colors,
                        );
                        inner_top = shown.inner_rect.top();
                    },
                );
            });
            if let Some(top) = topmost_visible_text(&out) {
                top_gaps.push(top - inner_top);
            }
            out.textures_delta.clear();
        }
        assert!(top_gaps.len() >= 12, "marker rarely painted: {top_gaps:?}");
        let worst = top_gaps.iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            worst < 64.0,
            "blank strip at the top of the pinned viewport on the turn-end shrink \
             (rows above the stale viewport were culled): {top_gaps:?}"
        );
    }

    #[test]
    fn expanding_a_past_rows_reasoning_does_not_jitter_the_transcript() {
        // Non-pinned "past message" path: settle pinned, wheel up to rest in the
        // middle of the transcript, then click the median visible row's reasoning
        // affordance. A correct expand leaves the viewport put: the offset never
        // moves on its own and no painted text lurches for a single frame before
        // snapping back.
        let colors = ThemeColors::default();
        let (rows, cards, tool) = streaming_bottom_fixture();
        let n = rows.len();
        let mut cache = Cache::new();
        cache.thinking_expanded = vec![false; n];
        cache.thinking_expanded[tool] = true;
        cache.expanded = vec![false; n];
        let mut thinking: Vec<Option<String>> = (0..n)
            .map(|_| Some("short reasoning\nsecond line".to_string()))
            .collect();
        thinking[tool] = Some(streaming_thinking(6));
        let ctx = egui::Context::default();
        let viewport_h = 1200.0;

        fn collect(shape: &egui::Shape, found: &mut Vec<(String, Rect)>) {
            match shape {
                egui::Shape::Text(t) => {
                    found.push((t.galley.text().to_string(), t.visual_bounding_rect()))
                }
                egui::Shape::Vec(v) => {
                    for s in v {
                        collect(s, found);
                    }
                }
                _ => {}
            }
        }

        let hover = egui::Pos2::new(W * 0.5, viewport_h * 0.5);
        let mut events: Vec<egui::Event> = Vec::new();
        let mut offsets: Vec<f32> = Vec::new();
        let mut tops: Vec<f32> = Vec::new();
        let mut saw_body = false;
        for frame in 0..26usize {
            let mut raw = input(frame, W);
            raw.events = std::mem::take(&mut events);
            // Wheel up out of the pinned bottom so the later frames lay out a
            // non-pinned, scrolled-up view.
            if frame == 5 {
                raw.events.push(egui::Event::PointerMoved(hover));
                raw.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, 0.0),
                    phase: egui::TouchPhase::Start,
                    modifiers: Default::default(),
                });
                raw.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, 700.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: Default::default(),
                });
            }
            let stick = frame < 5;
            let mut texts: Vec<(String, Rect)> = Vec::new();
            let mut inner_top = 0.0;
            let mut inner_bottom = 0.0;
            let mut offset = 0.0;
            let mut out = ctx.run_ui(raw, |ui| {
                ui.scope_builder(
                    UiBuilder::new().max_rect(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(W, viewport_h),
                    )),
                    |ui| {
                        let shown = cache
                            .show_with(ui, TAB, stick, &rows, &cards, &thinking, None, &colors);
                        inner_top = shown.inner_rect.top();
                        inner_bottom = shown.inner_rect.bottom();
                        offset = shown.state.offset.y;
                    },
                );
            });
            for c in &out.shapes {
                collect(&c.shape, &mut texts);
            }
            out.textures_delta.clear();
            // Record once the scrolled-up view has settled (frame 5's wheel is
            // applied at the end of that frame).
            if frame >= 8 {
                offsets.push(offset);
                saw_body |= texts.iter().any(|(s, _)| s.contains("short reasoning"));
                tops.push(
                    texts
                        .iter()
                        .map(|(_, r)| r.top())
                        .filter(|y| *y >= inner_top - 1.0 && *y <= inner_bottom)
                        .fold(f32::INFINITY, f32::min)
                        - inner_top,
                );
            }
            if frame == 12 || frame == 13 {
                // Click the median visible row's "✻" so rows sit both above and
                // below the row whose reasoning body expands.
                let mut bones: Vec<Rect> = texts
                    .iter()
                    .filter(|(s, r)| {
                        s == "Bone" && r.top() > inner_top + 2.0 && r.bottom() < inner_bottom - 2.0
                    })
                    .map(|(_, r)| *r)
                    .collect();
                bones.sort_by(|a, b| a.top().partial_cmp(&b.top()).unwrap());
                let bone = bones[bones.len() / 2];
                let pos = egui::pos2(bone.right() + 11.0, bone.center().y);
                events.push(egui::Event::PointerMoved(pos));
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: frame == 12,
                    modifiers: Default::default(),
                });
            }
        }
        assert!(
            saw_body,
            "test did not actually expand a reasoning body (vacuous): {tops:?}"
        );
        assert!(
            offsets.windows(2).all(|w| (w[0] - w[1]).abs() < 0.5),
            "expanding a past row scrolled the transcript on its own: {offsets:?}"
        );
        // A transient lurch is a frame that differs from *both* neighbours by
        // more than a few pixels: it appears and then snaps back. A genuine
        // one-time growth step differs from only one neighbour, so it is allowed.
        let lurched = tops
            .windows(3)
            .any(|w| (w[1] - w[0]).abs() > 8.0 && (w[1] - w[2]).abs() > 8.0);
        assert!(
            !lurched,
            "expanding a past row lurched the transcript: {tops:?}"
        );
    }
}
