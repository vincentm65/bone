//! Stripping of terminal control sequences from text the renderer draws.
//!
//! Transcript content is painted as styled cells (the TUI's scrollback and
//! viewport, the desktop's character grid), never written to a terminal as a
//! byte stream, so an escape sequence embedded in message text has no meaning.
//! Left in place it shows as literal `ESC` bytes plus visible parameters
//! (`[0m`) in the transcript, and the Markdown path turns the `ESC` into
//! U+FFFD. Command handlers (Lua plugins such as `/usage`) and shell output
//! routinely carry such sequences, so both frontends sanitize through this
//! shared helper.

use std::borrow::Cow;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Remove ANSI/VT escape sequences and stray control characters, keeping line
/// feeds and tabs. Returns a borrowed slice when there is nothing to strip.
pub fn strip_ansi(input: &str) -> Cow<'_, str> {
    if !input.bytes().any(is_control) {
        return Cow::Borrowed(input);
    }

    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if is_control(bytes[i]) {
            out.push_str(&input[copied..i]);
            i = if bytes[i] == ESC {
                skip_escape(bytes, i)
            } else {
                i + 1
            };
            copied = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&input[copied..]);
    Cow::Owned(out)
}

/// Control characters that carry no meaning in rendered transcript text.
fn is_control(byte: u8) -> bool {
    (byte < 0x20 && byte != b'\n' && byte != b'\t') || byte == 0x7f
}

/// Index just past the escape sequence starting at `start` (which holds `ESC`).
///
/// Only ASCII extends a sequence: a non-ASCII byte directly after `ESC` is not
/// an escape introducer and is kept. The returned index is always a UTF-8
/// character boundary, so malformed input cannot split a character: the CSI and
/// designator forms consume ASCII bytes only, and the string forms stop at an
/// ASCII BEL, at `ESC \`, or at the end of the input.
fn skip_escape(bytes: &[u8], start: usize) -> usize {
    let i = start + 1;
    let Some(&kind) = bytes.get(i) else {
        return i;
    };
    if !kind.is_ascii() {
        return i;
    }
    let mut end = i + 1;
    match kind {
        // CSI: parameters/intermediates, then one final byte in 0x40..=0x7e.
        b'[' => {
            while let Some(&byte) = bytes.get(end) {
                if !byte.is_ascii() {
                    break;
                }
                end += 1;
                if !(0x20..=0x3f).contains(&byte) {
                    break;
                }
            }
        }
        // String sequences (OSC, DCS, SOS, PM, APC): run to BEL or ST.
        b']' | b'P' | b'^' | b'_' => {
            while let Some(&byte) = bytes.get(end) {
                if byte == BEL {
                    end += 1;
                    break;
                }
                if byte == ESC && bytes.get(end + 1) == Some(&b'\\') {
                    end += 2;
                    break;
                }
                end += 1;
            }
        }
        // Character-set designators (`ESC ( B`) carry one more byte.
        b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' | b'#' | b'%' => {
            if bytes.get(end).is_some_and(u8::is_ascii) {
                end += 1;
            }
        }
        // Two-byte escapes (`ESC =`, `ESC c`, `ESC 7`, …).
        _ => {}
    }
    end
}

#[cfg(test)]
#[path = "ansi_tests.rs"]
mod tests;
