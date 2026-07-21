//! Wire protocol types for bone: events, commands, and shared data structures.
//!
//! This crate is the single source of truth for types that cross the
//! frontend↔daemon boundary. `bone-core` depends on it and re-exports
//! everything; a non-Rust client can depend on just this crate.

pub mod config;
pub mod event;
pub mod input;
pub mod message;
pub mod session;
pub mod tokens;
pub mod tools;
pub mod view;

pub use config::{
    ConfigPage, ConfigSchema, ConfigSnapshot, ProviderConfig, ProviderUpdate, SettingDefinition,
};
pub use event::{
    CommandAction, ConfigAction, ConversationLoad, KeymapDispatchKind, RuntimeCommand, RuntimeEvent,
};
pub use input::KeyEvent;
pub use message::{
    ChatMessage, ChatRole, ImageData, OutputItem, Reasoning, ReasoningItem, ToolCall, ToolResult,
};
pub use session::{SessionSnapshot, SubagentDefinition, UsageProviderContext};
pub use tokens::{CHARS_PER_TOKEN, TokenStats, format_tokens};
pub use tools::{CallOutcome, ToolDefinition, ToolOutput};
pub use view::{
    Align, Anchor, Component, FloatRect, PaneContent, PaneLineSpec, PaneSpanSpec, StatusSegment,
    ViewDiff, view_diff_from_pane_content,
};
