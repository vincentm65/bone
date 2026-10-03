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
    TurnStart::METHOD,
    TurnCancel::METHOD,
    AskRespond::METHOD,
    HealthCheck::METHOD,
    CoreReload::METHOD,
    PluginList::METHOD,
    PluginLoad::METHOD,
    PluginUnload::METHOD,
    PluginReload::METHOD,
];

/// Every server-to-client event.
pub const NOTIFICATIONS: &[&str] = &[
    Echoed::METHOD,
    TurnStarted::METHOD,
    MessageDelta::METHOD,
    MessageCompleted::METHOD,
    ToolStarted::METHOD,
    ToolFinished::METHOD,
    AskRequested::METHOD,
    AskResolved::METHOD,
    TurnFinished::METHOD,
    CoreReloaded::METHOD,
    SessionUpdated::METHOD,
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
    /// Answer an [`AskRequested`] event.
    AskRespond, "ask/respond", AskRespondParams => ()
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStartParams {
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
    /// The core switched to a newly loaded Lua configuration.
    CoreReloaded, "core/reloaded", ReloadResult
);
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolFinishedParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
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
