//! Non-bracketed paste-burst coalescing.
//!
//! Some terminals (notably Windows conhost) deliver a paste as a flood of
//! individual `Char` key events rather than a single bracketed `Paste` event.
//! These helpers detect such a burst and collapse it into one `insert_paste`
//! so a large paste costs a single redraw and renders as a placeholder.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::io;

use super::super::input::{InputAction, InputState};

pub(super) struct PasteKeyResult {
    pub(super) action: InputAction,
    pub(super) trailing: Option<Event>,
}

pub(super) struct PasteBurst {
    pub(super) text: String,
    pub(super) trailing: Option<Event>,
}

fn non_bracketed_paste_quiet_timeout() -> std::time::Duration {
    #[cfg(windows)]
    {
        std::time::Duration::from_millis(12)
    }
    #[cfg(not(windows))]
    {
        std::time::Duration::from_millis(0)
    }
}

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

pub(super) fn is_paste_burst(text: &str) -> bool {
    text.chars().nth(1).is_some()
}

pub(super) fn collect_non_bracketed_paste_burst(first: char) -> io::Result<PasteBurst> {
    let mut batch = String::new();
    batch.push(first);
    let quiet = non_bracketed_paste_quiet_timeout();
    let trailing = loop {
        if !event::poll(quiet)? {
            break None;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let plain = !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                match key.code {
                    KeyCode::Char(c) if plain => batch.push(c),
                    KeyCode::Enter if plain => {
                        // Treat Enter as an interior newline only if more pasted
                        // input follows within the quiet window. Otherwise leave
                        // it as the trailing submit key.
                        if event::poll(quiet)? {
                            batch.push('\n');
                        } else {
                            break Some(Event::Key(key));
                        }
                    }
                    _ => break Some(Event::Key(key)),
                }
            }
            Event::Key(_) => {}
            Event::Paste(text) => {
                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                batch.push_str(&normalized);
            }
            other => break Some(other),
        }
    };
    Ok(PasteBurst {
        text: batch,
        trailing,
    })
}

pub(super) fn apply_input_key_with_paste_burst(
    input: &mut InputState,
    key: KeyEvent,
) -> io::Result<PasteKeyResult> {
    if let Some(c) = plain_char(&key) {
        let burst = collect_non_bracketed_paste_burst(c)?;
        if is_paste_burst(&burst.text) {
            input.history_index = None;
            input.insert_paste(&burst.text);
            return Ok(PasteKeyResult {
                action: InputAction::Redraw,
                trailing: burst.trailing,
            });
        }
        input.paste_mode = false;
        return Ok(PasteKeyResult {
            action: input.apply_key(key.code, key.modifiers),
            trailing: burst.trailing,
        });
    }

    input.paste_mode = event::poll(std::time::Duration::from_millis(0)).unwrap_or(false);
    Ok(PasteKeyResult {
        action: input.apply_key(key.code, key.modifiers),
        trailing: None,
    })
}
