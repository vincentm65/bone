//! Process-global submit inbox: Lua → the running frontend.
//!
//! `bone.api.submit(text)` queues a prompt here from any Lua context (a tool, a
//! command, an autocmd handler). The interactive frontend drains it on its event
//! loop and submits it like typed input — between turns, or queued behind the
//! active turn. This is the steering primitive behind plugins like `/btw`: Lua
//! can drive the agent without holding a frontend handle.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

fn inbox() -> &'static Mutex<VecDeque<String>> {
    static INBOX: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    INBOX.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Queue a prompt for the frontend to submit on its next tick.
///
/// Bounded: beyond [`MAX_INBOX`] the oldest entry is dropped, so a runaway Lua
/// loop can't grow the queue without bound. The only drain is the UI event
/// loop, so this also bounds memory in non-interactive/headless runs.
pub fn push(text: String) {
    let mut q = inbox()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    q.push_back(text);
    while q.len() > MAX_INBOX {
        q.pop_front();
    }
}

/// Maximum prompts held in the inbox before the oldest is dropped.
const MAX_INBOX: usize = 256;

/// Take all queued prompts in FIFO order (empty when nothing is pending).
pub fn drain() -> Vec<String> {
    inbox()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The inbox is a process-global singleton, so tests touching it must run
    // one at a time — otherwise their pushes/drains interleave.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn push_then_drain_is_fifo_and_empties() {
        let _guard = TEST_LOCK.lock().unwrap();
        push("first".into());
        push("second".into());
        assert_eq!(drain(), vec!["first".to_string(), "second".to_string()]);
        assert!(drain().is_empty(), "drain clears the inbox");
    }

    #[test]
    fn push_is_bounded_and_drops_oldest() {
        let _guard = TEST_LOCK.lock().unwrap();
        for i in 0..(MAX_INBOX + 5) {
            push(i.to_string());
        }
        let got = drain();
        assert_eq!(got.len(), MAX_INBOX);
        // Oldest five were dropped; the cap's first surviving entry follows.
        assert_eq!(got.first().map(String::as_str), Some("5"));
        assert_eq!(got.last().map(String::as_str), Some((MAX_INBOX + 4).to_string()).as_deref());
    }
}
