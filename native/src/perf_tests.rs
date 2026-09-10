//! Renderer-only CPU baseline for the virtualized transcript. Run with
//! `--release --ignored --nocapture`. Does not measure GPU work or end-to-end
//! window frame latency.
//!
//! Warm frames lay out only the visible band plus over-scan, so long histories
//! must stay well under the 16.7 ms (60 fps) frame budget; frame 0 is the cold
//! full-measure pass and is reported separately.
use crate::state::{ToolCard, ToolState};
use crate::transcript::{CHUNK_LINES, Cache};
use eframe::egui;
use std::time::Instant;

const W: f32 = 1264.0;
const H: f32 = 1412.0;
const TAB_ID: u64 = 1;

/// One renderer scenario: name, transcript rows, tool cards, and whether the
/// warm budget is enforced (true) or only reported (false).
type Case = (String, Vec<(String, String)>, Vec<Option<ToolCard>>, bool);

#[test]
#[ignore = "manual release performance measurement"]
fn renderer_baseline() {
    let cases: Vec<Case> = vec![
        (
            "long-history-8MiB".into(),
            (0..10_000)
                .map(|i| {
                    (
                        "assistant".into(),
                        format!(
                            "## Message {i}\n{}\n- **item**\n",
                            "sample text ".repeat(65)
                        ),
                    )
                })
                .collect(),
            (0..10_000).map(|_| None).collect(),
            true, // hard budget: warm p95 must stay under 16.7 ms
        ),
        (
            "renderer-stress-20MiB".into(),
            (0..10_000)
                .map(|i| {
                    (
                        "assistant".into(),
                        format!("## Message {i}\n{}", "sample text ".repeat(174)),
                    )
                })
                .collect(),
            (0..10_000).map(|_| None).collect(),
            true, // hard budget: warm p95 must stay under 16.7 ms
        ),
        (
            // A single enormous tool output is a boundedness case: the row
            // lays out only the first chunk (CHUNK_LINES lines), so the row
            // height — and every frame — stays bounded no matter the size.
            "tool-100000-lines".into(),
            vec![("tool: shell".into(), "tool output line\n".repeat(100_000))],
            vec![Some(ToolCard {
                name: "shell".into(),
                state: ToolState::Done,
                args: None,
                label: None,
                show_result: None,
                eager: None,
            })],
            false,
        ),
        (
            // Many large tool outputs: warm frames build only the band and each
            // built row is chunk-bounded, so this must meet the 16.7 ms budget.
            "tool-outputs-200x2000".into(),
            (0..200)
                .map(|i| {
                    let log = (0..2000)
                        .map(|j| format!("line {i:03}-{j:04}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    (format!("tool: shell{i}"), log)
                })
                .collect(),
            (0..200)
                .map(|_| {
                    Some(ToolCard {
                        name: "shell".into(),
                        state: ToolState::Done,
                        args: None,
                        label: None,
                        show_result: None,
                        eager: None,
                    })
                })
                .collect(),
            true, // hard budget: warm p95 must stay under 16.7 ms
        ),
    ];

    for (name, rows, toolcards, hard_budget) in &cases {
        let bytes: usize = rows
            .iter()
            .map(|(role, text)| role.len() + text.len())
            .sum();
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        let mut samples = Vec::new();
        for frame in 0..12 {
            let start = Instant::now();
            let mut out = ctx.run_ui(input(frame, W), |ui| {
                cache.show(
                    ui,
                    TAB_ID,
                    true,
                    rows,
                    toolcards,
                    &crate::theme::ThemeColors::default(),
                );
            });
            // Headless frames always produce font-atlas deltas; clearing keeps
            // debug builds from panicking in `TexturesDelta::drop`.
            out.textures_delta.clear();
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            if frame == 0 {
                println!("{name}: cold_ms={elapsed_ms:.2}");
            }
            if frame >= 3 {
                // Frames 0 and 1 are the cold full-measure and first-warm passes.
                // Frame 2 is skipped too: headless frames rebuild the font atlas,
                // and that one rebuild invalidates the text-layout cache, so frame
                // 2 pays a re-layout that a real app never repeats (its atlas is
                // built once at startup). Steady-state warm frames start at 3.
                samples.push(elapsed_ms);
            }
        }
        samples.sort_by(f64::total_cmp);
        let p50 = samples[4];
        let p95 = samples[8];
        println!(
            "{name}: bytes={bytes} warm_p50_ms={p50:.2} warm_p95_ms={p95:.2} last_built={}",
            cache.last_built
        );
        if *hard_budget {
            assert!(
                p95 < 16.7,
                "{name}: warm p95 {p95:.2} ms exceeded the 16.7 ms frame budget"
            );
        }
        // Tool rows are chunk-bounded: a row whose output exceeds one chunk
        // may never lay out more than its shown chunks (1 by default), so its
        // measured height stays far below an unbounded single-label render.
        for (i, (role, text)) in rows.iter().enumerate() {
            if role.starts_with("tool:") && text.lines().count() > CHUNK_LINES {
                assert!(
                    cache.heights[i] < CHUNK_LINES as f32 * 40.0 + 500.0,
                    "{name}: tool row {i} measured {}px; expected one bounded chunk",
                    cache.heights[i]
                );
            }
        }
        // A resize invalidates every cached height: the reflow frame must
        // rebuild all rows (band-only rendering then resumes at the new width).
        let start = Instant::now();
        let mut out = ctx.run_ui(input(12, 1000.0), |ui| {
            cache.show(
                ui,
                TAB_ID,
                true,
                rows,
                toolcards,
                &crate::theme::ThemeColors::default(),
            );
        });
        out.textures_delta.clear();
        let reflow_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(cache.last_built, rows.len());
        println!(
            "{name}: reflow_ms={reflow_ms:.2} last_built={}",
            cache.last_built
        );
    }
}

fn input(frame: usize, width: f32) -> egui::RawInput {
    egui::RawInput {
        time: Some(frame as f64 * 0.016),
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, H),
        )),
        ..Default::default()
    }
}
