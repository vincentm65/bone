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
        /// Optional short user-row label when the daemon starts an automated turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<String>,
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
        /// Daemon-computed edit_file diff preview, rendered by the frontend
        /// instead of re-resolving the file locally. `None` for non-edit_file
        /// calls or when preview generation fails (non-fatal).
        #[serde(default)]
        preview: Option<String>,
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
    /// Conversation-scoped snapshots of daemon-owned background processes.
    ProcessesSnapshot {
        version: u64,
        processes: Vec<ProcessSnapshot>,
    },
    /// Boot-time resolved display state (settings, renderer presets, banner, and
    /// command list) owned by the daemon, so a frontend can render the user's
    /// customizations without running Lua itself. Sent on connect and re-sent
    /// after settings or extensions reload. The snapshot is carried as opaque
    /// JSON to keep the protocol crate free of the core's config types.
    FrontendState {
        banner: String,
        /// Serialized unified resolved frontend settings payload.
        settings: serde_json::Value,
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
        /// Structured named sub-agents. Lua-backed entries are promoted into
        /// canonical config when changed through a frontend.
        #[serde(default)]
        subagents: Vec<crate::session::SubagentDefinition>,
    },
    /// Daemon-owned schema and redacted resolved configuration.
    ConfigSnapshot {
        schema: crate::config::ConfigSchema,
        snapshot: crate::config::ConfigSnapshot,
    },
    /// Successful aggregate mutation. The included snapshot is authoritative.
    ConfigChanged {
        changed_paths: Vec<String>,
        schema: crate::config::ConfigSchema,
        snapshot: crate::config::ConfigSnapshot,
        restart_required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// Rejected mutation, including stale-revision conflicts.
    ConfigMutationRejected {
        current_revision: u64,
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
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
    /// Result of daemon-side keymap rhs dispatch and optional Lua callback.
    KeymapDispatched {
        kind: KeymapDispatchKind,
    },
}

/// Serializable daemon-owned background process state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessSnapshot {
    pub id: String,
    pub command: String,
    pub owner: String,
    pub running: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub error: Option<String>,
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
    ApplyRestartRequired,
    ReloadTools,
    SwitchProvider { id: String },
}

/// Frontend → daemon command.
///
/// # Ownership (who needs each variant)
///
/// Both the in-process TUI and `bone serve` / `--connect` share one daemon
/// control plane (`run_daemon`). Background job / `bone.submit` injection is
/// always daemon-owned; `forward_view_diffs` still selects whether the daemon
/// drains Lua `UiState` onto the event bus. Freeze remote-only growth until
/// serve is a committed product surface.
///
/// | Command | Owner path | Why it exists |
/// |---------|------------|---------------|
/// | `SubmitPrompt` | both | start a model turn |
/// | `ApprovalReply` / `KeyReply` | both | interactive gates mid-turn |
/// | `Cancel` / `Steer` | both | turn control |
/// | `CancelJob` | both | cancel one background sub-agent by id |
/// | `RunCommand` | both | slash commands on the daemon VM |
/// | `NewConversation` / `LoadConversation` / `ClearConversation` | both | durable chat lifecycle |
/// | `ReplaceConversation` | both | bulk transcript replace (e.g. compact) |
/// | `AppendMessage` | both | frontend-local context (e.g. `!shell`) into daemon transcript |
/// | `SwitchProvider` / `ReloadExtensions` / `ReloadSettings` | both | runtime config |
/// | `SetApprovalMode` | both | UI Safe/Danger → daemon gate |
/// | `SetTerminalWidth` | both | Lua `ctx.ui.width` on the daemon VM |
/// | `DispatchHook` | both (esp. remote) | fire Lua hooks when the client has no local VM |
/// | `KeymapDispatch` | both | resolve keymap rhs on the daemon |
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
    /// Cancel one background sub-agent owned by this conversation.
    CancelJob {
        id: String,
    },
    /// Request the current conversation's daemon-owned process snapshots.
    GetProcesses,
    /// Cancel one daemon-owned process in the current conversation.
    CancelProcess {
        id: String,
    },
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
    /// Reload only canonical `config.yaml`, preserving extension/tool runtime
    /// state, then broadcast a fresh full frontend settings snapshot.
    ReloadSettings,
    /// Request the current schema and redacted aggregate snapshot.
    GetConfig,
    SetConfigValue {
        path: String,
        value: serde_json::Value,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    ResetConfigValue {
        path: String,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    UpsertProvider {
        provider: crate::config::ProviderUpdate,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    DeleteProvider {
        id: String,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    SetActiveProvider {
        id: String,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    SetToolEnabled {
        name: String,
        enabled: bool,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    SetCommandEnabled {
        name: String,
        enabled: bool,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// Validate and atomically persist one registered extension setting.
    SetSetting {
        path: String,
        value: serde_json::Value,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// Create or replace one canonical config-backed sub-agent.
    UpsertSubagent {
        agent: crate::session::SubagentDefinition,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// Remove one canonical config-backed sub-agent.
    DeleteSubagent {
        name: String,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// Enable or disable one canonical config-backed sub-agent.
    SetSubagentEnabled {
        name: String,
        enabled: bool,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
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
    /// Dispatch a keymap action to the daemon for rhs classification and
    /// optional Lua callback execution. The frontend sends this instead of
    /// locally resolving action semantics. The daemon responds with a
    /// [`KeymapDispatched`] event.
    KeymapDispatch {
        /// The action string looked up from the binding (may be a callback id).
        action: String,
    },
}

/// Classified result of a keymap rhs dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymapDispatchKind {
    /// Callback completed without requesting a frontend action.
    Noop,
    /// A local built-in action the frontend knows how to execute
    /// (e.g. "toggle_panes", "paste_image").
    Builtin { action: String },
    /// A slash-command string (e.g. "/help").
    Command { text: String },
    /// A prompt to submit as a user message.
    Prompt { text: String },
}

#[cfg(test)]
#[path = "event_tests.rs"]
mod event_tests;
