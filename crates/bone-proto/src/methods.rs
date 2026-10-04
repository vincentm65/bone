//! Typed methods and events.
//!
//! Each request is a zero-sized type implementing [`Method`], tying a method
//! name to its params and result types. Server-to-client events implement
//! [`Notification`]. Clients and servers use these instead of raw strings.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::types::{
    AskId, ChatMessage, DeltaKind, SessionId, SessionInfo, ToolCall, TurnId, TurnOutcome, Usage,
};

/// A client-to-server request.
pub trait Method {
    const METHOD: &'static str;
    type Params: Serialize + DeserializeOwned + Send + 'static;
    type Result: Serialize + DeserializeOwned + Send + 'static;
}

/// A server-to-client event.
pub trait Notification {
    const METHOD: &'static str;
    type Params: Serialize + DeserializeOwned + Send + 'static;
}

/// Params for methods that take none. Serialises as `{}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}

macro_rules! method {
    ($(#[$doc:meta])* $ty:ident, $name:literal, $params:ty => $result:ty) => {
        $(#[$doc])*
        pub enum $ty {}
        impl Method for $ty {
            const METHOD: &'static str = $name;
            type Params = $params;
            type Result = $result;
        }
    };
}

macro_rules! notification {
    ($(#[$doc:meta])* $ty:ident, $name:literal, $params:ty) => {
        $(#[$doc])*
        pub enum $ty {}
        impl Notification for $ty {
            const METHOD: &'static str = $name;
            type Params = $params;
        }
    };
}

/// Every request method, in protocol order. Golden tests and the core's
/// handler check against this list, so add new methods here.
pub const METHODS: &[&str] = &[
    Initialize::METHOD,
    Shutdown::METHOD,
    Echo::METHOD,
    SessionCreate::METHOD,
    SessionList::METHOD,
    SessionMessages::METHOD,
    SessionRename::METHOD,
    SessionFork::METHOD,
    SessionDelete::METHOD,
    TurnStart::METHOD,
    TurnCancel::METHOD,
    TurnSteer::METHOD,
    QueueAdd::METHOD,
    QueueRemove::METHOD,
    QueueUpdate::METHOD,
    QueueMove::METHOD,
    QueueClear::METHOD,
    QueueResume::METHOD,
    AskRespond::METHOD,
    HealthCheck::METHOD,
    CoreReload::METHOD,
    PluginList::METHOD,
    PluginLoad::METHOD,
    PluginUnload::METHOD,
    PluginReload::METHOD,
    ModelList::METHOD,
    ModelComplete::METHOD,
    ModelCancel::METHOD,
    McpList::METHOD,
    LuaCall::METHOD,
    StoreQuery::METHOD,
];

/// Every server-to-client event.
pub const NOTIFICATIONS: &[&str] = &[
    Echoed::METHOD,
    TurnStarted::METHOD,
    MessageDelta::METHOD,
    MessageCompleted::METHOD,
    ToolStarted::METHOD,
    ToolFinished::METHOD,
    ToolOutput::METHOD,
    AskRequested::METHOD,
    AskResolved::METHOD,
    TurnFinished::METHOD,
    TurnSteered::METHOD,
    QueueChanged::METHOD,
    CoreReloaded::METHOD,
    SessionUpdated::METHOD,
    SessionDeleted::METHOD,
    ModelDeltaEvent::METHOD,
    ModelCompleted::METHOD,
];

// ---- connection ----------------------------------------------------------

method!(
    /// Handshake. Must be the first request on every connection.
    Initialize, "initialize", InitializeParams => InitializeResult
);
method!(
    /// Ask the server to close this connection after responding.
    Shutdown, "shutdown", Empty => ()
);
method!(
    /// Returns its input and also emits it as an [`Echoed`] event.
    Echo, "echo", EchoParams => EchoParams
);
notification!(
    /// Emitted by the core for every [`Echo`] request.
    Echoed, "echoed", EchoParams
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub client_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    pub protocol_version: u32,
    pub server_name: String,
    pub server_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EchoParams {
    pub text: String,
}

// ---- sessions ------------------------------------------------------------

method!(
    /// Create a new, empty session.
    SessionCreate, "session/create", SessionCreateParams => SessionInfo
);
method!(
    /// All sessions, newest first.
    SessionList, "session/list", Empty => Vec<SessionInfo>
);
method!(
    /// A session's full transcript (loaded from disk if needed).
    SessionMessages, "session/messages", SessionRef => SessionMessagesResult
);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCreateParams {
    /// Working directory for tools. Defaults to the server's cwd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessagesResult {
    pub info: SessionInfo,
    pub messages: Vec<ChatMessage>,
    /// The turn currently running, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn: Option<TurnId>,
    /// Messages waiting to be sent (see `queue/add`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<QueuedMessage>,
    /// The queue waits for `queue/resume` or a new message.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub queue_paused: bool,
}

method!(
    /// Give a session a title (replacing the one taken from its first
    /// message).
    SessionRename, "session/rename", SessionRenameParams => SessionInfo
);
method!(
    /// A new session with a copy of this one's transcript, or of the part
    /// before turn `before_turn` (1 is the first user message), to try
    /// something else from there. The original is untouched.
    SessionFork, "session/fork", SessionForkParams => SessionInfo
);
method!(
    /// Delete a session and its file. Not while a turn runs.
    SessionDelete, "session/delete", SessionRef => ()
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRenameParams {
    pub session_id: SessionId,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionForkParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_turn: Option<u32>,
}

// ---- turns ---------------------------------------------------------------

method!(
    /// Add a user message and run the agent until it stops. Returns as soon
    /// as the turn starts; progress arrives as events.
    TurnStart, "turn/start", TurnStartParams => TurnStartResult
);
method!(
    /// Cancel the session's running turn. No-op if none is running.
    TurnCancel, "turn/cancel", SessionRef => ()
);
method!(
    /// Add a user message to the session's running turn: it joins before
    /// the turn's next model call ([`TurnSteered`] says when). An error if
    /// no turn is running; use `turn/start` then.
    TurnSteer, "turn/steer", TurnSteerParams => ()
);
// ---- the message queue ---------------------------------------------------

method!(
    /// Send a message, queueing it while a turn runs. Idle: it starts a turn
    /// at once (`turn_id`). Running: `steer` joins that turn at its next
    /// step, `next` starts its own turn after it (`id`). Queued messages are
    /// kept with the session, across restarts.
    QueueAdd, "queue/add", QueueAddParams => QueueAddResult
);
method!(
    /// Take a message out of the queue.
    QueueRemove, "queue/remove", QueueItemRef => ()
);
method!(
    /// Change a queued message's text or mode.
    QueueUpdate, "queue/update", QueueUpdateParams => ()
);
method!(
    /// Move a queued message to position `to` (0 is first).
    QueueMove, "queue/move", QueueMoveParams => ()
);
method!(
    /// Empty the queue.
    QueueClear, "queue/clear", SessionRef => ()
);
method!(
    /// Let a paused queue go on (after a cancelled turn or a restart): if
    /// the session is idle, the first message starts a turn.
    QueueResume, "queue/resume", SessionRef => ()
);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueMode {
    /// Joins the running turn before its next model call.
    #[default]
    Steer,
    /// Starts its own turn once the running one ends.
    Next,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub id: u64,
    pub text: String,
    pub mode: QueueMode,
    /// Unix seconds.
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueAddParams {
    pub session_id: SessionId,
    pub text: String,
    #[serde(default)]
    pub mode: QueueMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueAddResult {
    /// It was queued under this id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// It started this turn straight away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItemRef {
    pub session_id: SessionId,
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueUpdateParams {
    pub session_id: SessionId,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<QueueMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueMoveParams {
    pub session_id: SessionId,
    pub id: u64,
    pub to: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueChangedParams {
    pub session_id: SessionId,
    pub items: Vec<QueuedMessage>,
    #[serde(default)]
    pub paused: bool,
}

method!(
    /// Answer an [`AskRequested`] event.
    AskRespond, "ask/respond", AskRespondParams => ()
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStartParams {
    pub session_id: SessionId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSteerParams {
    pub session_id: SessionId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStartResult {
    pub turn_id: TurnId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRespondParams {
    pub ask_id: AskId,
    /// Any JSON; its meaning is agreed between the asking Lua code and the
    /// client (e.g. "allow" / "deny" for the approval plugin).
    pub answer: serde_json::Value,
}

// ---- health --------------------------------------------------------------

method!(
    /// Run the core's checks (configuration, provider, storage, Lua) and
    /// those core Lua registers with `bone.health`.
    HealthCheck, "health/check", Empty => Vec<HealthItem>
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthItem {
    pub name: String,
    pub status: HealthStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Ok,
    Warn,
    Error,
}

// ---- core runtime and plugins ------------------------------------------

method!(
    /// Load the core's Lua configuration again (runtime, plugins, `core.lua`)
    /// and switch to it. Running turns finish on the previous one. Fails, and
    /// changes nothing, if loading fails.
    CoreReload, "core/reload", Empty => ReloadResult
);
method!(
    /// The plugins in the core's plugins folder.
    PluginList, "plugin/list", Empty => Vec<PluginInfo>
);
method!(
    /// Enable a core plugin and reload.
    PluginLoad, "plugin/load", PluginRef => ReloadResult
);
method!(
    /// Disable a core plugin and reload.
    PluginUnload, "plugin/unload", PluginRef => ReloadResult
);
method!(
    /// Reload with a plugin enabled (picks up changes to its files).
    PluginReload, "plugin/reload", PluginRef => ReloadResult
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRef {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    /// It has a `core.lua`.
    pub core: bool,
    /// Its `core.lua` is part of the running configuration.
    pub loaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadResult {
    pub plugins: Vec<PluginInfo>,
    /// Settings that cannot change while running, and were kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

// ---- model calls ----------------------------------------------------------

method!(
    /// The configured providers (`bone.config.providers`).
    ModelList, "model/list", Empty => Vec<ModelInfo>
);
method!(
    /// One model call outside any session (no tools are run). Returns at
    /// once; the answer arrives as [`ModelCompleted`], with
    /// [`ModelDeltaEvent`]s before it when `stream` is set.
    ModelComplete, "model/complete", ModelCompleteParams => ModelRequest
);
method!(
    /// Stop a model call; it completes with the error "cancelled".
    ModelCancel, "model/cancel", ModelRequest => ()
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// The key in `bone.config.providers`.
    pub name: String,
    pub model: String,
    /// The Lua provider type, if it is one.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The provider turns use.
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCompleteParams {
    /// A key of `bone.config.providers`; default: the one turns use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub messages: Vec<ChatMessage>,
    /// Tools to offer, as `{ name, description, parameters }`. The model may
    /// ask for them; nothing runs them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
    /// Overrides for this call: `model`, `reasoning_effort`, and for Lua
    /// providers any of their options.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub options: serde_json::Value,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub request_id: u64,
}

// ---- MCP -------------------------------------------------------------------

method!(
    /// The MCP servers core Lua configured (`bone.mcp.add`).
    McpList, "mcp/list", Empty => Vec<McpServerInfo>
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    /// `"idle"` (lazy, not started), `"starting"`, `"ready"` or `"failed"`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Its tools, by the names the model sees.
    #[serde(default)]
    pub tools: Vec<String>,
}

// ---- core Lua -------------------------------------------------------------

method!(
    /// Call a function core Lua registered with `bone.rpc.register(name,
    /// fn)`. It runs as a job (it may wait), gets `args` and `{ session_id,
    /// cwd }`, and its return value is the result. An error for a name
    /// nothing registered, or when the function fails.
    LuaCall, "lua/call", LuaCallParams => serde_json::Value
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LuaCallParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub args: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

// ---- events --------------------------------------------------------------

notification!(TurnStarted, "turn/started", TurnStartedParams);
notification!(
    /// Streamed model output.
    MessageDelta, "message/delta", MessageDeltaParams
);
notification!(
    /// The model finished one response. `message` is what was appended to
    /// the transcript.
    MessageCompleted, "message/completed", MessageCompletedParams
);
notification!(ToolStarted, "tool/started", ToolStartedParams);
notification!(ToolFinished, "tool/finished", ToolFinishedParams);
notification!(ToolOutput, "tool/output", ToolOutputParams);
notification!(
    /// Core-side Lua asked the user something (`bone.ask`); answer with
    /// [`AskRespond`].
    AskRequested, "ask/requested", AskRequestedParams
);
notification!(
    /// A question was answered (by any client) or dropped because its turn
    /// was cancelled.
    AskResolved, "ask/resolved", AskResolvedParams
);
notification!(TurnFinished, "turn/finished", TurnFinishedParams);
notification!(
    /// A session's queue changed: all of it, as it is now.
    QueueChanged, "queue/changed", QueueChangedParams
);
notification!(
    /// A `turn/steer` message joined the running turn's transcript.
    TurnSteered, "turn/steered", TurnStartedParams
);
notification!(
    /// The core switched to a newly loaded Lua configuration.
    CoreReloaded, "core/reloaded", ReloadResult
);
notification!(
    /// A session was deleted (by any client).
    SessionDeleted, "session/deleted", SessionRef
);
notification!(
    /// Streamed output of a `model/complete` call with `stream` set.
    ModelDeltaEvent, "model/delta", ModelDeltaParams
);
notification!(
    /// A `model/complete` call finished: its assistant `message` and `usage`,
    /// or an `error`.
    ModelCompleted, "model/completed", ModelCompletedParams
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDeltaParams {
    pub request_id: u64,
    pub kind: DeltaKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCompletedParams {
    pub request_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

notification!(
    /// Core Lua changed a session's transcript outside a turn's own
    /// messages (`bone.session.append` or `compact`). Clients should load
    /// it again with `session/messages`.
    SessionUpdated, "session/updated", SessionUpdatedParams
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUpdatedParams {
    pub session_id: SessionId,
    /// `"append"` or `"compact"`.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStartedParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    /// The user message that started the turn, so every attached client can
    /// show it.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDeltaParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub kind: DeltaKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCompletedParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub message: ChatMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStartedParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub call: ToolCall,
    /// When it started, in milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolFinishedParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
    /// How long it ran, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Output a running tool has produced so far, in order, as it comes (the
/// shell tool sends its output this way). `tool/finished` still carries the
/// whole result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub call_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRequestedParams {
    pub ask_id: AskId,
    /// The session the question is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Whatever the Lua code passed to `bone.ask`, e.g.
    /// `{ "kind": "approval", "tool": "shell", "arguments": {...} }`.
    pub question: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskResolvedParams {
    pub ask_id: AskId,
    /// `null` when the question was dropped because the turn was cancelled.
    #[serde(default)]
    pub answer: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFinishedParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub outcome: TurnOutcome,
}

// ---- Store -----------------------------------------------------------------

method!(
    /// One read-only SQL statement against the session index (`index.db`):
    /// tables `sessions`, `messages`, `search` (FTS5), `usage` and
    /// `tool_calls`.
    StoreQuery, "store/query", StoreQueryParams => StoreQueryResult
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreQueryParams {
    pub sql: String,
    /// A list for `?1`, `?2`…, or an object for `:name`.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreQueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    /// There were more rows than one query returns (10,000).
    #[serde(default)]
    pub truncated: bool,
}
