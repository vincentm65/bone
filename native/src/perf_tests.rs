//! Renderer-only CPU baseline for the virtualized transcript. Run with
//! `--release --ignored --nocapture`. Does not measure GPU work or end-to-end
//! window frame latency.
//!
//! Warm frames lay out only the visible band plus over-scan, so long histories
//! must stay well under the 16.7 ms (60 fps) frame budget; frame 0 is the cold
//! full-measure pass and is reported separately.
use crate::state::{ToolCard, ToolState};
use crate::transcript::Cache;
use eframe::egui;
use std::time::Instant;

const W: f32 = 1264.0;
const H: f32 = 1412.0;
const TAB_ID: u64 = 1;

#[test]
#[ignore = "manual release performance measurement"]
fn renderer_baseline() {
    let cases: Vec<(String, Vec<(String, String)>, Vec<Option<ToolCard>>, bool)> = vec![
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
            // A single enormous code row is a measurement-correctness case, not
            // a band case: it spans the whole transcript, so every frame still
            // builds it. Kept for the tall-row regression value.
            "tool-100000-lines".into(),
            vec![(
                "tool: shell".into(),
                format!("```text\n{}```", "tool output line\n".repeat(100_000)),
            )],
            vec![Some(ToolCard {
                name: "shell".into(),
                state: ToolState::Done,
                args: None,
            })],
            false,
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
                cache.show(ui, TAB_ID, true, rows, toolcards);
            });
            // Headless frames always produce font-atlas deltas; clearing keeps
            // debug builds from panicking in `TexturesDelta::drop`.
            out.textures_delta.clear();
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            if frame == 0 {
                println!("{name}: cold_ms={elapsed_ms:.2}");
            }
            if frame >= 2 {
                samples.push(elapsed_ms);
            }
        }
        samples.sort_by(f64::total_cmp);
        let p50 = samples[5];
        let p95 = samples[9];
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
        // A resize invalidates every cached height: the reflow frame must
        // rebuild all rows (band-only rendering then resumes at the new width).
        let start = Instant::now();
        let mut out = ctx.run_ui(input(12, 1000.0), |ui| {
            cache.show(ui, TAB_ID, true, rows, toolcards);
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
