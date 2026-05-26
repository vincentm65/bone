use bone::llm::TokenStats;
use bone::tools::ApprovalMode;
use bone::ui::input::InputState;
use bone::ui::prompt::Prompt;
use bone::ui::render::{Renderer, StatusInfo};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn status_info() -> StatusInfo {
    StatusInfo {
        model: "test-model".to_string(),
        token_stats: TokenStats::new(),
        streaming_completion_tokens: None,
        streaming: false,
        approval_mode: ApprovalMode::Safe,
        queue_len: 0,
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
        .draw(|frame| renderer.draw_bottom_pane(frame, &input, &status_info(), Some(&prompt)))
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
        .draw(|frame| renderer.draw_bottom_pane(frame, &input, &status_info(), None))
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
        .draw(|frame| renderer.draw_bottom_pane(frame, &input, &status_info(), None))
        .unwrap();

    assert!(row_text(&terminal, 1, 20).starts_with("> alpha"));
    assert!(row_text(&terminal, 2, 20).starts_with("beta"));
    assert!(!row_text(&terminal, 1, 20).contains("beta"));
}

#[test]
fn newline_cursor_marker_is_included_in_input_height() {
    let mut input = InputState::default();
    input.buffer = format!("{}\nnext", "a".repeat(18));
    input.cursor_pos = 18;

    assert_eq!(Renderer::desired_height(&input, None, 20), 6);
}

#[test]
fn composer_reserves_terminal_final_column_like_submitted_user_text() {
    let mut input = InputState::default();
    input.buffer = "a".repeat(17);
    input.cursor_pos = input.buffer.chars().count();

    assert_eq!(Renderer::desired_height(&input, None, 20), 5);
}

#[test]
fn composer_height_uses_the_same_word_wrapping_as_rendering() {
    let mut input = InputState::default();
    input.buffer = "alpha beta gamma".to_string();
    input.cursor_pos = 0;

    assert_eq!(Renderer::desired_height(&input, None, 10), 6);
}
