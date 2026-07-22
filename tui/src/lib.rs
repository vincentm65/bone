//! Bone TUI crate: UI module tree plus explicit core re-exports.
//!
//! No `pub use bone_core::*` — keep the binary/API surface auditable. UI code
//! still reaches core via `crate::…`; the binary may also use `bone_core::…`.

pub mod ui;

#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use bone_core::{
    agent, chat, commands, config, ext, llm, pane_content, processes, rpc, run, runtime,
    session_db, session_sink, shell_split, tools, update_check, util,
};
