/// Event dispatch types and logic.

/// Result of dispatching an event through all Lua handlers.
#[derive(Debug, Clone)]
pub enum EventDispatchResult {
    /// No handler blocked; continue normally.
    Continue,
    /// A handler requested blocking.
    Blocked { reason: String },
}
