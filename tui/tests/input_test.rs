use bone::ui::input::{
    InputState, MAX_INPUT_HISTORY_BYTES, MAX_INPUT_HISTORY_ENTRIES, PASTE_PLACEHOLDER_THRESHOLD,
};

#[test]
fn inserted_multiline_text_is_kept_in_the_input_buffer() {
    let mut input = InputState {
        buffer: "ac".to_string(),
        cursor_pos: 1,
        ..Default::default()
    };

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

#[test]
fn large_paste_collapses_to_a_placeholder_but_expands_on_submit() {
    let mut input = InputState::default();
    let blob = "x".repeat(PASTE_PLACEHOLDER_THRESHOLD + 1);

    input.insert_text("see: ");
    input.insert_paste(&blob);

    // The visible buffer holds only the short placeholder, not the blob.
    assert!(input.has_pastes());
    assert!(input.buffer.starts_with("see: [Pasted text #1 +"));
    assert!(input.buffer.chars().count() < blob.len());
    // ...but expansion restores the full pasted content.
    assert_eq!(input.expanded(), format!("see: {blob}"));
}

#[test]
fn small_paste_is_inserted_verbatim() {
    let mut input = InputState::default();

    input.insert_paste("just a short paste");

    assert!(!input.has_pastes());
    assert_eq!(input.buffer, "just a short paste");
    assert_eq!(input.expanded(), "just a short paste");
}

#[test]
fn backspace_after_a_placeholder_removes_the_whole_blob() {
    let mut input = InputState::default();
    let blob = "y".repeat(PASTE_PLACEHOLDER_THRESHOLD + 10);

    input.insert_paste(&blob);
    assert!(input.has_pastes());

    // Cursor sits right after the placeholder; one backspace clears it all.
    input.delete_backward();

    assert!(!input.has_pastes());
    assert_eq!(input.buffer, "");
    assert_eq!(input.expanded(), "");
}

#[test]
fn reset_clears_pending_pastes() {
    let mut input = InputState::default();
    input.insert_paste(&"z".repeat(PASTE_PLACEHOLDER_THRESHOLD + 1));

    input.reset();

    assert!(!input.has_pastes());
    assert_eq!(input.expanded(), "");
}

#[test]
fn input_history_is_bounded_by_count() {
    let mut input = InputState::default();
    for i in 0..(MAX_INPUT_HISTORY_ENTRIES + 10) {
        input.buffer = format!("prompt-{i}");
        input.reset();
    }

    assert_eq!(input.history.len(), MAX_INPUT_HISTORY_ENTRIES);
    assert_eq!(input.history.first().map(String::as_str), Some("prompt-10"));
}

#[test]
fn input_history_is_bounded_by_bytes() {
    let mut input = InputState {
        buffer: "x".repeat(MAX_INPUT_HISTORY_BYTES + 1),
        ..Default::default()
    };
    input.reset();

    assert!(input.history.is_empty());
}
