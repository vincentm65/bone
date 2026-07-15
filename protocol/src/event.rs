//! Runtime events and commands — the core frontend↔daemon protocol.

use serde::{Deserialize, Serialize};

use crate::input::KeyEvent;
use crate::message::{ChatMessage, ImageData};
use crate::session::SessionSnapshot;
use crate::tools::CallOutcome;
use crate::view::ViewDiff;

/// Daemon → frontend event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEvent {
    Started {
        approval: String,
        task: String,
        model: String,
    },
    Status {
        message: String,
    },
    Notice {
        message: String,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        summary: String,
        #[serde(default)]
        arguments: serde_json::Value,
    },
    ToolResult {
        name: String,
        call_id: String,
        is_error: bool,
        #[serde(default)]
        content: String,
    },
    /// Incremental output from a running tool. `call_id` keeps concurrent
    /// commands correctly associated in transcript and live-panel clients.
    ToolOutput {
        call_id: String,
        #[serde(default)]
        content: String,
        #[serde(default)]
        stderr: bool,
    },
    TokenUsage {
        sent: u64,
        received: u64,
        context_length: u64,
    },
    KeyRequest {
        id: u64,
    },
    ApprovalRequest {
        id: u64,
        call_id: String,
        name: String,
        summary: String,
        #[serde(default)]
        arguments: serde_json::Value,
        #[serde(default)]
        blocked: Option<String>,
        #[serde(default)]
        auto_allows: bool,
    },
    Finished {
        content: String,
    },
    Failed {
        message: String,
    },
    WorkElapsed {
        elapsed_ms: u64,
    },
    StateSnapshot {
        snapshot: SessionSnapshot,
    },
    /// Boot-time display state the daemon's Lua VM produced (theme/keymap/banner/
    /// command-list/config), so a frontend can render the user's customizations
    /// without running Lua itself. Sent on connect and re-sent after a
    /// `ReloadExtensions`. The snapshots are carried as opaque JSON to keep the
    /// protocol crate free of the core's Lua snapshot types; the consuming client
    /// deserializes them back into `Lua*Snapshot`.
    FrontendState {
        banner: String,
        theme: serde_json::Value,
        keymap: serde_json::Value,
        config: serde_json::Value,
        /// `(name, description)` for slash-command autocomplete.
        commands: Vec<(String, String)>,
        /// Enabled tool definitions, so a VM-less frontend can estimate context
        /// size and (with `tool_display`) render tool rows. Defaults empty for
        /// back-compat with daemons that predate this field.
        #[serde(default)]
        tool_defs: Vec<crate::tools::ToolDefinition>,
        /// `name → ToolDisplayConfig` (opaque JSON; the core type lives outside
        /// the protocol crate) so the frontend can render custom tool rows.
        #[serde(default)]
        tool_display: serde_json::Value,
    },
    ConversationLoaded {
        messages: Vec<ChatMessage>,
        snapshot: SessionSnapshot,
    },
    /// Correlated failure response for `LoadConversation`; lets a waiting
    /// frontend return to input instead of hanging after a database error.
    ConversationLoadFailed {
        id: i64,
        message: String,
    },
    TurnComplete,
    ViewDiff {
        diff: ViewDiff,
    },
    CommandComplete {
        output: String,
        submit: bool,
        display_role: Option<String>,
        /// Frontend action requested by the command's Lua handler, forwarded so
        /// the client can apply it. `None` for plain text/pane/submit commands.
        #[serde(default)]
        action: Option<CommandAction>,
    },
}

/// A frontend-coupled action an interactive command's Lua handler asked for.
///
/// These cannot be applied daemon-side because they read the frontend's local
/// config state (config files, last-provider) or mutate the client's rendered
/// scrollback. The daemon forwards them on `CommandComplete`; the client applies
/// them after the interactive phase. Mirrors the command-relevant subset of
/// `bone-core`'s `LuaReturnAction` (the `before_turn`-only fields are omitted).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandAction {
    /// Replace the active transcript with these messages (compaction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_replace: Option<Vec<ChatMessage>>,
    /// Load a past conversation as the active chat (`/history`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_load: Option<ConversationLoad>,
    /// Config/runtime mutation (`/config` apply, provider switch, tool reload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_action: Option<ConfigAction>,
}

/// Payload for the `conversation.load` action (`/history`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationLoad {
    /// Legacy command payload. New clients ignore this and let the daemon load
    /// the complete authoritative transcript by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<ChatMessage>,
    /// Conversation id to resume; future messages append here.
    #[serde(default)]
    pub conversation_id: Option<i64>,
}

/// Config/runtime mutation requested by an interactive command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigAction {
    Apply,
    ReloadTools,
    SwitchProvider { id: String },
}

/// Frontend → daemon command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCommand {
    SubmitPrompt {
        text: String,
        #[serde(default)]
        images: Vec<ImageData>,
    },
    ApprovalReply {
        id: u64,
        outcome: CallOutcome,
    },
    KeyReply {
        id: u64,
        key: KeyEvent,
    },
    Cancel,
    RunCommand {
        name: String,
        input: String,
    },
    NewConversation,
    LoadConversation {
        id: i64,
    },
    ClearConversation,
    ReplaceConversation {
        messages: Vec<ChatMessage>,
    },
    SwitchProvider {
        provider_id: String,
    },
    ReloadExtensions,
    /// Set the daemon's authoritative approval mode (`"safe"` / `"danger"`).
    /// The frontend sends this whenever the user cycles Safe/Danger so the
    /// daemon's `SharedApprovalMode` — which actually gates tool calls — tracks
    /// the UI instead of staying pinned at its startup value.
    SetApprovalMode {
        mode: String,
    },
    /// Append a message to the daemon's transcript without running a turn. Used
    /// for context the frontend produces locally (e.g. inline `!command` output)
    /// so a subsequent model turn can still see it now that the daemon owns the
    /// transcript.
    AppendMessage {
        role: String,
        content: String,
    },
    /// Fire a fire-and-forget Lua hook (`session_end`, `mode_change`, …) on the
    /// daemon's VM. Used by a remote frontend that has no local VM of its own.
    DispatchHook {
        name: String,
        payload: serde_json::Value,
    },
    /// Publish the live terminal width so Lua panes (`ctx.ui.width`) wrap text
    /// correctly on the daemon's VM. Sent on startup and on every resize.
    SetTerminalWidth {
        width: u16,
    },
    /// Steer the agent mid-turn: inject a system message into the transcript
    /// so the model sees the new direction. The turn continues (not cancelled);
    /// the steering text is also passed to the `before_turn` hook so a Lua
    /// handler can shape the next provider request (e.g. as a `turn_message`).
    Steer {
        text: String,
    },
}

#[cfg(test)]
#[path = "event_tests.rs"]
mod event_tests;
