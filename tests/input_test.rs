use bone::ui::input::InputState;

#[test]
fn inserted_multiline_text_is_kept_in_the_input_buffer() {
    let mut input = InputState::default();
    input.buffer = "ac".to_string();
    input.cursor_pos = 1;

    input.insert_text("one\ntwo");

    assert_eq!(input.buffer, "aone\ntwoc");
    assert_eq!(input.cursor_pos, 8);
}

#[test]
fn pasted_terminal_line_endings_are_normalized_to_newlines() {
    let mut input = InputState::default();

    input.insert_paste("one\r\ntwo\rthree");

    assert_eq!(input.buffer, "one\ntwo\nthree");
    assert_eq!(input.cursor_pos, 13);
}
