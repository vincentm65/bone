use super::*;
use crate::llm::ChatRole;
use crate::tools::builtin_tools;

#[test]
fn apply_outcome_persists_explicit_turn_messages_after_transcript_replacement() {
    let path = std::env::temp_dir().join(format!(
        "bone_compaction_persist_{}_{}.db",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = std::fs::remove_file(&path);
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

    session.apply_outcome(outcome).unwrap();
    let stored = session
        .session_db
        .as_ref()
        .unwrap()
        .load_messages(conv)
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].content, "current answer");

    drop(session);
    let _ = std::fs::remove_file(path);
}
