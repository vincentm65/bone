//! Regression fixture for transcript overwrites during inline viewport growth.
//!
//! Uses real Renderer/ratatui calls, but constructs a menu page directly rather
//! than running Lua or a daemon. Run `ask_user_swallow_repro.sh` in tmux 100x24.
//!
//! A: flush messages above the 4-row idle viewport.
//! A2: inject DECSTBM rows 1..20 (unless REPRO_SCROLL_REGION=full). This makes
//!     newlines below that region stop scrolling; it is fault injection, not
//!     evidence that the live app leaves a region active. DECSTBM also homes
//!     the cursor, which the subsequent viewport clear repositions.
//! B: open a menu, growing 4 -> 13. Before the fix, ratatui's append_lines
//!    emitted newlines without scrolling, and the new pane overwrote messages.
//! C: hard reset + transcript replay, as after a physical resize.
//! D/E: close and reopen the menu to check shrinking and repeated growth.
//!
//! Each marker includes viewport and terminal geometry. With REPRO_MARKER_DIR,
//! wait for `<phase>.continue` so captures cannot race the next draw; without
//! it, pause briefly per phase for manual inspection.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use bone::chat::Message;
use bone::llm::TokenStats;
use bone::tools::ApprovalMode;
use bone::ui::input::InputState;
use bone::ui::pane_page::PanePage;
use bone::ui::render::{PaneDraw, PaneSizing, Renderer, StatusInfo};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const SENTINEL: &str = "SENTINEL-LAST-AGENT-LINE-4f9c";

fn marker(name: &str, note: &str) -> io::Result<()> {
    // A cursor report is an output barrier: tmux has processed the preceding
    // draw before the driver sees the marker and captures the pane.
    io::stdout().flush()?;
    crossterm::cursor::position()?;
    if let Ok(dir) = std::env::var("REPRO_MARKER_DIR") {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(format!("{dir}/{name}"), note)?;
        let resume = std::path::PathBuf::from(format!("{dir}/{name}.continue"));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !resume.exists() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, name.to_owned()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    } else {
        std::thread::sleep(Duration::from_millis(2500));
    }
    Ok(())
}

/// Long enough that, after the user message + inter-message blanks, the
/// flushed block is taller than the space above the idle viewport at H=24 —
/// exactly like a real conversation — so the sentinel lands on the last row
/// above the viewport (row H-5), the same position the real app's last
/// message line occupies when ask_user opens.
fn assistant_body() -> String {
    let mut body = String::new();
    body.push_str("Here are the deployment targets bone can build for:\n");
    body.push_str("\n");
    for (i, target) in [
        "linux/amd64 — the primary target",
        "darwin/arm64 — Apple silicon",
        "windows/x86_64 — MSVC build",
    ]
    .iter()
    .enumerate()
    {
        body.push_str(&format!("{i}. {target}\n"));
    }
    body.push_str("\n");
    body.push_str("Each target is produced by the same build pipeline; the\n");
    body.push_str("cross-compiler flags are selected per target in the CI\n");
    body.push_str("matrix. Artifacts are cached for repeat builds.\n");
    body.push_str("\n");
    body.push_str("Let me know which one you want and I will kick off a\n");
    body.push_str("build for it.\n");
    body.push_str("\n");
    body.push_str("A full matrix build takes about four minutes on the CI\n");
    body.push_str("runners, and the artifacts land in the release bucket\n");
    body.push_str("under the current git short SHA.\n");
    body.push_str("\n");
    body.push_str("You can also pass --target to the CLI directly if you\n");
    body.push_str("just need a one-off binary for local testing. The\n");
    body.push_str("flag accepts a comma-separated list, matching the\n");
    body.push_str("release pipeline.\n");
    // No trailing newline: SENTINEL is the final content line of the
    // message, i.e. the "last visible line of the agent message".
    body.push_str(SENTINEL);
    body
}

/// Representative menu lines; Lua is not invoked by this renderer fixture.
fn menu_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "Question 1 of 1",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Which deployment target should bone use?",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![Span::raw(" > "), Span::raw("linux/amd64")])
            .style(Style::default().bg(Color::Rgb(0x3A, 0x3F, 0x4B))),
        Line::from("  darwin/arm64"),
        Line::from("  windows/x86_64"),
        Line::from(Span::styled(
            "↑↓/j/k move · Enter select · Esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
        Line::raw(""),
    ]
}

fn status() -> StatusInfo {
    StatusInfo {
        model: "repro".into(),
        token_stats: TokenStats::default(),
        streaming_completion_tokens: None,
        streaming: false,
        approval_mode: ApprovalMode::Safe,
        queue_len: 0,
        incognito: false,
        status_show: std::collections::HashMap::new(),
        elapsed: None,
        lua_status: vec![],
        spinner_frames: vec![],
        spinner_speed_ms: 100,
        spinner_texts: vec![],
        spinner_text_rotate: false,
        spinner_text_speed_ms: 0,
        spinner_elapsed_ms: 0,
    }
}

fn main() -> std::io::Result<()> {
    let mut renderer = Renderer::new();
    let mut term = Renderer::init_terminal(3)?; // MIN_ROWS

    let input = InputState::default();
    let status = status();
    let running: [(String, String, Instant); 0] = [];
    let messages = vec![
        Message::user("Which deployment targets does bone support?"),
        Message::assistant(assistant_body()),
    ];

    // --- Phase A: idle pane + messages flushed to scrollback -------------
    {
        let sizing = PaneSizing {
            input: &input,
            prompt: None,
            pages: &[],
            active_page: 0,
            autocomplete: None,
            running: 0,
        };
        renderer.ensure_viewport_height(&mut term, &sizing)?;
        let draw = PaneDraw {
            input: &input,
            status_info: &status,
            pages: &[],
            active_page: 0,
            autocomplete: None,
            running: &running,
        };
        term.draw(|frame| renderer.draw_bottom_pane(frame, &draw, None))?;
    }
    renderer.flush_new_to_scrollback(&messages, &mut term)?;
    let height = term.size().map(|s| s.height).unwrap_or(0);
    marker(
        "phase_a",
        &format!(
            "viewport={} terminal_h={}",
            renderer.viewport_height, height
        ),
    )?;

    // --- Phase A2: optional restricted-scroll-region fault injection ------
    let region = std::env::var("REPRO_SCROLL_REGION").unwrap_or_else(|_| "restricted".into());
    match region.as_str() {
        "restricted" => {
            crossterm::queue!(io::stdout(), crossterm::style::Print("\x1b[1;20r"))?;
            io::stdout().flush()?;
        }
        "full" => {}
        _ => {
            return Err(io::Error::other(
                "REPRO_SCROLL_REGION must be restricted or full",
            ));
        }
    }
    marker(
        "phase_a2",
        &format!(
            "region={region} viewport={} terminal_h={height}",
            renderer.viewport_height
        ),
    )?;

    // --- Phase B: ask_user menu page opens -> viewport grows 4 -> 13 ------
    let page = PanePage {
        source: "interact".into(),
        title: "ask_user".into(),
        content: menu_lines(),
        visible_rows: menu_lines().len(),
        scroll: 0,
    };
    {
        let sizing = PaneSizing {
            input: &input,
            prompt: None,
            pages: std::slice::from_ref(&page),
            active_page: 0,
            autocomplete: None,
            running: 0,
        };
        renderer.ensure_viewport_height(&mut term, &sizing)?; // real resize path
        let draw = PaneDraw {
            input: &input,
            status_info: &status,
            pages: std::slice::from_ref(&page),
            active_page: 0,
            autocomplete: None,
            running: &running,
        };
        term.draw(|frame| renderer.draw_bottom_pane(frame, &draw, None))?;
    }
    marker(
        "phase_b",
        &format!(
            "viewport={} terminal_h={}",
            renderer.viewport_height, height
        ),
    )?;

    // --- Phase C: hard reset + re-flush (the app's resize recovery) ------
    Renderer::hard_reset_viewport(&mut term, renderer.viewport_height)?;
    renderer.reset_scrollback_state();
    renderer.flush_new_to_scrollback(&messages, &mut term)?;
    {
        let draw = PaneDraw {
            input: &input,
            status_info: &status,
            pages: std::slice::from_ref(&page),
            active_page: 0,
            autocomplete: None,
            running: &running,
        };
        term.draw(|frame| renderer.draw_bottom_pane(frame, &draw, None))?;
    }
    marker("phase_c", &format!("viewport={}", renderer.viewport_height))?;

    // Closing and reopening must neither lose nor duplicate transcript rows.
    for (phase, pages) in [
        ("phase_d", &[][..]),
        ("phase_e", std::slice::from_ref(&page)),
    ] {
        renderer.ensure_viewport_height(
            &mut term,
            &PaneSizing {
                input: &input,
                prompt: None,
                pages,
                active_page: 0,
                autocomplete: None,
                running: 0,
            },
        )?;
        let draw = PaneDraw {
            input: &input,
            status_info: &status,
            pages,
            active_page: 0,
            autocomplete: None,
            running: &running,
        };
        term.draw(|frame| renderer.draw_bottom_pane(frame, &draw, None))?;
        marker(phase, &format!("viewport={}", renderer.viewport_height))?;
    }

    // Leave the terminal's scroll region in its default (whole-screen) state.
    crossterm::queue!(io::stdout(), crossterm::style::Print("\x1b[r"))?;
    io::stdout().flush()?;
    Renderer::shutdown_terminal()?;
    let _ = Renderer::prepare_exit(&mut term);
    eprintln!("repro finished");
    Ok(())
}
