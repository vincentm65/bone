use super::*;
use crate::llm::ChatRole;
use crate::tools::builtin_tools;

#[test]
fn apply_outcome_persists_explicit_turn_messages_after_transcript_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let db = SessionDb::open(&path).unwrap();
    let conv = db.create_conversation("test", "model").unwrap();
    let mut session = RuntimeSession::new(ToolHandler::new(builtin_tools()));
    session.session_db = Some(db);
    session.conversation_id = Some(conv);
    session.transcript = (0..10)
        .map(|i| ChatMessage::new(ChatRole::User, format!("old {i}")))
        .collect();

    let current = ChatMessage::new(ChatRole::Assistant, "current answer");
    let outcome = DriverOutcome {
        result: Ok(crate::agent::AgentResponse {
            content: "current answer".into(),
            transcript: Vec::new(),
        }),
        tools: ToolHandler::new(builtin_tools()),
        // Simulate compaction replacing a ten-message transcript with a
        // shorter summary before this turn completed.
        transcript: vec![ChatMessage::new(ChatRole::User, "summary"), current.clone()],
        token_stats: Default::default(),
        persist_messages: vec![current],
        transcript_replaced: true,
        usage: Vec::new(),
    };

    let (result, persistence_error) = session.apply_outcome(outcome);
    result.unwrap();
    assert!(persistence_error.is_none());
    let stored = session
        .session_db
        .as_ref()
        .unwrap()
        .load_messages(conv)
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].content, "current answer");

    drop(session);
}

#[test]
fn apply_outcome_surfaces_persistence_failure_after_adopting_state() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let db = SessionDb::open(&path).unwrap();
    let mut session = RuntimeSession::new(ToolHandler::new(builtin_tools()));
    session.session_db = Some(db);
    session.conversation_id = Some(i64::MAX);

    let current = ChatMessage::new(ChatRole::Assistant, "in-memory answer");
    let outcome = DriverOutcome {
        result: Ok(crate::agent::AgentResponse {
            content: "in-memory answer".into(),
            transcript: Vec::new(),
        }),
        tools: ToolHandler::new(builtin_tools()),
        transcript: vec![current.clone()],
        token_stats: Default::default(),
        persist_messages: vec![current],
        transcript_replaced: false,
        usage: Vec::new(),
    };

    let (result, persistence_error) = session.apply_outcome(outcome);
    assert_eq!(result.unwrap().content, "in-memory answer");
    assert_eq!(session.transcript.len(), 1);
    assert_eq!(session.transcript[0].content, "in-memory answer");
    assert_eq!(session.session_seq, 0);
    assert!(persistence_error.is_some());

    drop(session);
}
