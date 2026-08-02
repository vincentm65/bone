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
fn main_session_builds_each_turn_from_current_configured_prompt() {
    fn build(session: &RuntimeSession, config: &crate::config::store::ConfigStore) -> Driver {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        session.build_driver(
            Arc::new(TestProvider),
            ExtensionManager::unloaded(),
            config.clone(),
            SharedApprovalMode::new(crate::tools::ApprovalMode::Safe),
            Arc::new(crate::tools::AutoApprovalGate),
            tx,
            KeyReplyRegistry::new(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(crate::session_sink::NullSessionSink),
        )
    }

    let _guard = crate::util::test_env_lock();
    let previous = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    let config = crate::config::store::ConfigStore::for_test();
    let session = RuntimeSession::new(ToolHandler::new(builtin_tools()));

    config
        .set_value(
            "general.system_prompt",
            serde_json::json!("First configured prompt"),
            config.snapshot().revision,
        )
        .unwrap();
    let first = build(&session, &config);
    let first_prompt = first.system_prompt_override.as_ref().unwrap();
    assert_eq!(&first.history[0].content, first_prompt);
    assert!(first_prompt.starts_with("First configured prompt\n\n"));
    assert!(first_prompt.contains(&dir.path().display().to_string()));

    config
        .set_value(
            "general.system_prompt",
            serde_json::json!("Changed for the next turn"),
            config.snapshot().revision,
        )
        .unwrap();
    let second = build(&session, &config);
    let second_prompt = second.system_prompt_override.as_ref().unwrap();
    assert_eq!(&second.history[0].content, second_prompt);
    assert!(second_prompt.starts_with("Changed for the next turn\n\n"));
    assert!(!second_prompt.contains("First configured prompt"));

    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
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

#[test]
fn transcript_replacement_persists_compacted_view_without_losing_history() {
    fn contents(messages: &[ChatMessage]) -> Vec<&str> {
        messages
            .iter()
            .map(|message| message.content.as_str())
            .collect()
    }

    let temp = tempfile::tempdir().unwrap();
    let db = SessionDb::open(&temp.path().join("sessions.db")).unwrap();
    let conversation = db.create_conversation("test", "model").unwrap();
    let prior = vec![
        ChatMessage::new(ChatRole::User, "first request"),
        ChatMessage::new(ChatRole::Assistant, "first answer"),
        ChatMessage::new(ChatRole::User, "follow-up"),
        ChatMessage::new(ChatRole::Assistant, "second answer"),
        ChatMessage::new(ChatRole::User, "final question"),
    ];
    let sequence = db
        .append_turn_with_checkpoint(conversation, 0, &prior, &[], None)
        .unwrap();

    let mut session = RuntimeSession::new(ToolHandler::new(builtin_tools()));
    session.session_db = Some(db);
    session.conversation_id = Some(conversation);
    session.session_seq = sequence;
    session.transcript = prior.clone();

    let current = ChatMessage::new(ChatRole::Assistant, "current answer");
    let compacted = vec![
        ChatMessage::new(ChatRole::User, "compacted summary of prior turns"),
        current.clone(),
    ];
    let (result, persistence_error) = session.apply_outcome(DriverOutcome {
        result: Ok(crate::agent::AgentResponse {
            content: "current answer".into(),
            transcript: Vec::new(),
        }),
        tools: ToolHandler::new(builtin_tools()),
        transcript: compacted,
        token_stats: Default::default(),
        persist_messages: vec![current],
        transcript_replaced: true,
        usage: Vec::new(),
    });
    result.unwrap();
    assert!(persistence_error.is_none(), "checkpoint write must succeed");

    let db = session.session_db.as_ref().unwrap();
    let effective = db.load_effective_transcript(conversation).unwrap();
    assert_eq!(
        contents(&session.transcript),
        ["compacted summary of prior turns", "current answer"]
    );
    assert_eq!(contents(&effective), contents(&session.transcript));
    assert_eq!(
        contents(&session.display_transcript()),
        [
            "first request",
            "first answer",
            "follow-up",
            "second answer",
            "final question",
            "current answer",
        ]
    );

    let checkpoint_count: i64 = db
        .conn_ref()
        .query_row(
            "SELECT COUNT(*) FROM conversation_context_checkpoints WHERE conversation_id = ?1",
            rusqlite::params![conversation],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(checkpoint_count, 1);
    assert_eq!(
        db.load_messages(conversation).unwrap().len(),
        prior.len() + 1
    );
}
