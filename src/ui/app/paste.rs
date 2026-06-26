//! Non-bracketed paste-burst coalescing.
//!
//! Terminals with bracketed-paste support deliver a paste as a single
//! [`Event::Paste`], which the app inserts verbatim. Terminals without it
//! (notably Windows conhost) deliver a paste as a flood of individual `Char`
//! and `Enter` key events. These helpers detect such a flood and collapse it
//! into one [`InputState::insert_paste`] so a large paste renders as a single
//! placeholder and costs a single redraw.
//!
//! Detection is timing-based but latency-safe. An isolated keystroke is never
//! delayed: every probe is a non-blocking `poll(0)` until the burst is
//! *confirmed* (two or more characters already collected). Only then do we
//! bridge short gaps between events with [`BURST_QUIET`], so a multi-line
//! paste isn't split across two reads. A lone "type a char then Enter to
//! submit" is therefore unaffected — the Enter is handed back as the trailing
//! submit key — while a real multi-line paste survives intact.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::super::input::{InputAction, InputState};

/// How long to wait for the next event once a paste burst is *confirmed*
/// (two or more characters already collected).
///
/// This bridges the small scheduling gaps between the events of a single
/// paste so a multi-line paste isn't split when its bytes straddle two reads.
/// It is only ever waited on during a real paste, so it never adds latency to
/// ordinary typing.
const BURST_QUIET: Duration = Duration::from_millis(10);

/// Result of [`apply_input_key_with_paste_burst`]: the input action to take,
/// plus any trailing event the caller must still process.
pub(super) struct PasteKeyResult {
    pub(super) action: InputAction,
    pub(super) trailing: Option<Event>,
}

/// A drained paste burst: the coalesced text and an optional trailing event.
pub(super) struct PasteBurst {
    pub(super) text: String,
    pub(super) trailing: Option<Event>,
}

/// Whether `text` is long enough to be treated as a paste rather than an
/// ordinary keystroke. Two or more characters => a paste.
pub(super) fn is_paste_burst(text: &str) -> bool {
    text.chars().nth(1).is_some()
}

/// A key event carrying a plain (no Ctrl/Alt) printable character on press.
pub(super) fn plain_char(key: &KeyEvent) -> Option<char> {
    if key.kind == KeyEventKind::Press
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && let KeyCode::Char(c) = key.code
    {
        Some(c)
    } else {
        None
    }
}

/// Starting from `first`, drain consecutive paste content — plain chars,
/// `Enter` (as a newline), and bracketed-paste payloads — into one string.
///
/// The first non-content key event (or the queue running quiet) ends the
/// burst; that event is returned as `trailing` for the caller to handle.
pub(super) fn collect_paste_burst(first: char) -> io::Result<PasteBurst> {
    let mut text = String::new();
    text.push(first);
    let mut trailing = None;
    loop {
        // Non-blocking until confirmed; quiet-wait afterward so a multi-line
        // paste isn't split by a momentary gap between its events.
        let wait = if is_paste_burst(&text) {
            BURST_QUIET
        } else {
            Duration::ZERO
        };
        if !event::poll(wait)? {
            break;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let plain = !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                match key.code {
                    KeyCode::Char(c) if plain => text.push(c),
                    // Treat Enter as paste content (a newline) once the burst
                    // is confirmed, OR if more input is already queued behind
                    // it. The latter re-captures pastes whose first line is a
                    // single char (is_paste_burst is still false at that point);
                    // the poll is non-blocking so ordinary "type then submit"
                    // latency is unaffected. A lone char + Enter with nothing
                    // queued hands Enter back as the trailing submit key.
                    KeyCode::Enter
                        if plain && (is_paste_burst(&text) || event::poll(Duration::ZERO)?) =>
                    {
                        text.push('\n');
                    }
                    _ => {
                        trailing = Some(Event::Key(key));
                        break;
                    }
                }
            }
            // Release/repeat events are noise mid-flood; keep draining.
            Event::Key(_) => {}
            Event::Paste(p) => text.push_str(&p.replace("\r\n", "\n").replace('\r', "\n")),
            other => {
                trailing = Some(other);
                break;
            }
        }
    }
    Ok(PasteBurst { text, trailing })
}

/// Apply a key event to the input, coalescing a non-bracketed paste flood
/// into a single insert. Returns the resulting action plus any trailing
/// event the caller must still handle.
pub(super) fn apply_input_key_with_paste_burst(
    input: &mut InputState,
    key: KeyEvent,
) -> io::Result<PasteKeyResult> {
    if let Some(c) = plain_char(&key) {
        let burst = collect_paste_burst(c)?;
        if is_paste_burst(&burst.text) {
            input.history_index = None;
            input.insert_paste(&burst.text);
            return Ok(PasteKeyResult {
                action: InputAction::Redraw,
                trailing: burst.trailing,
            });
        }
        // Not a paste: apply the single char normally.
        input.paste_mode = false;
        return Ok(PasteKeyResult {
            action: input.apply_key(key.code, key.modifiers),
            trailing: burst.trailing,
        });
    }

    // Non-printable key. If more input is already queued behind it we're
    // likely mid-paste (a flood that led with a non-printable), so let Enter
    // insert a newline instead of submitting. `paste_mode` is the thin
    // fallback for that edge case; char-led floods are handled above.
    input.paste_mode = event::poll(Duration::ZERO).unwrap_or(false);
    Ok(PasteKeyResult {
        action: input.apply_key(key.code, key.modifiers),
        trailing: None,
    })
}
