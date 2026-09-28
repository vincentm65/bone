use super::*;

#[test]
fn styled_text_strips_to_plain_text() {
    assert_eq!(strip_ansi("\x1b[36mfoo\x1b[0m bar"), "foo bar");
    assert_eq!(strip_ansi("\x1b[2m\x1b[37m─\x1b[0m"), "─");
}

#[test]
fn clean_input_is_borrowed() {
    assert!(matches!(strip_ansi("plain text\n"), Cow::Borrowed(_)));
}

#[test]
fn clean_input_with_unicode_is_borrowed() {
    assert!(matches!(strip_ansi("▁▃▅ ⋮ ✻"), Cow::Borrowed(_)));
}

#[test]
fn string_sequences_stop_at_bel_or_st() {
    assert_eq!(strip_ansi("a\x1b]0;title\x07b"), "ab");
    assert_eq!(strip_ansi("a\x1b]0;title\x1b\\b"), "ab");
    assert_eq!(strip_ansi("a\x1bP q\x1b\\b"), "ab");
}

#[test]
fn other_escapes_are_removed() {
    // Two-byte, charset designator, and cursor-save escapes.
    assert_eq!(strip_ansi("a\x1b=b"), "ab");
    assert_eq!(strip_ansi("a\x1b(Bb"), "ab");
    assert_eq!(strip_ansi("a\x1b7b"), "ab");
}

#[test]
fn stray_controls_are_removed_and_layout_kept() {
    assert_eq!(strip_ansi("a\rb\x07c\x7fd"), "abcd");
    assert_eq!(strip_ansi("keep\nthis\tand\nthat"), "keep\nthis\tand\nthat");
}

#[test]
fn unicode_survives_stripping() {
    assert_eq!(strip_ansi("\x1b[1m漢字\x1b[0m"), "漢字");
    assert_eq!(strip_ansi("⠋\x1b[31m⠙\x1b[0m"), "⠋⠙");
}

#[test]
fn truncated_escape_at_end_of_input() {
    assert_eq!(strip_ansi("text\x1b"), "text");
    assert_eq!(strip_ansi("text\x1b["), "text");
    assert_eq!(strip_ansi("text\x1b]0;unterminated"), "text");
}

#[test]
fn malformed_escape_never_splits_a_utf8_character() {
    // The escape parameter scan stops on any byte outside 0x20..=0x3f, which
    // can be a UTF-8 lead byte; the consumed continuation bytes must not leave
    // the slice on a char boundary.
    assert_eq!(strip_ansi("\x1b[é"), "é");
    assert_eq!(strip_ansi("\x1b(é"), "é");
    assert_eq!(strip_ansi("\x1bé"), "é");
    assert_eq!(strip_ansi("\x1b[31mé\x1b[0m"), "é");
}

#[test]
fn escape_before_unicode_keeps_the_character() {
    assert_eq!(strip_ansi("\x1b[1m(\x1b[0m"), "(");
}
