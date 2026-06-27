//! Frontend-neutral input types for the wire protocol.

use serde::{Deserialize, Serialize};

/// Frontend-neutral key event delivered to Lua by `ctx.ui.key()`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub code: String,
    #[serde(default)]
    pub char: Option<String>,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
}
