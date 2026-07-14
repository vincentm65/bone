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

/// Lock the inbox mutex, panicking on poison (same as `unwrap_or_else`).
fn lock_inbox() -> std::sync::MutexGuard<'static, VecDeque<String>> {
    inbox().lock().unwrap_or_else(|e| e.into_inner())
}

/// Queue a prompt for the frontend to submit on its next tick.
///
/// Bounded: beyond [`MAX_INBOX`] the oldest entry is dropped, so a runaway Lua
/// loop can't grow the queue without bound. The only drain is the UI event
/// loop, so this also bounds memory in non-interactive/headless runs.
pub fn push(text: String) {
    let mut q = lock_inbox();
    q.push_back(text);
    while q.len() > MAX_INBOX {
        q.pop_front();
    }
}

/// Maximum prompts held in the inbox before the oldest is dropped.
const MAX_INBOX: usize = 256;

/// Take all queued prompts in FIFO order (empty when nothing is pending).
pub fn drain() -> Vec<String> {
    lock_inbox().drain(..).collect()
}

/// Take the single oldest queued prompt, or `None` when empty. Used by the
/// daemon's background-injection tick, which runs one turn at a time and so
/// consumes the queue one prompt per idle tick (rather than draining all at
/// once like an interactive frontend that queues the rest locally).
pub fn pop() -> Option<String> {
    lock_inbox().pop_front()
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod inbox_tests;
