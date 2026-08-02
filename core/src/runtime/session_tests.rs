use super::*;
use crate::llm::ChatRole;
use crate::llm::provider::{LlmError, ResponseStream};
use crate::tools::builtin_tools;
use async_trait::async_trait;

struct TestProvider;

#[async_trait]
impl LlmProvider for TestProvider {
    fn id(&self) -> &str {
        "test"
    }
    fn name(&self) -> &str {
        "Test"
    }
    fn model(&self) -> &str {
        "test-model"
    }
    fn set_model(&mut self, _model: String) {}

    async fn chat_stream(
        &self,
        _messages: Vec<crate::llm::provider::ChatMessage>,
        _tools: Vec<crate::tools::ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        Ok(Box::pin(futures_util::stream::empty()))
    }
}

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

#[test]
fn incognito_on_detaches_persistence_and_off_persists_whole_transcript() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let db = SessionDb::open(&path).unwrap();
    let mut session = RuntimeSession::new(ToolHandler::new(builtin_tools()));
    let original = db.create_conversation("test", "model").unwrap();
    session.session_db = Some(db);
    session.conversation_id = Some(original);
    session.transcript = vec![
        ChatMessage::new(ChatRole::User, "secret prompt"),
        ChatMessage::new(ChatRole::Assistant, "classified answer"),
    ];
    let llm = TestProvider;

    // Toggle on: detaches from the durable conversation; every write no-ops.
    session.set_incognito(true, &llm).unwrap();
    assert!(session.incognito);
    assert_eq!(session.conversation_id, None);

    // A user message and a completed turn must persist nothing while on.
    session.append_user_to_db("still secret", None);
    let current = ChatMessage::new(ChatRole::Assistant, "in-memory answer");
    let outcome = DriverOutcome {
        result: Ok(crate::agent::AgentResponse {
            content: "in-memory answer".into(),
            transcript: Vec::new(),
        }),
        tools: ToolHandler::new(builtin_tools()),
        // Like a real turn, the driver's transcript carries the full history.
        transcript: vec![
            ChatMessage::new(ChatRole::User, "secret prompt"),
            ChatMessage::new(ChatRole::Assistant, "classified answer"),
            current.clone(),
        ],
        token_stats: Default::default(),
        persist_messages: vec![current],
        transcript_replaced: false,
        usage: Vec::new(),
    };
    let (_, persistence_error) = session.apply_outcome(outcome);
    assert!(persistence_error.is_none(), "incognito must never persist");
    let stored = session
        .session_db
        .as_ref()
        .unwrap()
        .load_messages(original)
        .unwrap();
    assert!(stored.is_empty(), "incognito chat must not touch the DB");

    // Toggle off: mints a fresh conversation containing the whole in-memory
    // transcript (the "still secret" append was dropped, as intended).
    session.set_incognito(false, &llm).unwrap();
    assert!(!session.incognito);
    let conv_id = session.conversation_id.expect("fresh conversation minted");
    let stored = session
        .session_db
        .as_ref()
        .unwrap()
        .load_messages(conv_id)
        .unwrap();
    let contents: Vec<&str> = stored.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        contents,
        vec!["secret prompt", "classified answer", "in-memory answer"]
    );
    assert_eq!(session.session_seq, stored.len() as i64);

    drop(session);
}

#[test]
fn incognito_off_with_empty_transcript_mints_an_empty_conversation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let db = SessionDb::open(&path).unwrap();
    let mut session = RuntimeSession::new(ToolHandler::new(builtin_tools()));
    session.session_db = Some(db);
    let llm = TestProvider;

    session.set_incognito(true, &llm).unwrap();
    assert!(session.incognito);
    assert_eq!(session.conversation_id, None);

    session.set_incognito(false, &llm).unwrap();
    assert!(!session.incognito);
    let conv_id = session.conversation_id.expect("fresh conversation minted");
    let stored = session
        .session_db
        .as_ref()
        .unwrap()
        .load_messages(conv_id)
        .unwrap();
    assert!(stored.is_empty());
    assert_eq!(session.session_seq, 0);

    drop(session);
}

#[test]
fn incognito_off_without_db_just_clears_the_flag() {
    let mut session = RuntimeSession::new(ToolHandler::new(builtin_tools()));
    let llm = TestProvider;
    session.set_incognito(true, &llm).unwrap();
    assert!(session.incognito);
    assert_eq!(session.conversation_id, None);
    session.set_incognito(false, &llm).unwrap();
    assert!(!session.incognito);
    assert_eq!(session.conversation_id, None);
}

#[test]
fn incognito_on_ends_the_active_conversation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let db = SessionDb::open(&path).unwrap();
    let conv = db.create_conversation("test", "model").unwrap();
    let mut session = RuntimeSession::new(ToolHandler::new(builtin_tools()));
    session.session_db = Some(db);
    session.conversation_id = Some(conv);
    let llm = TestProvider;

    session.set_incognito(true, &llm).unwrap();
    assert!(session.incognito);
    assert_eq!(session.conversation_id, None);
    // The conversation being left is ended like `/new` would: an empty
    // conversation is removed outright instead of lingering as "open".
    assert!(
        !session
            .session_db
            .as_ref()
            .unwrap()
            .conversation_exists(conv)
            .unwrap()
    );
}

#[test]
fn incognito_off_removes_new_conversation_when_persist_fails() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let db = SessionDb::open(&path).unwrap();
    let fault = rusqlite::Connection::open(&path).unwrap();
    fault
        .execute_batch(
            "CREATE TRIGGER fail_message_insert
             BEFORE INSERT ON messages
             BEGIN
                 SELECT RAISE(ABORT, 'forced message insert failure');
             END;",
        )
        .unwrap();
    let mut session = RuntimeSession::new(ToolHandler::new(builtin_tools()));
    session.session_db = Some(db);
    session.transcript = vec![ChatMessage::new(ChatRole::User, "secret prompt")];
    let llm = TestProvider;

    session.set_incognito(true, &llm).unwrap();
    let err = session.set_incognito(false, &llm).unwrap_err();

    assert!(err.contains("failed to persist incognito transcript"));
    assert!(session.incognito);
    assert_eq!(session.conversation_id, None);
    let conversation_count: i64 = fault
        .query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(conversation_count, 0);
}

#[test]
fn snapshot_carries_the_incognito_flag() {
    let mut session = RuntimeSession::new(ToolHandler::new(builtin_tools()));
    let llm = TestProvider;
    assert!(!session.snapshot("p", "m").incognito);
    session.set_incognito(true, &llm).unwrap();
    assert!(session.snapshot("p", "m").incognito);
    session.set_incognito(false, &llm).unwrap();
    assert!(!session.snapshot("p", "m").incognito);
}
