use bone::llm::TokenStats;
use bone::tools::ApprovalMode;
use bone::ui::autocomplete::AutocompleteState;
use bone::ui::input::InputState;
use bone::ui::pane_page::PanePage;
use bone::ui::prompt::Prompt;
use bone::ui::render::{PaneDraw, Renderer, StatusInfo};
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

    assert!(row_text(&terminal, 40, 87).contains("Accept"));
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

    assert_eq!(Renderer::desired_height(&input, None, 20, &[], 0, None), 5);
}

#[test]
fn composer_reserves_terminal_final_column_like_submitted_user_text() {
    let mut input = InputState::default();
    input.buffer = "a".repeat(17);
    input.cursor_pos = input.buffer.chars().count();

    assert_eq!(Renderer::desired_height(&input, None, 20, &[], 0, None), 4);
}

#[test]
fn composer_height_uses_the_same_word_wrapping_as_rendering() {
    let input = InputState {
        buffer: "alpha beta gamma".to_string(),
        cursor_pos: 0,
        ..Default::default()
    };

    assert_eq!(Renderer::desired_height(&input, None, 10, &[], 0, None), 5);
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
        Renderer::desired_height(&input, Some(&prompt), 80, &[], 0, None),
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
        interaction: None,
    }];

    // Without pages: top_sep(1) + input(1) + status(1) = 3
    assert_eq!(Renderer::desired_height(&input, None, 80, &[], 0, None), 3);

    // With 3-line page: base(3) + page_sep(1) + content(3) = 7
    assert_eq!(
        Renderer::desired_height(&input, None, 80, &pages, 0, None),
        7
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
        interaction: None,
    }];

    // base(3) + page_sep(1) + tool-requested content rows(12)
    assert_eq!(
        Renderer::desired_height(&input, None, 80, &pages, 0, None),
        16
    );
}

#[test]
fn pane_page_with_two_pages_adds_tab_indicator() {
    let input = InputState::default();
    let pages = vec![
        PanePage {
            source: "tasks".to_string(),
            title: "tasks (2)".to_string(),
            content: vec![ratatui::text::Line::raw("task 1")],
            visible_rows: bone::ui::render::DEFAULT_PANE_ROWS,
            scroll: 0,
            interaction: None,
        },
        PanePage {
            source: "notes".to_string(),
            title: "notes".to_string(),
            content: vec![ratatui::text::Line::raw("note 1")],
            visible_rows: bone::ui::render::DEFAULT_PANE_ROWS,
            scroll: 0,
            interaction: None,
        },
    ];

    // base(3) + page_sep(1) + content(1) + page_sep(1) + tab_indicator(1) = 7
    assert_eq!(
        Renderer::desired_height(&input, None, 80, &pages, 0, None),
        7
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
        interaction: None,
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
        interaction: None,
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

    // Row layout (5 rows total):
    // 0: top sep
    // 1: input "> "
    // 2: page sep
    // 3: "hello pane"
    // 4: status bar
    assert!(row_text(&terminal, 3, 40).contains("hello pane"));
    assert!(row_text(&terminal, 4, 40).contains("test-model"));
}






