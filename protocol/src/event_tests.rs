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
            request_id: Some(11),
            approval: "safe".into(),
            task: "do it".into(),
            model: "m".into(),
            display: Some("[job results: reviewer ✓]".into()),
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
        RuntimeEvent::ProcessesSnapshot {
            version: 4,
            processes: vec![ProcessSnapshot {
                id: "process-1".into(),
                command: "cargo test".into(),
                owner: "conversation:7".into(),
                running: true,
                stdout: "running".into(),
                stderr: String::new(),
                exit_code: None,
                signal: None,
                error: None,
            }],
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
                incognito: false,
            },
        },
        RuntimeEvent::StateSynchronized {
            request_id: 17,
            busy: true,
            snapshot: SessionSnapshot::default(),
            messages: Some(vec![ChatMessage::new(ChatRole::User, "repair")]),
        },
        RuntimeEvent::StreamLagged { skipped: 23 },
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
            subagents: vec![],
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
        RuntimeEvent::TurnCompleted { request_id: 11 },
        RuntimeEvent::ViewDiff {
            diff: ViewDiff::SetHighlight {
                name: "accent".into(),
                fg: Some("#abcdef".into()),
            },
        },
        RuntimeEvent::CommandComplete {
            request_id: Some(12),
            output: "done".into(),
            submit: false,
            display_role: Some("assistant".into()),
            action: None,
        },
        RuntimeEvent::CommandComplete {
            request_id: None,
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
            request_id: None,
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
            request_id: None,
            kind: KeymapDispatchKind::Noop,
        },
        RuntimeEvent::KeymapDispatched {
            request_id: Some(13),
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
            request_id: Some(11),
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
        RuntimeCommand::CancelJob { id: "job-1".into() },
        RuntimeCommand::GetProcesses,
        RuntimeCommand::Synchronize {
            request_id: 17,
            include_messages: true,
        },
        RuntimeCommand::CancelProcess {
            id: "process-1".into(),
        },
        RuntimeCommand::RunCommand {
            request_id: Some(12),
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
        RuntimeCommand::SetSetting {
            path: "compact.auto".into(),
            value: json!(true),
            expected_revision: 7,
            request_id: Some("setting-1".into()),
        },
        RuntimeCommand::UpsertSubagent {
            agent: crate::SubagentDefinition {
                name: "researcher".into(),
                description: "Investigates code".into(),
                source: "config".into(),
                ..Default::default()
            },
            expected_revision: 8,
            request_id: Some("subagent-1".into()),
        },
        RuntimeCommand::DeleteSubagent {
            name: "researcher".into(),
            expected_revision: 9,
            request_id: None,
        },
        RuntimeCommand::SetSubagentEnabled {
            name: "researcher".into(),
            enabled: false,
            expected_revision: 10,
            request_id: None,
        },
        RuntimeCommand::SetApprovalMode {
            mode: "danger".into(),
        },
        RuntimeCommand::SetIncognito { enabled: true },
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
            request_id: Some(13),
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

#[test]
fn session_snapshot_incognito_defaults_off_for_legacy_wire_messages() {
    // A snapshot serialized by a daemon predating the flag has no `incognito`
    // key and must deserialize with it off.
    let snapshot: SessionSnapshot = serde_json::from_value(json!({
        "sent": 0, "received": 0, "cached": 0, "cost": 0.0, "request_count": 0,
        "context_length": 0, "transcript_len": 0, "conversation_id": null,
        "session_seq": 0, "provider_id": "", "provider_model": ""
    }))
    .unwrap();
    assert!(!snapshot.incognito);
    let snapshot: SessionSnapshot = serde_json::from_value(json!({
        "sent": 1, "received": 2, "cached": 0, "cost": 0.0, "request_count": 1,
        "context_length": 3, "transcript_len": 4, "conversation_id": null,
        "session_seq": 5, "provider_id": "p", "provider_model": "m", "incognito": true
    }))
    .unwrap();
    assert!(snapshot.incognito);
}

#[test]
fn synchronize_defaults_to_snapshot_only() {
    let command: RuntimeCommand =
        serde_json::from_value(json!({ "synchronize": { "request_id": 9 } })).unwrap();
    assert!(matches!(
        command,
        RuntimeCommand::Synchronize {
            request_id: 9,
            include_messages: false
        }
    ));

    let event: RuntimeEvent = serde_json::from_value(json!({
        "state_synchronized": {
            "request_id": 9,
            "busy": false,
            "snapshot": SessionSnapshot::default()
        }
    }))
    .unwrap();
    assert!(matches!(
        event,
        RuntimeEvent::StateSynchronized {
            request_id: 9,
            busy: false,
            messages: None,
            ..
        }
    ));
}

#[test]
fn request_ids_are_optional_for_legacy_wire_messages() {
    let started: RuntimeEvent = serde_json::from_value(json!({
        "started": {
            "approval": "safe",
            "task": "hi",
            "model": "m",
            "display": null
        }
    }))
    .unwrap();
    assert!(matches!(
        started,
        RuntimeEvent::Started {
            request_id: None,
            ..
        }
    ));

    let submit: RuntimeCommand =
        serde_json::from_value(json!({ "submit_prompt": { "text": "hi", "images": [] } })).unwrap();
    assert!(matches!(
        submit,
        RuntimeCommand::SubmitPrompt {
            request_id: None,
            ..
        }
    ));

    let command: RuntimeCommand =
        serde_json::from_value(json!({ "run_command": { "name": "help", "input": "" } })).unwrap();
    assert!(matches!(
        command,
        RuntimeCommand::RunCommand {
            request_id: None,
            ..
        }
    ));

    let keymap: RuntimeCommand =
        serde_json::from_value(json!({ "keymap_dispatch": { "action": "noop" } })).unwrap();
    assert!(matches!(
        keymap,
        RuntimeCommand::KeymapDispatch {
            request_id: None,
            ..
        }
    ));

    let complete: RuntimeEvent = serde_json::from_value(json!({
        "command_complete": {
            "output": "",
            "submit": false,
            "display_role": null,
            "action": null
        }
    }))
    .unwrap();
    assert!(matches!(
        complete,
        RuntimeEvent::CommandComplete {
            request_id: None,
            ..
        }
    ));

    let dispatched: RuntimeEvent =
        serde_json::from_value(json!({ "keymap_dispatched": { "kind": "noop" } })).unwrap();
    assert!(matches!(
        dispatched,
        RuntimeEvent::KeymapDispatched {
            request_id: None,
            ..
        }
    ));

    let legacy_submit = RuntimeCommand::SubmitPrompt {
        request_id: None,
        text: "hi".into(),
        images: vec![],
    };
    assert_eq!(
        serde_json::to_value(legacy_submit).unwrap(),
        json!({ "submit_prompt": { "text": "hi", "images": [] } })
    );
}
