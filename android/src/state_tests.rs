use super::*;
use bone_protocol::{SessionSnapshot, ToolCall};

fn loaded(messages: Vec<ChatMessage>) -> State {
    let mut state = State::default();
    state.apply(RuntimeEvent::ConversationLoaded {
        messages,
        snapshot: SessionSnapshot {
            conversation_id: Some(7),
            provider_model: "gpt".into(),
            ..Default::default()
        },
        busy: false,
    });
    state
}

fn texts(state: &State) -> Vec<(RowKind, &str)> {
    state
        .rows
        .iter()
        .map(|row| (row.kind, row.text.as_str()))
        .collect()
}

#[test]
fn loading_a_conversation_builds_rows_and_refreshes_the_list() {
    let mut call = ChatMessage::new(ChatRole::Assistant, "");
    call.tool_calls = vec![ToolCall {
        id: "c1".into(),
        name: "shell".into(),
        arguments: serde_json::json!({}),
    }];
    let mut result = ChatMessage::new(ChatRole::Tool, "boom");
    result.tool_call_id = Some("c1".into());
    result.is_error = true;
    let mut state = State::default();
    let replies = state.apply(RuntimeEvent::ConversationLoaded {
        messages: vec![
            ChatMessage::new(ChatRole::User, "hi"),
            call,
            result,
            ChatMessage::new(ChatRole::Assistant, "done"),
        ],
        snapshot: SessionSnapshot {
            conversation_id: Some(7),
            provider_model: "gpt".into(),
            ..Default::default()
        },
        busy: false,
    });
    assert_eq!(
        texts(&state),
        [
            (RowKind::User, "hi"),
            (RowKind::Tool, "shell"),
            (RowKind::Assistant, "done")
        ]
    );
    assert!(state.rows[1].failed, "tool results mark their call row");
    assert_eq!(
        (state.conversation_id, state.model.as_str()),
        (Some(7), "gpt")
    );
    assert!(matches!(
        replies.as_slice(),
        [RuntimeCommand::HostRequest {
            request: HostRequest::Conversations { .. },
            ..
        }]
    ));
}

#[test]
fn a_turn_streams_text_then_tools_then_settles() {
    let mut state = loaded(Vec::new());
    state.apply(RuntimeEvent::Started {
        request_id: Some(1),
        approval: String::new(),
        task: "fix it".into(),
        model: String::new(),
        display: None,
    });
    assert!(state.busy);
    for chunk in ["Look", "ing"] {
        state.apply(RuntimeEvent::TextDelta { text: chunk.into() });
    }
    state.apply(RuntimeEvent::ToolCall {
        id: "c1".into(),
        name: "read_file".into(),
        summary: "main.rs".into(),
        arguments: serde_json::Value::Null,
    });
    state.apply(RuntimeEvent::TextDelta {
        text: "Fixed".into(),
    });
    state.apply(RuntimeEvent::Finished {
        content: "Fixed.".into(),
    });
    let replies = state.apply(RuntimeEvent::TurnCompleted { request_id: 1 });
    assert_eq!(
        texts(&state),
        [
            (RowKind::User, "fix it"),
            (RowKind::Assistant, "Looking"),
            (RowKind::Tool, "read_file main.rs"),
            (RowKind::Assistant, "Fixed.")
        ]
    );
    assert!(!state.busy);
    assert_eq!(replies.len(), 1, "a finished turn refreshes the chat list");
}

#[test]
fn approvals_answer_oldest_first_and_blocked_calls_are_denied() {
    let mut state = loaded(Vec::new());
    for (id, blocked) in [(1, None), (2, Some("policy".to_string()))] {
        state.apply(RuntimeEvent::ApprovalRequest {
            id,
            call_id: format!("c{id}"),
            name: "shell".into(),
            summary: "rm".into(),
            arguments: serde_json::Value::Null,
            blocked,
            auto_allows: false,
            preview: None,
        });
    }
    assert!(matches!(
        state.answer(true),
        Some(RuntimeCommand::ApprovalReply {
            id: 1,
            outcome: CallOutcome::Approve
        })
    ));
    assert!(matches!(
        state.answer(true),
        Some(RuntimeCommand::ApprovalReply {
            id: 2,
            outcome: CallOutcome::Denied
        })
    ));
    assert!(state.answer(true).is_none());
}

#[test]
fn key_requests_are_answered_with_escape_immediately() {
    let mut state = loaded(Vec::new());
    let replies = state.apply(RuntimeEvent::KeyRequest { id: 9 });
    assert!(matches!(
        replies.as_slice(),
        [RuntimeCommand::KeyReply { id: 9, key }] if key.code == "Esc"
    ));
}

#[test]
fn only_the_awaited_conversation_list_is_applied() {
    let mut state = loaded(Vec::new());
    let RuntimeCommand::HostRequest { request_id, .. } = state.request_conversations() else {
        unreachable!()
    };
    let meta = ConversationMeta {
        id: 3,
        title: "t".into(),
        full_title: String::new(),
        updated_at: String::new(),
        updated_at_local: String::new(),
        message_count: 1,
        provider: "p".into(),
        model: "m".into(),
        token_count: 0,
        status: Default::default(),
    };
    state.apply(RuntimeEvent::HostResponse {
        request_id: request_id + 100,
        response: HostResponse::Conversations(vec![meta.clone()]),
    });
    assert!(state.conversations.is_empty(), "stale replies are ignored");
    state.apply(RuntimeEvent::HostResponse {
        request_id,
        response: HostResponse::Conversations(vec![meta]),
    });
    assert_eq!(state.conversations.len(), 1);
}

#[test]
fn a_lagged_stream_reloads_the_open_conversation() {
    let mut state = loaded(Vec::new());
    assert!(matches!(
        state
            .apply(RuntimeEvent::StreamLagged { skipped: 3 })
            .as_slice(),
        [RuntimeCommand::LoadConversation { id: 7, .. }]
    ));
}
