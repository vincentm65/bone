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
        RuntimeEvent::ToolOutput {
            call_id: "c1".into(),
            content: "partial\n".into(),
            stderr: false,
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
            preview: None,
        },
        RuntimeEvent::ApprovalRequest {
            id: 3,
            call_id: "c1".into(),
            name: "edit_file".into(),
            summary: "edit_file: path".into(),
            arguments: json!({ "path": "f", "old_text": "a", "new_text": "b" }),
            blocked: None,
            auto_allows: true,
            preview: Some("--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n".into()),
        },
        RuntimeEvent::Finished {
            content: "done".into(),
        },
        RuntimeEvent::Failed {
            message: "boom".into(),
        },
        RuntimeEvent::WorkElapsed { elapsed_ms: 1234 },
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
            },
        },
        RuntimeEvent::FrontendState {
            banner: "bone".into(),
            settings: json!({
                "version": 1,
                "general": { "approval": "danger", "show_reasoning": true },
                "ui": {},
                "theme": { "palette": { "accent": "#abcdef" } },
                "keymaps": { "normal": [{ "key": "<C-p>", "action": "toggle_panes" }] }
            }),
            commands: vec![("config".into(), "Configure Bone".into())],
            tool_defs: vec![],
            tool_display: json!({}),
        },
        RuntimeEvent::ConversationLoaded {
            messages: vec![ChatMessage::new(ChatRole::User, "hi")],
            snapshot: SessionSnapshot::default(),
        },
        RuntimeEvent::ConversationLoadFailed {
            id: 7,
            message: "missing".into(),
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
            output: "restart".into(),
            submit: false,
            display_role: None,
            action: Some(CommandAction {
                conversation_replace: None,
                conversation_load: None,
                config_action: Some(ConfigAction::ApplyRestartRequired),
            }),
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
        RuntimeEvent::KeymapDispatched {
            kind: KeymapDispatchKind::Noop,
        },
        RuntimeEvent::KeymapDispatched {
            kind: KeymapDispatchKind::Prompt {
                text: "summarize this".into(),
            },
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
        RuntimeCommand::ReloadSettings,
        RuntimeCommand::SetApprovalMode {
            mode: "danger".into(),
        },
        RuntimeCommand::AppendMessage {
            role: "user".into(),
            content: "context".into(),
        },
        RuntimeCommand::DispatchHook {
            name: "mode_change".into(),
            payload: json!({ "mode": "danger" }),
        },
        RuntimeCommand::SetTerminalWidth { width: 120 },
        RuntimeCommand::Steer {
            text: "go left instead".into(),
        },
        RuntimeCommand::KeymapDispatch {
            action: "toggle_panes".into(),
        },
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
