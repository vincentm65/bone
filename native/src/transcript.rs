//! Variable-height transcript virtualization with a bounded parse cache.
//!
//! A conversation history grows without bound, so the Stage 2 approach of
//! re-parsing and laying out every row on every frame is O(rows) even when the
//! visible window is small. This module keeps a per-row measured height and
//! parses/lays out only rows near the visible band (`OVERSCAN`), using
//! [`egui::ScrollArea::show_viewport`]. Heights persist across frames so warm
//! frames re-lay-out just the band; width/zoom changes invalidate every
//! measurement (a reflow). Parsed rows are cached but evicted outside a slack
//! margin around the band, so memory stays bounded too.
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

/// Per-tab virtualization state, owned by [`crate::Tab`]. One per conversation,
/// kept alive across reconnects via [`Self::reset`].
pub(crate) struct Cache {
    /// Content width the cached measurements were taken at.
    width: f32,
    /// Zoom factor the cached measurements were taken at.
    zoom: f32,
    /// Per-row measured height in points; `0.0` means "not measured yet".
    heights: Vec<f32>,
    /// Per-row parsed markdown; `None` means "not parsed yet or evicted".
    parsed: Vec<Option<Vec<markdown::Block>>>,
    /// Number of rows in `heights` that are still `0.0` (unmeasured).
    unknown: usize,
    /// Rows actually laid out during the most recent frame (test/measurement
    /// instrumentation; not read outside `#[cfg(test)]` code).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) last_built: usize,
}

impl Cache {
    pub(crate) fn new() -> Self {
        Self {
            width: 0.0,
            zoom: 0.0,
            heights: Vec::new(),
            parsed: Vec::new(),
            unknown: 0,
            last_built: 0,
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
            self.heights.fill(0.0);
            self.parsed.fill(None);
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
            if self.parsed[i].is_none() {
                self.parsed[i] = Some(markdown::parse_markdown(&rows[i].1));
            }
            let blocks = self.parsed[i].as_deref().expect("parsed just above");
            let bound = if known { height } else { MEASURE_BOUND };
            let top = ui.max_rect().top() + content_y as f32;
            let rect = Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + bound);
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
                        )
                    })
                },
            );
            let measured = out.response.rect.height();
            if !known {
                self.unknown = self.unknown.saturating_sub(1);
            }
            self.heights[i] = measured;
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

/// Body of one transcript row group: a role/tool heading followed by the
/// rendered markdown blocks. `card` is the tool overlay state for tool rows.
pub(crate) fn render_row_contents(
    ui: &mut Ui,
    row_index: usize,
    role: &str,
    card: Option<&ToolCard>,
    text: &str,
    blocks: &[markdown::Block],
) {
    if role.starts_with("tool:") {
        if let Some(card) = card {
            tool_heading(ui, role, card, text);
        } else {
            ui.strong(role);
        }
    } else {
        ui.strong(role);
    }
    markdown::render_blocks(ui, row_index, blocks);
}

/// Draw a tool-card heading (running/done/error) above a tool row body.
fn tool_heading(ui: &mut Ui, role: &str, card: &ToolCard, text: &str) {
    let (state_text, color) = match card.state {
        crate::state::ToolState::Running => ("in progress…", egui::Color32::from_rgb(235, 190, 80)),
        crate::state::ToolState::Done => ("complete", egui::Color32::from_rgb(120, 200, 140)),
        crate::state::ToolState::Error => ("error", egui::Color32::from_rgb(235, 90, 90)),
    };
    let name = if card.name.is_empty() {
        role
    } else {
        &card.name
    };
    ui.horizontal(|ui| {
        ui.strong(egui::RichText::new(name).color(color));
        ui.label(egui::RichText::new(state_text).small().color(color));
    });
    if let Some(args) = &card.args {
        if !args.is_empty() && *args != text {
            let mut shown: String = args.chars().take(400).collect();
            if shown != *args {
                shown.push('…');
            }
            ui.add(egui::Label::new(egui::RichText::new(shown).monospace().weak()).wrap());
        }
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
    /// taller than the viewport.
    fn sample_rows(n: usize) -> (Vec<(String, String)>, Vec<Option<ToolCard>>) {
        let mut rows = Vec::with_capacity(n);
        let mut cards = Vec::with_capacity(n);
        for i in 0..n {
            match i % 8 {
                3 => {
                    rows.push((
                        "tool: shell".into(),
                        format!("```text\n# output {i}\nexit 0\n```"),
                    ));
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
    /// parse each side once, then lay out every row on every frame.
    fn reference_content_height(rows: &[(String, String)], cards: &[Option<ToolCard>]) -> f32 {
        let parsed: Vec<Vec<markdown::Block>> = rows
            .iter()
            .map(|(_, text)| markdown::parse_markdown(text))
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
                            ui.group(|ui| {
                                render_row_contents(ui, i, role, cards[i].as_ref(), text, blocks)
                            });
                        }
                    });
                content_h = out.content_size.y;
            });
        }
        content_h
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

    /// (b) A width change invalidates every measurement: the reflow frame
    /// rebuilds all rows, then warm behavior resumes.
    #[test]
    fn width_change_reflows_everything_then_warms_up() {
        let n = 1200;
        let (rows, cards) = sample_rows(n);
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                cache.show(ui, TAB, true, &rows, &cards);
            });
        }
        assert!(cache.last_built < n / 4, "expected a warm frame first");
        // Narrower window: stale heights, so this frame measures all rows.
        run_frame(&ctx, input(FRAMES, 1000.0), |ui| {
            cache.show(ui, TAB, true, &rows, &cards);
        });
        assert_eq!(cache.last_built, n, "reflow frame must rebuild every row");
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

    /// (d) A very tall code row mid-history must not be column-wrapped or
    /// height-capped, and the virtual total must still match the reference.
    #[test]
    fn tall_code_row_is_measured_full_height() {
        let (mut rows, _) = sample_rows(50);
        let tall = 25;
        rows.insert(
            tall,
            (
                "assistant".into(),
                format!("```text\n{}```", "tool output line\n".repeat(1500)),
            ),
        );
        let cards: Vec<Option<ToolCard>> = (0..rows.len()).map(|_| None).collect();

        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        for frame in 0..FRAMES {
            run_frame(&ctx, input(frame, W), |ui| {
                cache.show(ui, TAB, true, &rows, &cards);
            });
        }
        let tall_h = cache.heights[tall];
        assert!(
            tall_h > 1.0e4,
            "tall row measured {tall_h}px; expected > 10000 (no column wrap/cap)"
        );

        let (virtual_h, _) = virtual_content_height(&rows, &cards);
        let reference_h = reference_content_height(&rows, &cards);
        assert!(
            (virtual_h - reference_h).abs() <= 2.0,
            "virtual content height {virtual_h}px != reference {reference_h}px"
        );
    }
}
