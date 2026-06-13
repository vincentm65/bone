//! Streaming `<think>` tag stripping in the OpenAI-compat SSE layer.
//!
//! Providers like MiniMax-M2 and Qwen emit chain-of-thought inline in
//! `content` wrapped in `<think>…</think>`. The parser must split this into
//! thoughts vs. real text even when tags are torn across arbitrary chunk
//! boundaries, and must be a no-op for content with no tags at all.

use bone::llm::providers::openai_compat::ThinkParser;

/// Feed `s` one byte at a time, returning the concatenated (text, thoughts).
fn feed_byte_wise(s: &str) -> (String, String) {
    let mut parser = ThinkParser::new();
    let mut text = String::new();
    let mut thoughts = String::new();
    for byte in s.as_bytes() {
        let chunk = std::str::from_utf8(std::slice::from_ref(byte)).unwrap();
        let (t, th) = parser.feed(chunk);
        text.push_str(&t);
        thoughts.push_str(&th);
    }
    // Drain any buffered partial-tag tail (no more input coming).
    let (t, th) = parser.feed("");
    text.push_str(&t);
    thoughts.push_str(&th);
    (text, thoughts)
}

#[test]
fn strips_think_block_split_at_every_byte() {
    let raw = "<think>\nThe user said hi.\n</think>\n\nHello there!";
    let (text, thoughts) = feed_byte_wise(raw);
    assert_eq!(text, "Hello there!");
    assert_eq!(thoughts, "The user said hi.\n");
}

#[test]
fn whole_block_in_one_chunk() {
    let mut parser = ThinkParser::new();
    let (t, th) = parser.feed("<think>reasoning here</think>answer");
    assert_eq!(t, "answer");
    assert_eq!(th, "reasoning here");
}

#[test]
fn no_tags_passes_through_unchanged() {
    let mut parser = ThinkParser::new();
    let (t, th) = parser.feed("just a normal answer, with a < b math");
    assert_eq!(t, "just a normal answer, with a < b math");
    assert!(th.is_empty());
}

#[test]
fn stray_lt_does_not_swallow_following_text() {
    // A lone `<` that turns out not to be a tag must still be emitted.
    let (t, th) = feed_byte_wise("a < b and c < d");
    assert_eq!(t, "a < b and c < d");
    assert!(th.is_empty());
}

#[test]
fn multiple_think_blocks() {
    let raw = "<think>one</think>mid<think>two</think>end";
    let (t, th) = feed_byte_wise(raw);
    assert_eq!(t, "midend");
    assert_eq!(th, "onetwo");
}

#[test]
fn unclosed_think_treats_remainder_as_thoughts() {
    let mut parser = ThinkParser::new();
    let (t, th) = parser.feed("<think>lost in thought, no close");
    assert!(t.is_empty());
    assert_eq!(th, "lost in thought, no close");
}
