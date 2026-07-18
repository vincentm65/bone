use bone::ext::snapshots::{InputBorderSnapshot, InputStyleSnapshot};
use bone::llm::TokenStats;
use bone::tools::ApprovalMode;
use bone::ui::autocomplete::AutocompleteState;
use bone::ui::input::InputState;
use bone::ui::pane_page::PanePage;
use bone::ui::prompt::Prompt;
use bone::ui::render::{InputStyle, PaneDraw, Renderer, StatusInfo};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn status_info() -> StatusInfo {
    let mut status_show = std::collections::HashMap::new();
    for key in &[
        "status_show_model",
        "status_show_approval",
        "status_show_tokens_curr",
        "status_show_tokens_in",
        "status_show_tokens_out",
        "status_show_tokens_total",
        "status_show_queue",
        "status_show_spinner",
        "status_show_timer",
    ] {
        status_show.insert((*key).to_string(), true);
    }
    StatusInfo {
        model: "test-model".to_string(),
        token_stats: TokenStats::new(),
        streaming_completion_tokens: None,
        streaming: false,
        approval_mode: ApprovalMode::Safe,
        queue_len: 0,
        status_show,
        elapsed: None,
        lua_status: Vec::new(),
        spinner_frames: Vec::new(),
        spinner_speed_ms: 0,
        spinner_texts: Vec::new(),
        spinner_text_rotate: true,
        spinner_text_speed_ms: 0,
        spinner_elapsed_ms: 0,
    }
}

fn pane_args<'a>(
    input: &'a InputState,
    status_info: &'a StatusInfo,
    pages: &'a [PanePage],
    active_page: usize,
    autocomplete: Option<&'a AutocompleteState>,
) -> PaneDraw<'a> {
    PaneDraw {
        input,
        status_info,
        pages,
        active_page,
        autocomplete,
        running: &[],
    }
}
fn row_text(terminal: &Terminal<TestBackend>, row: u16, width: u16) -> String {
    (0..width)
        .map(|column| {
            terminal
                .backend()
                .buffer()
                .cell((column, row))
                .unwrap()
                .symbol()
        })
        .collect()
}

fn screen_text(terminal: &Terminal<TestBackend>, width: u16, height: u16) -> String {
    (0..height)
        .map(|row| row_text(terminal, row, width))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn running_shell_row_shows_state_elapsed_time_and_syntax_colors() {
    let renderer = Renderer::new();
    let input = InputState::default();
    let mut status = status_info();
    status.spinner_frames = vec!["⠋".to_string()];
    let running = vec![(
        "call-1".to_string(),
        "shell rg -n \"needle\" tui/src".to_string(),
        std::time::Instant::now() - std::time::Duration::from_secs(2),
    )];
    let args = PaneDraw {
        input: &input,
        status_info: &status,
        pages: &[],
        active_page: 0,
        autocomplete: None,
        running: &running,
    };
    let mut terminal = Terminal::new(TestBackend::new(60, 5)).unwrap();

    terminal
        .draw(|frame| renderer.draw_bottom_pane(frame, &args, None))
        .unwrap();

    let row = row_text(&terminal, 0, 60);
    assert!(row.starts_with("⠋ RUNNING  2."), "row: {row:?}");
    assert!(row.contains("rg -n \"needle\" tui/src"), "row: {row:?}");
    assert!(!row.contains("shell rg"), "row: {row:?}");

    let theme = bone::ui::theme::Theme::default();
    let program_col = unicode_width::UnicodeWidthStr::width(row.split("rg").next().unwrap()) as u16;
    let flag_col = unicode_width::UnicodeWidthStr::width(row.split("-n").next().unwrap()) as u16;
    let buffer = terminal.backend().buffer();
    assert_eq!(
        buffer.cell((program_col, 0)).unwrap().fg,
        theme.shell_program
    );
    assert_eq!(buffer.cell((flag_col, 0)).unwrap().fg, theme.shell_flag);
}

#[test]
fn running_shell_rows_show_parallel_position_and_truncate() {
    let renderer = Renderer::new();
    let input = InputState::default();
    let running = vec![
        (
            "call-1".to_string(),
            "shell cargo test --workspace".to_string(),
            std::time::Instant::now() - std::time::Duration::from_secs(3),
        ),
        (
            "call-2".to_string(),
            "shell cargo clippy --workspace --all-targets".to_string(),
            std::time::Instant::now() - std::time::Duration::from_secs(4),
        ),
    ];
    let status = status_info();
    let args = PaneDraw {
        input: &input,
        status_info: &status,
        pages: &[],
        active_page: 0,
        autocomplete: None,
        running: &running,
    };
    let mut terminal = Terminal::new(TestBackend::new(34, 6)).unwrap();

    terminal
        .draw(|frame| renderer.draw_bottom_pane(frame, &args, None))
        .unwrap();

    let first = row_text(&terminal, 0, 34);
    let second = row_text(&terminal, 1, 34);
    assert!(first.contains("[1/2] cargo test"), "row: {first:?}");
    assert!(second.contains("[2/2] cargo clippy…"), "row: {second:?}");
}

#[test]
fn expanded_command_preview_is_clipped_to_a_short_frame() {
    let renderer = Renderer::new();
    let mut prompt = Prompt::new("shell", vec!["Accept", "Advise", "Cancel"]);
    prompt.full_command = Some((0..80).map(|i| format!("echo line {i}\n")).collect());
    prompt.peek_mode = true;
    let input = InputState::default();
    let mut terminal = Terminal::new(TestBackend::new(87, 45)).unwrap();

    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(
                frame,
                &pane_args(&input, &status_info(), &[], 0, None),
                Some(&prompt),
            )
        })
        .unwrap();

    assert!(row_text(&terminal, 41, 87).contains("Accept"));
    assert!(row_text(&terminal, 44, 87).contains("test-model"));
}

#[test]
fn multiline_input_is_clipped_to_a_short_frame() {
    let renderer = Renderer::new();
    let mut input = InputState::default();
    input.buffer = (0..80).map(|i| format!("line {i}\n")).collect();
    input.cursor_pos = input.buffer.chars().count();
    let mut terminal = Terminal::new(TestBackend::new(20, 8)).unwrap();

    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(
                frame,
                &pane_args(&input, &status_info(), &[], 0, None),
                None,
            )
        })
        .unwrap();

    let screen = screen_text(&terminal, 20, 8);
    // 80-line buffer cannot fit an 8-row frame; late lines are clipped while
    // the start of the buffer remains visible.
    assert!(
        screen.contains("line 0"),
        "start of input should remain visible: {screen:?}"
    );
    assert!(
        !screen.contains("line 79"),
        "overflowing input lines should be clipped: {screen:?}"
    );
    // The short frame must not expand to fit the whole buffer.
    assert_eq!(terminal.backend().buffer().area().height, 8);
}

#[test]
fn multiline_input_renders_hard_newlines_on_separate_rows() {
    let renderer = Renderer::new();
    let mut input = InputState::default();
    input.buffer = "alpha\nbeta".to_string();
    input.cursor_pos = input.buffer.chars().count();
    let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();

    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(
                frame,
                &pane_args(&input, &status_info(), &[], 0, None),
                None,
            )
        })
        .unwrap();

    assert!(row_text(&terminal, 1, 20).starts_with("> alpha"));
    assert!(row_text(&terminal, 2, 20).starts_with("beta"));
    assert!(!row_text(&terminal, 1, 20).contains("beta"));
}

#[test]
fn newline_cursor_marker_is_included_in_input_height() {
    let input = InputState {
        buffer: format!("{}\nnext", "a".repeat(18)),
        cursor_pos: 18,
        ..Default::default()
    };

    assert_eq!(
        Renderer::new().desired_height(&input, None, 20, &[], 0, None, 0),
        6
    );
}

#[test]
fn composer_reserves_terminal_final_column_like_submitted_user_text() {
    let mut input = InputState::default();
    input.buffer = "a".repeat(17);
    input.cursor_pos = input.buffer.chars().count();

    assert_eq!(
        Renderer::new().desired_height(&input, None, 20, &[], 0, None, 0),
        5
    );
}

#[test]
fn composer_height_uses_the_same_word_wrapping_as_rendering() {
    let input = InputState {
        buffer: "alpha beta gamma".to_string(),
        cursor_pos: 0,
        ..Default::default()
    };

    assert_eq!(
        Renderer::new().desired_height(&input, None, 10, &[], 0, None, 0),
        6
    );
}

#[test]
fn prompt_navigation_scrolls_selected_rows_into_view() {
    let mut prompt = Prompt::new(
        "Tools",
        (0..20).map(|i| format!("tool {i}")).collect::<Vec<_>>(),
    );
    prompt.visible_rows = 4;

    for _ in 0..6 {
        prompt.down();
    }

    assert_eq!(prompt.selected, 6);
    assert_eq!(prompt.visible_options(), 3..7);
    prompt.page_up();
    assert_eq!(prompt.selected, 2);
    assert_eq!(prompt.visible_options(), 2..6);
}

#[test]
fn rebuilt_prompt_scrolls_selected_row_into_view() {
    let mut prompt = Prompt::new(
        "Providers",
        (0..20).map(|i| format!("provider {i}")).collect::<Vec<_>>(),
    );
    prompt.visible_rows = 4;
    prompt.set_selected(6);

    assert_eq!(prompt.selected, 6);
    assert_eq!(prompt.visible_options(), 3..7);
}

#[test]
fn long_prompt_uses_a_bounded_viewport_height() {
    let input = InputState::default();
    let prompt = Prompt::new(
        "Providers",
        (0..50).map(|i| format!("provider {i}")).collect::<Vec<_>>(),
    );

    assert_eq!(
        Renderer::new().desired_height(&input, Some(&prompt), 80, &[], 0, None, 0),
        13
    );
}

#[test]
fn pane_page_adds_height_to_viewport() {
    let input = InputState::default();
    let pages = vec![PanePage {
        source: "test".to_string(),
        title: "test page".to_string(),
        content: vec![
            ratatui::text::Line::raw("line 1"),
            ratatui::text::Line::raw("line 2"),
            ratatui::text::Line::raw("line 3"),
        ],
        visible_rows: bone::ui::render::DEFAULT_PANE_ROWS,
        scroll: 0,
    }];

    // Without pages: top_sep(1) + input(1) + bot_sep(1) + status(1) = 4
    assert_eq!(
        Renderer::new().desired_height(&input, None, 80, &[], 0, None, 0),
        4
    );

    // With 3-line page: base(4) + blank separator(1) + content(3) = 8
    assert_eq!(
        Renderer::new().desired_height(&input, None, 80, &pages, 0, None, 0),
        8
    );
}

#[test]
fn pane_page_honors_visible_rows() {
    let input = InputState::default();
    let pages = vec![PanePage {
        source: "test".to_string(),
        title: "test page".to_string(),
        content: (0..20)
            .map(|i| ratatui::text::Line::raw(format!("line {i}")))
            .collect(),
        visible_rows: 12,
        scroll: 0,
    }];

    // base(4) + blank separator(1) + tool-requested content rows(12)
    assert_eq!(
        Renderer::new().desired_height(&input, None, 80, &pages, 0, None, 0),
        17
    );
}

#[test]
fn pane_page_with_two_pages_renders_content() {
    let input = InputState::default();
    let pages = vec![
        PanePage {
            source: "tasks".to_string(),
            title: "tasks (2)".to_string(),
            content: vec![ratatui::text::Line::raw("task 1")],
            visible_rows: bone::ui::render::DEFAULT_PANE_ROWS,
            scroll: 0,
        },
        PanePage {
            source: "notes".to_string(),
            title: "notes".to_string(),
            content: vec![ratatui::text::Line::raw("note 1")],
            visible_rows: bone::ui::render::DEFAULT_PANE_ROWS,
            scroll: 0,
        },
    ];

    // base(4) + blank separator(1) + content(1) = 6
    assert_eq!(
        Renderer::new().desired_height(&input, None, 80, &pages, 0, None, 0),
        6
    );
}

#[test]
fn pane_page_does_not_panic_with_tiny_viewport() {
    let renderer = Renderer::new();
    let input = InputState::default();
    // 10 lines of content but only 4 rows of viewport (minimum)
    let pages = vec![PanePage {
        source: "test".to_string(),
        title: "big page".to_string(),
        content: (0..10)
            .map(|i| ratatui::text::Line::raw(format!("line {i}")))
            .collect(),
        visible_rows: bone::ui::render::DEFAULT_PANE_ROWS,
        scroll: 0,
    }];
    let mut terminal = Terminal::new(TestBackend::new(40, 4)).unwrap();

    // This should not panic — content is clipped to what fits
    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(
                frame,
                &pane_args(&input, &status_info(), &pages, 0, None),
                None,
            )
        })
        .unwrap();

    // Status bar should still be on the last row
    assert!(row_text(&terminal, 3, 40).contains("test-model"));
}

#[test]
fn pane_page_renders_content_between_input_and_status() {
    let renderer = Renderer::new();
    let input = InputState::default();
    let pages = vec![PanePage {
        source: "test".to_string(),
        title: "test".to_string(),
        content: vec![ratatui::text::Line::raw("hello pane")],
        visible_rows: bone::ui::render::DEFAULT_PANE_ROWS,
        scroll: 0,
    }];
    let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();

    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(
                frame,
                &pane_args(&input, &status_info(), &pages, 0, None),
                None,
            )
        })
        .unwrap();

    // Row layout (6 rows total):
    // 0: top sep
    // 1: input "> "
    // 2: input bottom border
    // 3: blank pane separator
    // 4: "hello pane"
    // 5: status bar
    assert!(row_text(&terminal, 2, 40).contains('─'));
    assert!(row_text(&terminal, 3, 40).trim().is_empty());
    assert!(row_text(&terminal, 4, 40).contains("hello pane"));
    assert!(row_text(&terminal, 5, 40).contains("test-model"));
}

#[test]
fn redraw_clears_stale_prompt_and_pane_rows() {
    let renderer = Renderer::new();
    let input = InputState::default();
    let status = status_info();
    let pages = vec![PanePage {
        source: "agents".to_string(),
        title: "Agents".to_string(),
        content: vec![
            ratatui::text::Line::raw("deepseek-1 idle"),
            ratatui::text::Line::raw("deepseek-2 idle"),
        ],
        visible_rows: bone::ui::render::DEFAULT_PANE_ROWS,
        scroll: 0,
    }];
    let mut prompt = Prompt::new(
        "General",
        vec![
            "approval_mode                  danger".to_string(),
            "auto_compact_tokens            75000".to_string(),
            "auto_compact_keep_messages     3".to_string(),
        ],
    );
    prompt.hint = Some("Enter edit/cycle  Esc close".to_string());
    let width = 80;
    let height = 12;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(
                frame,
                &pane_args(&input, &status, &pages, 0, None),
                Some(&prompt),
            )
        })
        .unwrap();

    let first = screen_text(&terminal, width, height);
    assert!(first.contains("General"));
    assert!(first.contains("deepseek-1 idle"));

    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(frame, &pane_args(&input, &status, &[], 0, None), None)
        })
        .unwrap();

    let second = screen_text(&terminal, width, height);
    assert!(!second.contains("General"));
    assert!(!second.contains("approval_mode"));
    assert!(!second.contains("deepseek-1 idle"));
    assert!(second.contains(">"));
    assert!(second.contains("test-model"));
}

fn input_style(preset: &str) -> InputStyle {
    InputStyle::from_snapshot(&InputStyleSnapshot {
        preset: Some(preset.to_string()),
        ..Default::default()
    })
}

#[test]
fn box_preset_draws_sides_and_corners() {
    let mut renderer = Renderer::new();
    renderer.input_style = input_style("box");
    let mut input = InputState::default();
    input.buffer = "hello".to_string();
    input.cursor_pos = input.buffer.chars().count();
    let status = status_info();
    let height = renderer.desired_height(&input, None, 20, &[], 0, None, 0);
    let mut terminal = Terminal::new(TestBackend::new(20, height)).unwrap();

    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(frame, &pane_args(&input, &status, &[], 0, None), None)
        })
        .unwrap();

    let top = row_text(&terminal, 0, 20);
    let input_row = row_text(&terminal, 1, 20);
    let bottom = row_text(&terminal, 2, 20);
    assert!(top.starts_with('╭') && top.ends_with('╮'));
    assert!(input_row.starts_with("│ > hello") && input_row.ends_with('│'));
    assert!(bottom.starts_with('╰') && bottom.ends_with('╯'));
}

#[test]
fn filled_preset_fills_three_composer_rows() {
    let mut renderer = Renderer::new();
    renderer.input_style = input_style("filled");
    let input = InputState::default();
    let status = status_info();
    let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();

    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(frame, &pane_args(&input, &status, &[], 0, None), None)
        })
        .unwrap();

    assert!(row_text(&terminal, 1, 20).starts_with(" >"));
    for y in 0..=2 {
        assert_eq!(
            terminal.backend().buffer().cell((0, y)).unwrap().bg,
            renderer.theme.input_bg
        );
        assert_eq!(
            terminal.backend().buffer().cell((18, y)).unwrap().bg,
            renderer.theme.input_bg
        );
    }
}

#[test]
fn custom_prefix_padding_and_border_glyphs_are_applied() {
    let mut renderer = Renderer::new();
    renderer.input_style = InputStyle::from_snapshot(&InputStyleSnapshot {
        preset: Some("box".to_string()),
        prefix: Some("λ ".to_string()),
        horizontal_padding: Some(2),
        vertical_padding: Some(1),
        border: InputBorderSnapshot {
            horizontal: Some("-".to_string()),
            vertical: Some("|".to_string()),
            top_left: Some("+".to_string()),
            top_right: Some("+".to_string()),
            bottom_left: Some("[".to_string()),
            bottom_right: Some("]".to_string()),
        },
        ..Default::default()
    });
    let input = InputState::default();
    let status = status_info();
    let height = renderer.desired_height(&input, None, 16, &[], 0, None, 0);
    let mut terminal = Terminal::new(TestBackend::new(16, height)).unwrap();

    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(frame, &pane_args(&input, &status, &[], 0, None), None)
        })
        .unwrap();

    assert_eq!(height, 6);
    assert_eq!(row_text(&terminal, 0, 16), "+--------------+");
    assert!(row_text(&terminal, 1, 16).starts_with('|'));
    assert!(row_text(&terminal, 2, 16).starts_with("|  λ "));
    assert_eq!(row_text(&terminal, 4, 16), "[--------------]");
}

#[test]
fn box_wrapping_and_autocomplete_stay_inside_the_composer() {
    let mut renderer = Renderer::new();
    renderer.input_style = input_style("box");
    let mut input = InputState::default();
    input.buffer = "/help with a long suffix".to_string();
    input.cursor_pos = input.buffer.chars().count();
    let autocomplete = AutocompleteState::new(vec![
        ("help".to_string(), "show help".to_string()),
        ("history".to_string(), "show history".to_string()),
    ]);
    let status = status_info();
    let height = renderer.desired_height(&input, None, 14, &[], 0, Some(&autocomplete), 0);
    let mut terminal = Terminal::new(TestBackend::new(14, height)).unwrap();

    terminal
        .draw(|frame| {
            renderer.draw_bottom_pane(
                frame,
                &pane_args(&input, &status, &[], 0, Some(&autocomplete)),
                None,
            )
        })
        .unwrap();

    assert!(
        height > 9,
        "wrapped input and autocomplete must be measured"
    );
    for row in 1..height - 2 {
        let text = row_text(&terminal, row, 14);
        assert!(
            text.starts_with('│') && text.ends_with('│'),
            "row {row}: {text:?}"
        );
    }
    assert!(row_text(&terminal, height - 2, 14).starts_with('╰'));
}

#[test]
fn all_presets_clip_without_panicking_in_a_tiny_terminal() {
    for preset in ["lines", "box", "filled"] {
        let mut renderer = Renderer::new();
        renderer.input_style = input_style(preset);
        let input = InputState {
            buffer: "wide text".to_string(),
            cursor_pos: 9,
            ..Default::default()
        };
        let status = status_info();
        let mut terminal = Terminal::new(TestBackend::new(1, 2)).unwrap();
        terminal
            .draw(|frame| {
                renderer.draw_bottom_pane(frame, &pane_args(&input, &status, &[], 0, None), None)
            })
            .unwrap();
    }
}
