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
    Status { message: String },
    Notice { message: String },
    TextDelta { text: String },
    ReasoningDelta { text: String },
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
    TokenUsage {
        sent: u64,
        received: u64,
        context_length: u64,
    },
    KeyRequest { id: u64 },
    ApprovalRequest {
        id: u64,
        call_id: String,
        name: String,
        summary: String,
        #[serde(default)]
        arguments: serde_json::Value,
        #[serde(default)]
        blocked: Option<String>,
        auto_allows: bool,
    },
    Finished { content: String },
    Failed { message: String },
    StateSnapshot { snapshot: SessionSnapshot },
    ConversationLoaded {
        messages: Vec<ChatMessage>,
        snapshot: SessionSnapshot,
    },
    TurnComplete,
    ViewDiff { diff: ViewDiff },
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
    ApprovalReply { id: u64, outcome: CallOutcome },
    KeyReply { id: u64, key: KeyEvent },
    Cancel,
    RunCommand { name: String, input: String },
    NewConversation,
    LoadConversation { id: i64 },
    ClearConversation,
    ReplaceConversation {
        messages: Vec<ChatMessage>,
    },
    SwitchProvider { provider_id: String },
    ReloadExtensions,
    /// Set the daemon's authoritative approval mode (`"safe"` / `"danger"`).
    /// The frontend sends this whenever the user cycles Safe/Danger so the
    /// daemon's `SharedApprovalMode` — which actually gates tool calls — tracks
    /// the UI instead of staying pinned at its startup value.
    SetApprovalMode { mode: String },
    /// Append a message to the daemon's transcript without running a turn. Used
    /// for context the frontend produces locally (e.g. inline `!command` output)
    /// so a subsequent model turn can still see it now that the daemon owns the
    /// transcript.
    AppendMessage { role: String, content: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ChatRole;
    use serde_json::json;

    fn roundtrip_event(ev: &RuntimeEvent) -> RuntimeEvent {
        let s = serde_json::to_string(ev).expect("serialize");
        serde_json::from_str(&s).expect("deserialize")
    }

    fn json_of(ev: &RuntimeEvent) -> serde_json::Value {
        serde_json::to_value(ev).expect("to_value")
    }

    #[test]
    fn every_runtime_event_variant_round_trips() {
        let variants = vec![
            RuntimeEvent::Started {
                approval: "safe".into(),
                task: "do it".into(),
                model: "m".into(),
            },
            RuntimeEvent::Status {
                message: "thinking".into(),
            },
            RuntimeEvent::Notice {
                message: "compacted".into(),
            },
            RuntimeEvent::TextDelta { text: "hi".into() },
            RuntimeEvent::ReasoningDelta { text: "hmm".into() },
            RuntimeEvent::ToolCall {
                id: "c1".into(),
                name: "shell".into(),
                summary: "ls".into(),
                arguments: json!({ "command": "ls" }),
            },
            RuntimeEvent::ToolResult {
                name: "shell".into(),
                call_id: "c1".into(),
                is_error: false,
                content: "files".into(),
            },
            RuntimeEvent::TokenUsage {
                sent: 10,
                received: 2,
                context_length: 8,
            },
            RuntimeEvent::KeyRequest { id: 7 },
            RuntimeEvent::ApprovalRequest {
                id: 3,
                call_id: "c1".into(),
                name: "shell".into(),
                summary: "shell: ls".into(),
                arguments: json!({ "command": "ls" }),
                blocked: None,
                auto_allows: false,
            },
            RuntimeEvent::Finished {
                content: "done".into(),
            },
            RuntimeEvent::Failed {
                message: "boom".into(),
            },
            RuntimeEvent::StateSnapshot {
                snapshot: SessionSnapshot {
                    sent: 100,
                    received: 20,
                    cached: 5,
                    cost: 0.01,
                    request_count: 3,
                    context_length: 42,
                    transcript_len: 8,
                    conversation_id: Some(7),
                    session_seq: 15,
                    provider_id: "openai".into(),
                    provider_model: "gpt-4o".into(),
                    usage_by_provider: vec![],
                },
            },
            RuntimeEvent::ConversationLoaded {
                messages: vec![ChatMessage::new(ChatRole::User, "hi")],
                snapshot: SessionSnapshot::default(),
            },
            RuntimeEvent::TurnComplete,
            RuntimeEvent::ViewDiff {
                diff: ViewDiff::SetHighlight {
                    name: "accent".into(),
                    fg: Some("#abcdef".into()),
                },
            },
            RuntimeEvent::CommandComplete {
                output: "done".into(),
                submit: false,
                display_role: Some("assistant".into()),
                action: None,
            },
            RuntimeEvent::CommandComplete {
                output: "switched".into(),
                submit: false,
                display_role: None,
                action: Some(CommandAction {
                    conversation_replace: None,
                    conversation_load: Some(ConversationLoad {
                        messages: vec![ChatMessage::new(ChatRole::User, "past")],
                        conversation_id: Some(9),
                    }),
                    config_action: Some(ConfigAction::SwitchProvider {
                        id: "anthropic".into(),
                    }),
                }),
            },
        ];
        for ev in &variants {
            assert_eq!(
                json_of(ev),
                json_of(&roundtrip_event(ev)),
                "round-trip {ev:?}"
            );
        }
    }

    #[test]
    fn every_runtime_command_variant_round_trips() {
        let cmds = vec![
            RuntimeCommand::SubmitPrompt {
                text: "hi".into(),
                images: vec![],
            },
            RuntimeCommand::ApprovalReply {
                id: 3,
                outcome: CallOutcome::Blocked("user advice".into()),
            },
            RuntimeCommand::KeyReply {
                id: 7,
                key: KeyEvent {
                    code: "Enter".into(),
                    char: None,
                    ctrl: false,
                    alt: false,
                    shift: false,
                },
            },
            RuntimeCommand::Cancel,
            RuntimeCommand::RunCommand {
                name: "usage".into(),
                input: "".into(),
            },
            RuntimeCommand::NewConversation,
            RuntimeCommand::LoadConversation { id: 42 },
            RuntimeCommand::ClearConversation,
            RuntimeCommand::ReplaceConversation {
                messages: vec![ChatMessage::new(ChatRole::User, "replacement")],
            },
            RuntimeCommand::SwitchProvider {
                provider_id: "anthropic".into(),
            },
            RuntimeCommand::ReloadExtensions,
        ];
        for cmd in &cmds {
            let s = serde_json::to_string(cmd).expect("serialize");
            let back: RuntimeCommand = serde_json::from_str(&s).expect("deserialize");
            assert_eq!(
                serde_json::to_value(cmd).unwrap(),
                serde_json::to_value(&back).unwrap(),
                "round-trip {cmd:?}"
            );
        }
    }
}
