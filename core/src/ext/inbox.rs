//! Runtime-scoped submit inbox: Lua → the owning frontend.
//!
//! Each Lua VM owns one [`SubmitInbox`]. `bone.submit(text)` and
//! `ctx.conversation.submit(text)` enqueue into that VM's inbox, and only the
//! corresponding daemon consumes it. This keeps steering prompts isolated when
//! several conversation actors share a process.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Maximum prompts held by one runtime before the oldest is dropped.
const MAX_INBOX: usize = 256;

/// One Lua-submitted prompt and its optional frontend label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmittedPrompt {
    pub text: String,
    /// `None` shows `text`; an empty label suppresses the user row.
    pub display: Option<String>,
}

/// Parse `{ display = true | false | "label" }` from a Lua submit call.
pub fn parse_display(options: Option<mlua::Table>) -> mlua::Result<Option<String>> {
    let Some(options) = options else {
        return Ok(None);
    };
    match options.get::<mlua::Value>("display")? {
        mlua::Value::Nil | mlua::Value::Boolean(true) => Ok(None),
        mlua::Value::Boolean(false) => Ok(Some(String::new())),
        mlua::Value::String(label) => Ok(Some(label.to_str()?.to_owned())),
        _ => Err(mlua::Error::external(
            "submit display must be true, false, or a string",
        )),
    }
}

/// Bounded FIFO of prompts submitted by one Lua runtime.
#[derive(Clone, Debug, Default)]
pub struct SubmitInbox {
    queue: Arc<Mutex<VecDeque<SubmittedPrompt>>>,
}

impl SubmitInbox {
    /// Queue a normally displayed prompt for the owning frontend's next idle tick.
    pub fn push(&self, text: String) {
        self.push_with_display(text, None);
    }

    /// Queue a prompt with an optional replacement frontend label.
    pub fn push_with_display(&self, text: String, display: Option<String>) {
        let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
        queue.push_back(SubmittedPrompt { text, display });
        while queue.len() > MAX_INBOX {
            queue.pop_front();
        }
    }

    /// Take all queued prompt text in FIFO order.
    pub fn drain(&self) -> Vec<String> {
        self.queue
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(..)
            .map(|prompt| prompt.text)
            .collect()
    }

    /// Take the single oldest queued prompt, or `None` when empty.
    pub fn pop(&self) -> Option<SubmittedPrompt> {
        self.queue
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
    }

    pub(crate) fn same_queue(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.queue, &other.queue)
    }
}

/// Return the inbox attached to `lua`, creating one for bare test/tool VMs.
///
/// Keeping the handle in Lua app-data lets every API surface created from that
/// VM resolve the same queue without threading it through each call context.
pub(crate) fn for_lua(lua: &mlua::Lua) -> SubmitInbox {
    if let Some(inbox) = lua.app_data_ref::<SubmitInbox>() {
        return inbox.clone();
    }

    let inbox = SubmitInbox::default();
    lua.set_app_data(inbox.clone());
    inbox
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod inbox_tests;
