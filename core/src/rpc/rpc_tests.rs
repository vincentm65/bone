use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn publisher_does_not_keep_command_channel_open() {
    let (hub, mut commands_rx) = Hub::new();
    let publisher = hub.publisher();

    drop(hub);

    let received = tokio::time::timeout(std::time::Duration::from_secs(1), commands_rx.recv())
        .await
        .expect("command receiver stayed open");
    assert!(received.is_none());

    // The runtime-facing half remains usable without retaining a command
    // sender, even when there are no event subscribers.
    publisher.publish(RuntimeEvent::Status {
        message: "no listeners".into(),
    });
}

#[tokio::test]
async fn grouped_hubs_broadcast_global_events() {
    let group = HubGroup::default();
    let (hub_a, _commands_a) = Hub::new_grouped(group.clone());
    let (hub_b, _commands_b) = Hub::new_grouped(group);
    let mut events_a = hub_a.subscribe();
    let mut events_b = hub_b.subscribe();

    hub_a.publisher().publish_global(RuntimeEvent::Status {
        message: "global".into(),
    });

    assert!(
        matches!(events_a.recv().await.unwrap(), RuntimeEvent::Status { message } if message == "global")
    );
    assert!(
        matches!(events_b.recv().await.unwrap(), RuntimeEvent::Status { message } if message == "global")
    );
}

#[test]
fn grouped_hubs_do_not_retain_dropped_actor_channels() {
    let group = HubGroup::default();
    let (dropped, _commands) = Hub::new_grouped(group.clone());
    drop(dropped);
    assert_eq!(group.0.lock().unwrap().len(), 1);

    let (live, _commands) = Hub::new_grouped(group.clone());
    live.publisher().publish_global(RuntimeEvent::Status {
        message: "cleanup".into(),
    });

    let senders = group.0.lock().unwrap();
    assert_eq!(senders.len(), 1);
    assert!(senders[0].upgrade().is_some());
}

struct ConfigTestProvider;

#[async_trait::async_trait]
impl crate::llm::provider::LlmProvider for ConfigTestProvider {
    fn id(&self) -> &str {
        "mock"
    }

    fn name(&self) -> &str {
        "Mock"
    }

    fn model(&self) -> &str {
        "mock-1"
    }

    fn set_model(&mut self, _: String) {}

    async fn chat_stream(
        &self,
        _: Vec<crate::llm::ChatMessage>,
        _: Vec<crate::tools::ToolDefinition>,
    ) -> Result<crate::llm::ResponseStream, crate::llm::LlmError> {
        unreachable!()
    }
}

struct BlockingTestProvider {
    release: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl crate::llm::provider::LlmProvider for BlockingTestProvider {
    fn id(&self) -> &str {
        "blocking"
    }

    fn name(&self) -> &str {
        "Blocking"
    }

    fn model(&self) -> &str {
        "blocking-1"
    }

    fn set_model(&mut self, _: String) {}

    async fn chat_stream(
        &self,
        _: Vec<crate::llm::ChatMessage>,
        _: Vec<crate::tools::ToolDefinition>,
    ) -> Result<crate::llm::ResponseStream, crate::llm::LlmError> {
        self.release.notified().await;
        Ok(Box::pin(futures_util::stream::iter([Ok(
            crate::llm::ChatEvent::TextDelta("done".into()),
        )])))
    }
}

fn test_daemon_ctx(
    llm: Arc<dyn crate::llm::provider::LlmProvider>,
    extensions: crate::ext::ExtensionManager,
    session: crate::runtime::RuntimeSession,
) -> (
    DaemonCtx,
    Hub,
    tokio::sync::mpsc::UnboundedReceiver<RuntimeCommand>,
) {
    let submit_inbox = extensions.submit_inbox();
    let config = crate::config::store::ConfigStore::for_test();
    config.attach_extensions(extensions.clone());
    let (hub, commands) = Hub::new();
    (
        DaemonCtx {
            hub: hub.publisher(),
            llm,
            extensions,
            submit_inbox,
            session: Arc::new(Mutex::new(session)),
            mode: crate::tools::SharedApprovalMode::new(crate::tools::ApprovalMode::Safe),
            approval_registry: crate::runtime::ApprovalReplyRegistry::new(),
            key_registry: crate::runtime::KeyReplyRegistry::new(),
            pending_interactions: PendingInteractions::default(),
            pending_commands: std::collections::VecDeque::new(),
            reload_inbox: None,
            forward_view_diffs: false,
            config,
            processes_seen: None,
            jobs_seen: None,
        },
        hub,
        commands,
    )
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn synchronize_is_correlated_when_idle_and_during_a_turn() {
    let _guard = crate::util::test_env_lock();
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(BlockingTestProvider {
        release: release.clone(),
    });
    let extensions = crate::ext::ExtensionManager::unloaded();
    let session = crate::runtime::RuntimeSession::new(crate::tools::registry::ToolHandler::new(
        crate::tools::builtin_tools(),
    ));
    let (mut ctx, hub, mut commands) = test_daemon_ctx(provider, extensions, session);
    let mut events = hub.subscribe();

    let flow = ctx
        .handle_idle_command(
            RuntimeCommand::Synchronize {
                request_id: 41,
                include_messages: false,
            },
            &mut commands,
        )
        .await;
    assert!(matches!(flow, Flow::Continue));
    assert!(matches!(
        events.recv().await.unwrap(),
        RuntimeEvent::StateSynchronized {
            request_id: 41,
            busy: false,
            messages: None,
            ..
        }
    ));

    let Flow::StartTurn {
        request_id,
        text,
        display,
    } = ctx
        .handle_idle_command(
            RuntimeCommand::SubmitPrompt {
                request_id: None,
                text: "repair this state".into(),
                images: vec![],
            },
            &mut commands,
        )
        .await
    else {
        panic!("prompt did not start a turn");
    };

    let command_tx = hub.command_sender();
    let observer = tokio::spawn(async move {
        while !matches!(events.recv().await.unwrap(), RuntimeEvent::Started { .. }) {}
        command_tx
            .send(RuntimeCommand::Synchronize {
                request_id: 42,
                include_messages: true,
            })
            .unwrap();
        loop {
            if let RuntimeEvent::StateSynchronized {
                request_id,
                busy,
                snapshot,
                messages,
            } = events.recv().await.unwrap()
            {
                break (request_id, busy, snapshot, messages);
            }
        }
    });

    let turn = ctx.run_turn(request_id, text, display, &mut commands);
    let synchronized = async {
        let result = observer.await.unwrap();
        release.notify_one();
        result
    };
    let (_, (request_id, busy, snapshot, messages)) = tokio::join!(turn, synchronized);

    assert_eq!(request_id, 42);
    assert!(busy);
    assert_eq!(snapshot.transcript_len, 1);
    let messages = messages.expect("requested transcript was omitted");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "repair this state");
}

fn interactive_test_extensions() -> crate::ext::ExtensionManager {
    let lua = mlua::Lua::new();
    let bone = lua.create_table().unwrap();
    lua.globals().set("bone", bone.clone()).unwrap();
    crate::ext::ops_commands::setup_register_command(&lua, &bone).unwrap();
    lua.load(
        r#"
        bone.command.register("wait_for_key", function(_, ctx)
            ctx.ui.key()
            return { display = "done", submit = false }
        end)

        bone.command.register("wait_for_approval", function(_, ctx)
            local result = ctx.shell("printf approved")
            return { display = result.stdout, submit = false }
        end)

        bone.command.register("submit_output", function()
            return { display = "follow up", submit = true }
        end)
        "#,
    )
    .exec()
    .unwrap();

    crate::ext::types::ExtensionManager::from_arc(
        Arc::new(Mutex::new(lua)),
        true,
        true,
        vec![
            crate::ext::ops_commands::RegisteredLuaCommand {
                name: "wait_for_key".into(),
                description: String::new(),
            },
            crate::ext::ops_commands::RegisteredLuaCommand {
                name: "wait_for_approval".into(),
                description: String::new(),
            },
            crate::ext::ops_commands::RegisteredLuaCommand {
                name: "submit_output".into(),
                description: String::new(),
            },
        ],
        Arc::new(Mutex::new(crate::config::settings::Settings::defaults())),
        Arc::new(std::sync::RwLock::new(Default::default())),
        crate::ext::api_ui::new_shared(),
    )
}

fn private_command_extensions() -> crate::ext::ExtensionManager {
    let lua = mlua::Lua::new();
    let bone = lua.create_table().unwrap();
    lua.globals().set("bone", bone.clone()).unwrap();
    crate::ext::ops_commands::setup_register_command(&lua, &bone).unwrap();
    lua.load(
        r#"
        bone.command.register("private_replace", function(_, ctx)
            local result = ctx.llm.complete({
                messages = {
                    { role = "system", content = "summarize privately" },
                    { role = "user", content = "full transcript" },
                },
                tools = {},
                max_tokens = 4000,
            })
            if not result.ok then
                return { display = result.error, submit = false }
            end
            return {
                action = "conversation.replace",
                messages = {
                    { role = "user", content = result.content },
                },
            }
        end)
        "#,
    )
    .exec()
    .unwrap();

    crate::ext::types::ExtensionManager::from_arc(
        Arc::new(Mutex::new(lua)),
        true,
        true,
        vec![crate::ext::ops_commands::RegisteredLuaCommand {
            name: "private_replace".into(),
            description: String::new(),
        }],
        Arc::new(Mutex::new(crate::config::settings::Settings::defaults())),
        Arc::new(std::sync::RwLock::new(Default::default())),
        crate::ext::api_ui::new_shared(),
    )
}

#[derive(Debug)]
struct CapturedPrivateCommandRequest {
    messages: Vec<crate::llm::ChatMessage>,
    tools: Vec<crate::tools::ToolDefinition>,
    context: crate::llm::provider::ProviderRequestContext,
}

struct PrivateCommandProvider {
    requests: Arc<Mutex<Vec<CapturedPrivateCommandRequest>>>,
}

struct CancellingPrivateCommandProvider {
    waiting_after_usage: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl crate::llm::provider::LlmProvider for CancellingPrivateCommandProvider {
    fn id(&self) -> &str {
        "private-command-cancel"
    }

    fn name(&self) -> &str {
        "Private command cancellation"
    }

    fn model(&self) -> &str {
        "private-command-cancel-model"
    }

    fn set_model(&mut self, _: String) {}

    async fn chat_stream(
        &self,
        _: Vec<crate::llm::ChatMessage>,
        _: Vec<crate::tools::ToolDefinition>,
    ) -> Result<crate::llm::ResponseStream, crate::llm::LlmError> {
        unreachable!("private command must use request context")
    }

    async fn chat_stream_with_context(
        &self,
        _: Vec<crate::llm::ChatMessage>,
        _: Vec<crate::tools::ToolDefinition>,
        _: crate::llm::provider::ProviderRequestContext,
    ) -> Result<crate::llm::ResponseStream, crate::llm::LlmError> {
        let waiting_after_usage = Arc::clone(&self.waiting_after_usage);
        Ok(Box::pin(futures_util::stream::unfold(0, move |state| {
            let waiting_after_usage = Arc::clone(&waiting_after_usage);
            async move {
                if state == 0 {
                    Some((
                        Ok(crate::llm::ChatEvent::TokenUsage {
                            prompt_tokens: 7,
                            completion_tokens: 3,
                            cached_tokens: Some(2),
                            cost: Some(0.5),
                        }),
                        1,
                    ))
                } else {
                    waiting_after_usage.notify_one();
                    std::future::pending().await
                }
            }
        })))
    }
}

#[async_trait::async_trait]
impl crate::llm::provider::LlmProvider for PrivateCommandProvider {
    fn id(&self) -> &str {
        "private-command"
    }

    fn name(&self) -> &str {
        "Private command"
    }

    fn model(&self) -> &str {
        "private-command-model"
    }

    fn set_model(&mut self, _: String) {}

    async fn chat_stream(
        &self,
        _: Vec<crate::llm::ChatMessage>,
        _: Vec<crate::tools::ToolDefinition>,
    ) -> Result<crate::llm::ResponseStream, crate::llm::LlmError> {
        unreachable!("private command must use request context")
    }

    async fn chat_stream_with_context(
        &self,
        messages: Vec<crate::llm::ChatMessage>,
        tools: Vec<crate::tools::ToolDefinition>,
        context: crate::llm::provider::ProviderRequestContext,
    ) -> Result<crate::llm::ResponseStream, crate::llm::LlmError> {
        self.requests
            .lock()
            .unwrap()
            .push(CapturedPrivateCommandRequest {
                messages,
                tools,
                context,
            });
        Ok(Box::pin(futures_util::stream::iter([
            Ok(crate::llm::ChatEvent::TextDelta("safe checkpoint".into())),
            Ok(crate::llm::ChatEvent::TokenUsage {
                prompt_tokens: 9,
                completion_tokens: 2,
                cached_tokens: Some(1),
                cost: Some(0.25),
            }),
        ])))
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_command_private_completion_returns_replace_and_accounts_usage() {
    let _guard = crate::util::test_env_lock();
    let extensions = private_command_extensions();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(PrivateCommandProvider {
        requests: Arc::clone(&requests),
    });
    let temp = tempfile::tempdir().unwrap();
    let db = crate::session_db::SessionDb::open(&temp.path().join("sessions.db")).unwrap();
    let conversation_id = db
        .create_conversation("private-command", "private-command-model")
        .unwrap();
    let mut session = crate::runtime::RuntimeSession::new(
        crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
    );
    session.session_db = Some(db);
    session.conversation_id = Some(conversation_id);
    session.transcript.push(crate::llm::ChatMessage::new(
        crate::llm::ChatRole::User,
        "original history",
    ));
    let (mut ctx, hub, mut commands) = test_daemon_ctx(provider, extensions, session);
    let mut events = hub.subscribe();

    let (ret, operations) = ctx
        .run_interactive_command(&mut commands, "private_replace".into(), String::new())
        .await
        .expect("registered command was not found");
    assert!(operations.is_empty());
    let replacement = ret
        .expect("command returned no action")
        .action
        .and_then(|action| action.conversation_replace)
        .expect("conversation.replace was not returned");
    assert_eq!(replacement.len(), 1);
    assert_eq!(replacement[0].content, "safe checkpoint");

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "completion must make one provider request"
    );
    assert_eq!(requests[0].messages.len(), 2);
    assert_eq!(requests[0].messages[0].content, "summarize privately");
    assert_eq!(requests[0].messages[1].content, "full transcript");
    assert!(requests[0].tools.is_empty());
    assert_eq!(requests[0].context.conversation_id, Some(conversation_id));
    assert!(requests[0].context.turn_state.is_some());
    assert_eq!(requests[0].context.max_tokens, Some(4000));
    drop(requests);

    let session = ctx.session.lock().unwrap();
    assert_eq!(session.transcript[0].content, "original history");
    assert_eq!(session.token_stats.sent, 9);
    assert_eq!(session.token_stats.received, 2);
    assert_eq!(session.token_stats.cached, 1);
    assert_eq!(session.token_stats.cost, 0.25);
    assert_eq!(session.token_stats.request_count, 1);
    let persisted = session
        .session_db
        .as_ref()
        .unwrap()
        .conversation_usage(conversation_id)
        .unwrap();
    assert_eq!(persisted.prompt_tokens, 9);
    assert_eq!(persisted.completion_tokens, 2);
    assert_eq!(persisted.cached_tokens, 1);
    assert_eq!(persisted.cost, 0.25);
    assert_eq!(persisted.request_count, 1);
    drop(session);

    assert!(matches!(
        ctx.handle_idle_command(
            RuntimeCommand::ReplaceConversation {
                messages: replacement.clone(),
            },
            &mut commands,
        )
        .await,
        Flow::Continue
    ));
    let session = ctx.session.lock().unwrap();
    assert_eq!(session.transcript, replacement);
    let effective = session
        .session_db
        .as_ref()
        .unwrap()
        .load_effective_transcript(conversation_id)
        .unwrap();
    assert_eq!(effective, replacement);
    drop(session);

    let usage = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| matches!(event, RuntimeEvent::TokenUsage { .. }))
        .expect("private completion did not publish token usage");
    assert!(matches!(
        usage,
        RuntimeEvent::TokenUsage {
            sent: 9,
            received: 2,
            context_length: 9,
        }
    ));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_command_cancellation_drains_private_usage_with_bounded_grace() {
    let _guard = crate::util::test_env_lock();
    let extensions = private_command_extensions();
    let waiting_after_usage = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(CancellingPrivateCommandProvider {
        waiting_after_usage: Arc::clone(&waiting_after_usage),
    });
    let temp = tempfile::tempdir().unwrap();
    let db = crate::session_db::SessionDb::open(&temp.path().join("sessions.db")).unwrap();
    let conversation_id = db
        .create_conversation("private-command-cancel", "private-command-cancel-model")
        .unwrap();
    let mut session = crate::runtime::RuntimeSession::new(
        crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
    );
    session.session_db = Some(db);
    session.conversation_id = Some(conversation_id);
    let (mut ctx, hub, mut commands) = test_daemon_ctx(provider, extensions, session);
    let mut events = hub.subscribe();
    let command_tx = hub.command_sender();

    let cancel = tokio::spawn(async move {
        waiting_after_usage.notified().await;
        command_tx.send(RuntimeCommand::Cancel).unwrap();
    });
    let started = std::time::Instant::now();
    let (ret, operations) = ctx
        .run_interactive_command(&mut commands, "private_replace".into(), String::new())
        .await
        .expect("registered command was not found");
    cancel.await.unwrap();

    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    assert!(ret.is_none());
    assert!(operations.is_empty());
    let session = ctx.session.lock().unwrap();
    assert_eq!(session.token_stats.sent, 7);
    assert_eq!(session.token_stats.received, 3);
    assert_eq!(session.token_stats.cached, 2);
    assert_eq!(session.token_stats.cost, 0.5);
    assert_eq!(session.token_stats.request_count, 1);
    let persisted = session
        .session_db
        .as_ref()
        .unwrap()
        .conversation_usage(conversation_id)
        .unwrap();
    assert_eq!(persisted.prompt_tokens, 7);
    assert_eq!(persisted.completion_tokens, 3);
    assert_eq!(persisted.cached_tokens, 2);
    assert_eq!(persisted.cost, 0.5);
    assert_eq!(persisted.request_count, 1);
    drop(session);

    let mut saw_usage = false;
    while let Ok(event) = events.try_recv() {
        if matches!(
            event,
            RuntimeEvent::TokenUsage {
                sent: 7,
                received: 3,
                context_length: 7,
            }
        ) {
            saw_usage = true;
        }
    }
    assert!(saw_usage);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn synchronize_reports_busy_during_interactive_command() {
    let _guard = crate::util::test_env_lock();
    let extensions = interactive_test_extensions();
    let session = crate::runtime::RuntimeSession::new(crate::tools::registry::ToolHandler::new(
        crate::tools::builtin_tools(),
    ));
    let (mut ctx, hub, mut commands) =
        test_daemon_ctx(Arc::new(ConfigTestProvider), extensions, session);
    let mut events = hub.subscribe();
    let command_tx = hub.command_sender();

    let observer = tokio::spawn(async move {
        let key_id = loop {
            if let RuntimeEvent::KeyRequest { id } = events.recv().await.unwrap() {
                break id;
            }
        };
        command_tx
            .send(RuntimeCommand::Synchronize {
                request_id: 73,
                include_messages: false,
            })
            .unwrap();
        let synchronized = loop {
            if let event @ RuntimeEvent::StateSynchronized { .. } = events.recv().await.unwrap() {
                break event;
            }
        };
        command_tx
            .send(RuntimeCommand::KeyReply {
                id: key_id,
                key: bone_protocol::KeyEvent {
                    code: "Enter".into(),
                    char: None,
                    ctrl: false,
                    alt: false,
                    shift: false,
                },
            })
            .unwrap();
        synchronized
    });

    let result = ctx
        .run_interactive_command(&mut commands, "wait_for_key".into(), String::new())
        .await;
    assert!(result.is_some());
    assert!(matches!(
        observer.await.unwrap(),
        RuntimeEvent::StateSynchronized {
            request_id: 73,
            busy: true,
            messages: None,
            ..
        }
    ));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn synchronize_replays_and_routes_pending_command_approval() {
    let _guard = crate::util::test_env_lock();
    let extensions = interactive_test_extensions();
    let session = crate::runtime::RuntimeSession::new(crate::tools::registry::ToolHandler::new(
        crate::tools::builtin_tools(),
    ));
    let (mut ctx, hub, mut commands) =
        test_daemon_ctx(Arc::new(ConfigTestProvider), extensions, session);
    let mut events = hub.subscribe();
    let command_tx = hub.command_sender();

    let observer = tokio::spawn(async move {
        let approval_id = loop {
            if let RuntimeEvent::ApprovalRequest { id, .. } = events.recv().await.unwrap() {
                break id;
            }
        };
        command_tx
            .send(RuntimeCommand::Synchronize {
                request_id: 74,
                include_messages: false,
            })
            .unwrap();

        let mut replayed = false;
        loop {
            match events.recv().await.unwrap() {
                RuntimeEvent::ApprovalRequest { id, .. } if id == approval_id => replayed = true,
                RuntimeEvent::StateSynchronized {
                    request_id: 74,
                    busy,
                    ..
                } => {
                    assert!(busy);
                    break;
                }
                _ => {}
            }
        }
        assert!(replayed, "synchronize did not replay the pending approval");

        command_tx
            .send(RuntimeCommand::ApprovalReply {
                id: approval_id,
                outcome: bone_protocol::CallOutcome::Approve,
            })
            .unwrap();
    });

    let (ret, operations) = ctx
        .run_interactive_command(&mut commands, "wait_for_approval".into(), String::new())
        .await
        .expect("registered command was not found");
    observer.await.unwrap();

    assert!(operations.is_empty());
    assert_eq!(ret.expect("command returned no output").output, "approved");
    assert_eq!(ctx.approval_registry.pending_count(), 0);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn correlated_prompt_marks_started_and_completion() {
    let _guard = crate::util::test_env_lock();
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(BlockingTestProvider {
        release: release.clone(),
    });
    let extensions = crate::ext::ExtensionManager::unloaded();
    let session = crate::runtime::RuntimeSession::new(crate::tools::registry::ToolHandler::new(
        crate::tools::builtin_tools(),
    ));
    let (mut ctx, hub, mut commands) = test_daemon_ctx(provider, extensions, session);
    let mut events = hub.subscribe();

    let Flow::StartTurn {
        request_id,
        text,
        display,
    } = ctx
        .handle_idle_command(
            RuntimeCommand::SubmitPrompt {
                request_id: Some(91),
                text: "correlate this".into(),
                images: vec![],
            },
            &mut commands,
        )
        .await
    else {
        panic!("prompt did not start a turn");
    };
    release.notify_one();
    ctx.run_turn(request_id, text, display, &mut commands).await;

    let mut started = false;
    let mut completed = false;
    let mut legacy_complete = false;
    while let Ok(event) = events.try_recv() {
        match event {
            RuntimeEvent::Started {
                request_id: Some(91),
                ..
            } => started = true,
            RuntimeEvent::TurnCompleted { request_id: 91 } => completed = true,
            RuntimeEvent::TurnComplete => legacy_complete = true,
            _ => {}
        }
    }
    assert!(started);
    assert!(completed);
    assert!(!legacy_complete);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn command_and_keymap_replies_echo_request_ids() {
    let _guard = crate::util::test_env_lock();
    let extensions = interactive_test_extensions();
    let session = crate::runtime::RuntimeSession::new(crate::tools::registry::ToolHandler::new(
        crate::tools::builtin_tools(),
    ));
    let (mut ctx, hub, mut commands) =
        test_daemon_ctx(Arc::new(ConfigTestProvider), extensions, session);
    let mut events = hub.subscribe();

    let flow = ctx
        .handle_idle_command(
            RuntimeCommand::RunCommand {
                request_id: Some(92),
                name: "submit_output".into(),
                input: String::new(),
            },
            &mut commands,
        )
        .await;
    assert!(matches!(
        flow,
        Flow::StartTurn {
            request_id: Some(92),
            ..
        }
    ));
    loop {
        if matches!(
            events.recv().await.unwrap(),
            RuntimeEvent::CommandComplete {
                request_id: Some(92),
                submit: true,
                ..
            }
        ) {
            break;
        }
    }

    let flow = ctx
        .handle_idle_command(
            RuntimeCommand::KeymapDispatch {
                request_id: Some(93),
                action: "toggle_panes".into(),
            },
            &mut commands,
        )
        .await;
    assert!(matches!(flow, Flow::Continue));
    loop {
        if matches!(
            events.recv().await.unwrap(),
            RuntimeEvent::KeymapDispatched {
                request_id: Some(93),
                ..
            }
        ) {
            break;
        }
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn turn_queues_idle_commands_and_preserves_correlated_replies() {
    let _guard = crate::util::test_env_lock();
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(BlockingTestProvider {
        release: release.clone(),
    });
    let extensions = crate::ext::ExtensionManager::unloaded();
    let session = crate::runtime::RuntimeSession::new(crate::tools::registry::ToolHandler::new(
        crate::tools::builtin_tools(),
    ));
    let (mut ctx, hub, mut commands) = test_daemon_ctx(provider, extensions, session);
    let mut observer_events = hub.subscribe();
    let command_tx = hub.command_sender();

    let Flow::StartTurn {
        request_id,
        text,
        display,
    } = ctx
        .handle_idle_command(
            RuntimeCommand::SubmitPrompt {
                request_id: Some(201),
                text: "keep the runtime busy".into(),
                images: vec![],
            },
            &mut commands,
        )
        .await
    else {
        panic!("prompt did not start a turn");
    };

    let observer = tokio::spawn(async move {
        while !matches!(
            observer_events.recv().await.unwrap(),
            RuntimeEvent::Started { .. }
        ) {}
        command_tx
            .send(RuntimeCommand::RunCommand {
                request_id: Some(202),
                name: "missing".into(),
                input: String::new(),
            })
            .unwrap();
        command_tx
            .send(RuntimeCommand::KeymapDispatch {
                request_id: Some(203),
                action: "toggle_panes".into(),
            })
            .unwrap();
        // This response proves the two preceding FIFO commands were consumed
        // by the busy turn and placed in DaemonCtx's idle queue.
        command_tx
            .send(RuntimeCommand::Synchronize {
                request_id: 204,
                include_messages: false,
            })
            .unwrap();
        loop {
            if matches!(
                observer_events.recv().await.unwrap(),
                RuntimeEvent::StateSynchronized {
                    request_id: 204,
                    busy: true,
                    ..
                }
            ) {
                break;
            }
        }
        release.notify_one();
    });

    ctx.run_turn(request_id, text, display, &mut commands).await;
    observer.await.unwrap();
    assert_eq!(ctx.pending_commands.len(), 2);

    let mut replies = hub.subscribe();
    let command = ctx.pending_commands.pop_front().unwrap();
    assert!(matches!(
        &command,
        RuntimeCommand::RunCommand {
            request_id: Some(202),
            ..
        }
    ));
    assert!(matches!(
        ctx.handle_idle_command(command, &mut commands).await,
        Flow::Continue
    ));
    loop {
        if matches!(
            replies.recv().await.unwrap(),
            RuntimeEvent::CommandComplete {
                request_id: Some(202),
                ..
            }
        ) {
            break;
        }
    }

    let command = ctx.pending_commands.pop_front().unwrap();
    assert!(matches!(
        &command,
        RuntimeCommand::KeymapDispatch {
            request_id: Some(203),
            ..
        }
    ));
    assert!(matches!(
        ctx.handle_idle_command(command, &mut commands).await,
        Flow::Continue
    ));
    loop {
        if matches!(
            replies.recv().await.unwrap(),
            RuntimeEvent::KeymapDispatched {
                request_id: Some(203),
                ..
            }
        ) {
            break;
        }
    }
    assert!(ctx.pending_commands.is_empty());
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_command_queues_idle_work_in_fifo_order() {
    let _guard = crate::util::test_env_lock();
    let extensions = interactive_test_extensions();
    let session = crate::runtime::RuntimeSession::new(crate::tools::registry::ToolHandler::new(
        crate::tools::builtin_tools(),
    ));
    let (mut ctx, hub, mut commands) =
        test_daemon_ctx(Arc::new(ConfigTestProvider), extensions, session);
    let mut observer_events = hub.subscribe();
    let command_tx = hub.command_sender();

    let observer = tokio::spawn(async move {
        let key_id = loop {
            if let RuntimeEvent::KeyRequest { id } = observer_events.recv().await.unwrap() {
                break id;
            }
        };
        command_tx
            .send(RuntimeCommand::RunCommand {
                request_id: Some(301),
                name: "missing".into(),
                input: String::new(),
            })
            .unwrap();
        command_tx
            .send(RuntimeCommand::KeymapDispatch {
                request_id: Some(302),
                action: "toggle_panes".into(),
            })
            .unwrap();
        command_tx.send(RuntimeCommand::NewConversation).unwrap();
        command_tx
            .send(RuntimeCommand::Synchronize {
                request_id: 303,
                include_messages: false,
            })
            .unwrap();
        loop {
            if matches!(
                observer_events.recv().await.unwrap(),
                RuntimeEvent::StateSynchronized {
                    request_id: 303,
                    busy: true,
                    ..
                }
            ) {
                break;
            }
        }
        command_tx
            .send(RuntimeCommand::KeyReply {
                id: key_id,
                key: bone_protocol::KeyEvent {
                    code: "Enter".into(),
                    char: None,
                    ctrl: false,
                    alt: false,
                    shift: false,
                },
            })
            .unwrap();
    });

    let result = ctx
        .run_interactive_command(&mut commands, "wait_for_key".into(), String::new())
        .await;
    assert!(result.is_some());
    observer.await.unwrap();
    assert_eq!(ctx.pending_commands.len(), 3);

    let mut replies = hub.subscribe();
    let command = ctx.pending_commands.pop_front().unwrap();
    assert!(matches!(
        &command,
        RuntimeCommand::RunCommand {
            request_id: Some(301),
            ..
        }
    ));
    assert!(matches!(
        ctx.handle_idle_command(command, &mut commands).await,
        Flow::Continue
    ));
    loop {
        if matches!(
            replies.recv().await.unwrap(),
            RuntimeEvent::CommandComplete {
                request_id: Some(301),
                ..
            }
        ) {
            break;
        }
    }

    let command = ctx.pending_commands.pop_front().unwrap();
    assert!(matches!(
        &command,
        RuntimeCommand::KeymapDispatch {
            request_id: Some(302),
            ..
        }
    ));
    assert!(matches!(
        ctx.handle_idle_command(command, &mut commands).await,
        Flow::Continue
    ));
    loop {
        if matches!(
            replies.recv().await.unwrap(),
            RuntimeEvent::KeymapDispatched {
                request_id: Some(302),
                ..
            }
        ) {
            break;
        }
    }

    let command = ctx.pending_commands.pop_front().unwrap();
    assert!(matches!(&command, RuntimeCommand::NewConversation));
    assert!(matches!(
        ctx.handle_idle_command(command, &mut commands).await,
        Flow::Continue
    ));
    assert!(ctx.pending_commands.is_empty());
    assert!(matches!(
        ctx.handle_idle_command(
            RuntimeCommand::Synchronize {
                request_id: 304,
                include_messages: false,
            },
            &mut commands,
        )
        .await,
        Flow::Continue
    ));
    loop {
        if matches!(
            replies.recv().await.unwrap(),
            RuntimeEvent::StateSynchronized {
                request_id: 304,
                busy: false,
                snapshot: bone_protocol::SessionSnapshot {
                    transcript_len: 0,
                    ..
                },
                ..
            }
        ) {
            break;
        }
    }
}

#[test]
fn daemon_actors_only_consume_their_own_submitted_prompts() {
    let _guard = crate::util::test_env_lock();

    fn actor(extensions: crate::ext::ExtensionManager, conversation_id: i64) -> DaemonCtx {
        let submit_inbox = extensions.submit_inbox();
        let config = crate::config::store::ConfigStore::for_test();
        config.attach_extensions(extensions.clone());
        let (hub, _commands) = Hub::new();
        let mut session = crate::runtime::RuntimeSession::new(
            crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
        );
        session.conversation_id = Some(conversation_id);
        DaemonCtx {
            hub: hub.publisher(),
            llm: Arc::new(ConfigTestProvider),
            extensions,
            submit_inbox,
            session: Arc::new(Mutex::new(session)),
            mode: crate::tools::SharedApprovalMode::new(crate::tools::ApprovalMode::Safe),
            approval_registry: crate::runtime::ApprovalReplyRegistry::new(),
            key_registry: crate::runtime::KeyReplyRegistry::new(),
            pending_interactions: PendingInteractions::default(),
            pending_commands: std::collections::VecDeque::new(),
            reload_inbox: None,
            forward_view_diffs: false,
            config,
            processes_seen: None,
            jobs_seen: None,
        }
    }

    let extensions_a = crate::ext::ExtensionManager::unloaded();
    let extensions_b = crate::ext::ExtensionManager::unloaded();
    let inbox_a = extensions_a.submit_inbox();
    let inbox_b = extensions_b.submit_inbox();
    let actor_a = actor(extensions_a, -9_001);
    let actor_b = actor(extensions_b, -9_002);

    inbox_a.push("a-1".into());
    inbox_b.push("b-1".into());
    inbox_a.push("a-2".into());
    inbox_b.push("b-2".into());

    // Poll B first to make the cross-runtime failure deterministic: the old
    // process-global queue would have handed it A's first prompt.
    assert_eq!(actor_b.next_background_prompt(), Some(("b-1".into(), None)));
    assert_eq!(actor_b.next_background_prompt(), Some(("b-2".into(), None)));
    assert_eq!(actor_a.next_background_prompt(), Some(("a-1".into(), None)));
    assert_eq!(actor_a.next_background_prompt(), Some(("a-2".into(), None)));
    assert!(inbox_a.pop().is_none());
    assert!(inbox_b.pop().is_none());
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn invalid_provider_mutations_leave_config_and_runtime_unchanged() {
    let _guard = crate::util::test_env_lock();
    let old_bone = std::env::var_os("BONE_DIR");
    let dir = std::env::temp_dir().join(format!(
        "bone-provider-preflight-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    unsafe { std::env::set_var("BONE_DIR", &dir) };

    let provider = |handler: &str| crate::config::ProviderEntry {
        label: "Mock".into(),
        base_url: "http://localhost".into(),
        model: "configured-model".into(),
        api_key: Default::default(),
        endpoint: "/chat/completions".into(),
        handler: handler.into(),
        context_window_tokens: None,
        max_concurrency: None,
        reasoning_effort: String::new(),
        fast_mode: false,
        supports_prompt_cache_key: false,
    };
    let mut providers = crate::config::ProvidersConfig::default();
    providers
        .providers
        .insert("mock".into(), provider("openai"));
    providers
        .providers
        .insert("bad".into(), provider("unsupported"));
    providers.last_provider = "mock".into();
    crate::config::domains::persist_providers(&providers).unwrap();

    let extensions = crate::ext::ExtensionManager::unloaded();
    let config = crate::config::store::ConfigStore::new(extensions.clone()).unwrap();
    let revision = config.snapshot().revision;
    let (hub, commands_rx) = Hub::new();
    let mut events = hub.subscribe();
    let commands = hub.command_sender();
    let daemon = tokio::spawn(run_daemon(
        hub.publisher(),
        commands_rx,
        Arc::new(ConfigTestProvider),
        extensions,
        config.clone(),
        Arc::new(Mutex::new(crate::runtime::RuntimeSession::new(
            crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
        ))),
        crate::tools::ApprovalMode::Safe,
        None,
        false,
    ));

    commands
        .send(RuntimeCommand::UpsertProvider {
            provider: bone_protocol::ProviderUpdate {
                id: "mock".into(),
                label: "Changed".into(),
                base_url: "http://changed".into(),
                model: "changed-model".into(),
                api_key: None,
                endpoint: "/chat/completions".into(),
                handler: "unsupported".into(),
                context_window_tokens: None,
                max_concurrency: None,
                reasoning_effort: String::new(),
                fast_mode: None,
                supports_prompt_cache_key: None,
            },
            expected_revision: revision,
            request_id: Some("upsert".into()),
        })
        .unwrap();

    let mut upsert_rejected = false;
    let mut upsert_runtime_unchanged = false;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !(upsert_rejected && upsert_runtime_unchanged) {
            match events.recv().await.unwrap() {
                RuntimeEvent::ConfigMutationRejected { request_id, .. }
                    if request_id.as_deref() == Some("upsert") =>
                {
                    upsert_rejected = true;
                }
                RuntimeEvent::StateSnapshot { snapshot } => {
                    upsert_runtime_unchanged =
                        snapshot.provider_id == "mock" && snapshot.provider_model == "mock-1";
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let after_upsert = config.snapshot();
    assert_eq!(after_upsert.revision, revision);
    assert_eq!(
        config.providers_config().providers["mock"].model,
        "configured-model"
    );

    commands
        .send(RuntimeCommand::SetActiveProvider {
            id: "bad".into(),
            expected_revision: revision,
            request_id: Some("activate".into()),
        })
        .unwrap();
    let mut activation_rejected = false;
    let mut activation_runtime_unchanged = false;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !(activation_rejected && activation_runtime_unchanged) {
            match events.recv().await.unwrap() {
                RuntimeEvent::ConfigMutationRejected { request_id, .. }
                    if request_id.as_deref() == Some("activate") =>
                {
                    activation_rejected = true;
                }
                RuntimeEvent::StateSnapshot { snapshot } => {
                    activation_runtime_unchanged =
                        snapshot.provider_id == "mock" && snapshot.provider_model == "mock-1";
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(config.snapshot().revision, revision);
    assert_eq!(config.snapshot().active_provider, "mock");

    daemon.abort();
    std::fs::remove_dir_all(dir).ok();
    unsafe {
        match old_bone {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn resetting_approval_updates_live_mode() {
    let _guard = crate::util::test_env_lock();
    let old_bone = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    let extensions = crate::ext::ExtensionManager::unloaded();
    let submit_inbox = extensions.submit_inbox();
    let config = crate::config::store::ConfigStore::new(extensions.clone()).unwrap();
    config
        .set_value(
            "general.approval",
            serde_json::json!("danger"),
            config.snapshot().revision,
        )
        .unwrap();
    let revision = config.snapshot().revision;
    let (hub, mut commands) = Hub::new();
    let mode = crate::tools::SharedApprovalMode::new(crate::tools::ApprovalMode::Danger);
    let mut ctx = DaemonCtx {
        hub: hub.publisher(),
        llm: Arc::new(ConfigTestProvider),
        extensions,
        submit_inbox,
        session: Arc::new(Mutex::new(crate::runtime::RuntimeSession::new(
            crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
        ))),
        mode: mode.clone(),
        approval_registry: crate::runtime::ApprovalReplyRegistry::new(),
        key_registry: crate::runtime::KeyReplyRegistry::new(),
        pending_interactions: PendingInteractions::default(),
        pending_commands: std::collections::VecDeque::new(),
        reload_inbox: None,
        forward_view_diffs: false,
        config,
        processes_seen: None,
        jobs_seen: None,
    };

    let _ = ctx
        .handle_idle_command(
            RuntimeCommand::ResetConfigValue {
                path: "general.approval".into(),
                expected_revision: revision,
                request_id: Some("reset".into()),
            },
            &mut commands,
        )
        .await;

    assert_eq!(mode.get(), crate::tools::ApprovalMode::Safe);
    unsafe {
        match old_bone {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn reload_settings_reports_config_yaml_and_fresh_snapshot() {
    let _guard = crate::util::test_env_lock();
    let old_bone = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    let extensions = crate::ext::ExtensionManager::unloaded();
    let submit_inbox = extensions.submit_inbox();
    let config = crate::config::store::ConfigStore::new(extensions.clone()).unwrap();
    let before_revision = config.snapshot().revision;
    let mut persisted = crate::config::settings::Settings::load().unwrap().unwrap();
    persisted
        .set_value("general", "show_thinking", "true".into())
        .unwrap();
    let (hub, mut commands) = Hub::new();
    let mut events = hub.subscribe();
    let mut ctx = DaemonCtx {
        hub: hub.publisher(),
        llm: Arc::new(ConfigTestProvider),
        extensions,
        submit_inbox,
        session: Arc::new(Mutex::new(crate::runtime::RuntimeSession::new(
            crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
        ))),
        mode: crate::tools::SharedApprovalMode::new(crate::tools::ApprovalMode::Safe),
        approval_registry: crate::runtime::ApprovalReplyRegistry::new(),
        key_registry: crate::runtime::KeyReplyRegistry::new(),
        pending_interactions: PendingInteractions::default(),
        pending_commands: std::collections::VecDeque::new(),
        reload_inbox: None,
        forward_view_diffs: false,
        config,
        processes_seen: None,
        jobs_seen: None,
    };

    let _ = ctx
        .handle_idle_command(RuntimeCommand::ReloadSettings, &mut commands)
        .await;

    let event = events.recv().await.unwrap();
    let RuntimeEvent::ConfigChanged {
        changed_paths,
        snapshot,
        ..
    } = event
    else {
        panic!("expected ConfigChanged");
    };
    assert_eq!(changed_paths, vec!["config.yaml"]);
    assert_eq!(snapshot.revision, before_revision + 1);
    assert_eq!(snapshot.values["general"]["show_reasoning"], true);

    unsafe {
        match old_bone {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[tokio::test]
async fn dropping_remote_client_closes_its_transport() {
    let (client_io, mut peer_io) = tokio::io::duplex(4096);
    let (read_half, write_half) = tokio::io::split(client_io);
    let client = RemoteClient::connect(read_half, write_half);

    drop(client);

    let mut byte = [0_u8; 1];
    let read = tokio::time::timeout(std::time::Duration::from_secs(1), peer_io.read(&mut byte))
        .await
        .expect("remote bridge kept the transport open")
        .unwrap();
    assert_eq!(read, 0, "peer should observe EOF after client drop");
}

#[tokio::test]
async fn remote_eof_closes_frontend_event_receivers() {
    let (client_io, peer_io) = tokio::io::duplex(4096);
    let (read_half, write_half) = tokio::io::split(client_io);
    let client = RemoteClient::connect(read_half, write_half);
    let mut events = client.subscribe();

    drop(peer_io);

    let result = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
        .await
        .expect("frontend event receiver stayed open after socket EOF");
    assert!(matches!(
        result,
        Err(tokio::sync::broadcast::error::RecvError::Closed)
    ));

    let mut late = client.subscribe();
    assert!(matches!(
        late.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Closed)
    ));
}

#[tokio::test]
async fn hub_fans_out_events_and_merges_commands() {
    let (hub, mut commands_rx) = Hub::new();

    // Two clients connected by in-memory duplex pipes.
    let (client_a, server_a) = tokio::io::duplex(4096);
    let (client_b, server_b) = tokio::io::duplex(4096);
    tokio::spawn(serve_connection(server_a, hub.clone(), vec![]));
    tokio::spawn(serve_connection(
        server_b,
        hub.clone(),
        vec![RuntimeEvent::Status {
            message: "welcome".into(),
        }],
    ));

    // Give the writer tasks a moment to subscribe before broadcasting.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(hub.client_count(), 2);

    // Broadcast an event; both clients receive it.
    hub.publish(RuntimeEvent::Finished {
        content: "done".into(),
    });

    let mut ra = codec::MessageReader::new(tokio::io::split(client_a).0);
    let ev_a: RuntimeEvent = ra.read().await.unwrap().unwrap();
    assert!(matches!(ev_a, RuntimeEvent::Finished { content } if content == "done"));

    // Client B saw its initial welcome first, then the broadcast.
    let mut rb = codec::MessageReader::new(tokio::io::split(client_b).0);
    let ev_b0: RuntimeEvent = rb.read().await.unwrap().unwrap();
    assert!(matches!(ev_b0, RuntimeEvent::Status { message } if message == "welcome"));
    let ev_b1: RuntimeEvent = rb.read().await.unwrap().unwrap();
    assert!(matches!(ev_b1, RuntimeEvent::Finished { .. }));

    // A client writes a command; the hub surfaces it on the merged stream.
    let (client_c, server_c) = tokio::io::duplex(4096);
    tokio::spawn(serve_connection(server_c, hub.clone(), vec![]));
    let mut wc = tokio::io::split(client_c).1;
    codec::write_message(
        &mut wc,
        &RuntimeCommand::SubmitPrompt {
            request_id: None,
            text: "hi".into(),
            images: vec![],
        },
    )
    .await
    .unwrap();

    let cmd = commands_rx.recv().await.unwrap();
    assert!(matches!(cmd, RuntimeCommand::SubmitPrompt { text, .. } if text == "hi"));
}

#[tokio::test]
async fn socket_bridge_reports_broadcast_lag() {
    let (hub, _commands) = Hub::new();
    let (client, server) = tokio::io::duplex(64);
    let bridge = tokio::spawn(serve_connection(server, hub.clone(), vec![]));

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while hub.client_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("socket bridge did not subscribe");

    hub.publish(RuntimeEvent::Status {
        message: "x".repeat(256),
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !hub.events_tx.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("socket writer did not take the first event");

    for index in 0..1_100 {
        hub.publish(RuntimeEvent::Status {
            message: index.to_string(),
        });
    }

    let mut reader = codec::MessageReader::new(tokio::io::split(client).0);
    assert!(matches!(
        reader.read::<RuntimeEvent>().await.unwrap().unwrap(),
        RuntimeEvent::Status { .. }
    ));
    let lagged = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read())
        .await
        .expect("lag marker timed out")
        .unwrap()
        .unwrap();
    assert!(matches!(
        lagged,
        RuntimeEvent::StreamLagged { skipped } if skipped > 0
    ));

    bridge.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn managed_socket_bridge_reports_broadcast_lag() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (manager, receiver) = SessionManager::new();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, |_| {
                let (hub, mut commands) = Hub::new();
                let publisher = hub.publisher();
                Ok(ManagedRuntime {
                    conversation_id: 1,
                    hub,
                    initial: Arc::new(Vec::new),
                    task: Box::pin(async move {
                        if commands.recv().await.is_some() {
                            publisher.publish(RuntimeEvent::Status {
                                message: "x".repeat(256),
                            });
                            while !publisher.events_tx.is_empty() {
                                tokio::task::yield_now().await;
                            }
                            for index in 0..1_100 {
                                publisher.publish(RuntimeEvent::Status {
                                    message: index.to_string(),
                                });
                            }
                        }
                        std::future::pending::<()>().await;
                    }),
                })
            }));
            let (client, server) = tokio::io::duplex(64);
            let bridge = tokio::task::spawn_local(serve_managed_connection(
                server,
                manager,
                SessionTarget::Latest,
            ));
            let (read, mut write) = tokio::io::split(client);
            codec::write_message(&mut write, &RuntimeCommand::GetProcesses)
                .await
                .unwrap();

            let mut reader = codec::MessageReader::new(read);
            assert!(matches!(
                reader.read::<RuntimeEvent>().await.unwrap().unwrap(),
                RuntimeEvent::Status { .. }
            ));
            let lagged = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read())
                .await
                .expect("managed lag marker timed out")
                .unwrap()
                .unwrap();
            assert!(matches!(
                lagged,
                RuntimeEvent::StreamLagged { skipped } if skipped > 0
            ));

            bridge.abort();
            runner.abort();
        })
        .await;
}

#[tokio::test]
async fn malformed_frame_is_skipped_not_fatal() {
    let (hub, mut commands_rx) = Hub::new();
    let (client, server) = tokio::io::duplex(4096);
    tokio::spawn(serve_connection(server, hub.clone(), vec![]));

    let mut w = tokio::io::split(client).1;
    // Garbage line, then a valid command on the next line.
    w.write_all(b"{not valid json}\n").await.unwrap();
    codec::write_message(&mut w, &RuntimeCommand::Cancel)
        .await
        .unwrap();

    let cmd = commands_rx.recv().await.unwrap();
    assert!(matches!(cmd, RuntimeCommand::Cancel));
}

fn fake_managed_runtime(
    id: i64,
    active: Arc<std::sync::atomic::AtomicUsize>,
    max_active: Arc<std::sync::atomic::AtomicUsize>,
) -> ManagedRuntime {
    use std::sync::atomic::Ordering;

    let (hub, mut commands) = Hub::new();
    let publisher = hub.publisher();
    let initial_hub = hub.clone();
    let initial = Arc::new(move || {
        let snapshot = bone_protocol::SessionSnapshot {
            conversation_id: Some(id),
            ..Default::default()
        };
        vec![RuntimeEvent::ConversationLoaded {
            messages: Vec::new(),
            snapshot,
            busy: initial_hub.is_busy(),
        }]
    });
    let task = Box::pin(async move {
        while let Some(command) = commands.recv().await {
            if let RuntimeCommand::SubmitPrompt { text, .. } = command {
                let _turn = publisher.begin_turn();
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                publisher.publish(RuntimeEvent::Started {
                    request_id: None,
                    approval: "safe".into(),
                    task: String::new(),
                    model: "test".into(),
                    display: None,
                });
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                publisher.publish(RuntimeEvent::Finished {
                    content: format!("session-{id}:{text}"),
                });
                publisher.publish(RuntimeEvent::TurnComplete);
                active.fetch_sub(1, Ordering::SeqCst);
            }
        }
    });
    ManagedRuntime {
        conversation_id: id,
        hub,
        initial,
        task,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn managed_connections_isolate_events_and_run_concurrently() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (manager, receiver) = SessionManager::new();
            let factory_active = active.clone();
            let factory_max = max_active.clone();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, move |target| {
                let id = match target {
                    SessionTarget::Latest => 1,
                    SessionTarget::New => 3,
                    SessionTarget::Conversation(id) => id,
                };
                Ok(fake_managed_runtime(
                    id,
                    factory_active.clone(),
                    factory_max.clone(),
                ))
            }));

            let (client_a, server_a) = tokio::io::duplex(4096);
            let (client_b, server_b) = tokio::io::duplex(4096);
            let serve_a = tokio::task::spawn_local(serve_managed_connection(
                server_a,
                manager.clone(),
                SessionTarget::Latest,
            ));
            let serve_b = tokio::task::spawn_local(serve_managed_connection(
                server_b,
                manager.clone(),
                SessionTarget::Latest,
            ));
            let (read_a, mut write_a) = tokio::io::split(client_a);
            let (read_b, mut write_b) = tokio::io::split(client_b);
            let mut read_a = codec::MessageReader::new(read_a);
            let mut read_b = codec::MessageReader::new(read_b);

            // Both initially attach to actor 1. Move only B to actor 2.
            let _: RuntimeEvent = read_a.read().await.unwrap().unwrap();
            let _: RuntimeEvent = read_b.read().await.unwrap().unwrap();
            codec::write_message(&mut write_b, &RuntimeCommand::LoadConversation { id: 2 })
                .await
                .unwrap();
            let switched: RuntimeEvent = read_b.read().await.unwrap().unwrap();
            assert!(matches!(
                switched,
                RuntimeEvent::ConversationLoaded { snapshot, .. }
                    if snapshot.conversation_id == Some(2)
            ));

            codec::write_message(
                &mut write_a,
                &RuntimeCommand::SubmitPrompt {
                    request_id: None,
                    text: "alpha".into(),
                    images: vec![],
                },
            )
            .await
            .unwrap();
            codec::write_message(
                &mut write_b,
                &RuntimeCommand::SubmitPrompt {
                    request_id: None,
                    text: "beta".into(),
                    images: vec![],
                },
            )
            .await
            .unwrap();

            let started: RuntimeEvent = read_a.read().await.unwrap().unwrap();
            assert!(matches!(started, RuntimeEvent::Started { .. }));

            // A newly attached client must learn that actor 1 is already mid-turn,
            // even though it missed the actor's Started event.
            let (client_c, server_c) = tokio::io::duplex(4096);
            let serve_c = tokio::task::spawn_local(serve_managed_connection(
                server_c,
                manager,
                SessionTarget::Conversation(1),
            ));
            let (read_c, _) = tokio::io::split(client_c);
            let mut read_c = codec::MessageReader::new(read_c);
            let attached: RuntimeEvent = read_c.read().await.unwrap().unwrap();
            assert!(matches!(
                attached,
                RuntimeEvent::ConversationLoaded {
                    busy: true,
                    snapshot,
                    ..
                } if snapshot.conversation_id == Some(1)
            ));

            async fn finished<R: AsyncRead + Unpin>(
                reader: &mut codec::MessageReader<R>,
            ) -> String {
                loop {
                    match reader.read::<RuntimeEvent>().await.unwrap().unwrap() {
                        RuntimeEvent::Finished { content } => return content,
                        _ => continue,
                    }
                }
            }
            let (a, b) = tokio::join!(finished(&mut read_a), finished(&mut read_b));
            assert_eq!(a, "session-1:alpha");
            assert_eq!(b, "session-2:beta");
            assert_eq!(
                max_active.load(std::sync::atomic::Ordering::SeqCst),
                2,
                "different conversation actors should overlap"
            );

            serve_a.abort();
            serve_b.abort();
            serve_c.abort();
            runner.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn managed_load_failure_is_correlated() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (manager, receiver) = SessionManager::new();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, move |target| {
                let id = match target {
                    SessionTarget::Conversation(404) => return Err("conversation not found".into()),
                    SessionTarget::Conversation(id) => id,
                    SessionTarget::Latest => 1,
                    SessionTarget::New => 2,
                };
                Ok(fake_managed_runtime(id, active.clone(), max_active.clone()))
            }));

            let (client, server) = tokio::io::duplex(4096);
            let serve = tokio::task::spawn_local(serve_managed_connection(
                server,
                manager,
                SessionTarget::Latest,
            ));
            let (read, mut write) = tokio::io::split(client);
            let mut read = codec::MessageReader::new(read);
            let _: RuntimeEvent = read.read().await.unwrap().unwrap();

            codec::write_message(&mut write, &RuntimeCommand::LoadConversation { id: 404 })
                .await
                .unwrap();
            let failed: RuntimeEvent =
                tokio::time::timeout(std::time::Duration::from_secs(1), read.read())
                    .await
                    .expect("load failure response timed out")
                    .unwrap()
                    .unwrap();
            assert!(matches!(
                failed,
                RuntimeEvent::ConversationLoadFailed { id: 404, message }
                    if message == "conversation not found"
            ));

            serve.abort();
            runner.abort();
        })
        .await;
}

#[tokio::test]
async fn socket_conn_skips_decode_errors_then_reads_next_event() {
    use crate::runtime::{RuntimeConn, SocketConn};

    let (read_side, mut peer) = tokio::io::duplex(4096);
    let mut conn = SocketConn::new(read_side, tokio::io::sink());
    peer.write_all(b"not json\n").await.unwrap();
    codec::write_message(
        &mut peer,
        &RuntimeEvent::Status {
            message: "healthy".into(),
        },
    )
    .await
    .unwrap();

    let event = tokio::time::timeout(std::time::Duration::from_secs(1), conn.next_event())
        .await
        .expect("socket read timed out");
    assert!(matches!(event, Some(RuntimeEvent::Status { message }) if message == "healthy"));
}

#[tokio::test]
async fn socket_conn_terminates_on_oversized_frame() {
    use crate::runtime::{RuntimeConn, SocketConn};

    let input = std::io::Cursor::new(vec![b'x'; codec::MAX_LINE_BYTES + 1]);
    let mut conn = SocketConn::new(input, tokio::io::sink());

    let event = tokio::time::timeout(std::time::Duration::from_secs(5), conn.next_event())
        .await
        .expect("oversized frame caused a retry loop");
    assert!(event.is_none());
}

#[tokio::test]
async fn socket_conn_terminates_on_io_error() {
    use crate::runtime::{RuntimeConn, SocketConn};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    struct ErrorReader;
    impl AsyncRead for ErrorReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::other("read failed")))
        }
    }

    let mut conn = SocketConn::new(ErrorReader, tokio::io::sink());
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), conn.next_event())
        .await
        .expect("I/O error caused a retry loop");
    assert!(event.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn managed_actor_panic_does_not_stop_other_sessions() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (manager, receiver) = SessionManager::new();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, move |target| {
                let id = match target {
                    SessionTarget::Conversation(id) => id,
                    _ => return Err("explicit conversation required".into()),
                };
                if id == 1 {
                    let (hub, _commands) = Hub::new();
                    Ok(ManagedRuntime {
                        conversation_id: id,
                        hub,
                        initial: Arc::new(Vec::new),
                        task: Box::pin(async { panic!("actor boom") }),
                    })
                } else {
                    Ok(fake_managed_runtime(id, active.clone(), max_active.clone()))
                }
            }));

            let mut failed = manager
                .attach(SessionTarget::Conversation(1))
                .await
                .unwrap();
            let panic_status =
                tokio::time::timeout(std::time::Duration::from_secs(1), failed.events.recv())
                    .await
                    .expect("panic status timed out")
                    .unwrap();
            assert!(matches!(
                panic_status,
                RuntimeEvent::Status { message } if message.contains("actor boom")
            ));

            let mut healthy = manager
                .attach(SessionTarget::Conversation(2))
                .await
                .expect("manager stopped after another actor panicked");
            healthy
                .commands
                .send(RuntimeCommand::SubmitPrompt {
                    request_id: None,
                    text: "still alive".into(),
                    images: vec![],
                })
                .unwrap();
            let content = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    if let RuntimeEvent::Finished { content } = healthy.events.recv().await.unwrap()
                    {
                        break content;
                    }
                }
            })
            .await
            .expect("healthy actor did not respond");
            assert_eq!(content, "session-2:still alive");

            runner.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn managed_event_channel_closure_writes_one_status_then_eof() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (manager, receiver) = SessionManager::new();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, |_| {
                let (hub, _commands) = Hub::new();
                Ok(ManagedRuntime {
                    conversation_id: 1,
                    hub,
                    initial: Arc::new(Vec::new),
                    task: Box::pin(async {}),
                })
            }));
            let (client, server) = tokio::io::duplex(4096);
            let serve = tokio::task::spawn_local(serve_managed_connection(
                server,
                manager,
                SessionTarget::Latest,
            ));
            let mut reader = codec::MessageReader::new(client);

            let terminal = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read())
                .await
                .expect("terminal status timed out")
                .unwrap()
                .unwrap();
            assert!(matches!(
                terminal,
                RuntimeEvent::Status { message } if message == "conversation runtime stopped"
            ));
            let eof = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                reader.read::<RuntimeEvent>(),
            )
            .await
            .expect("managed connection did not close after terminal status");
            assert!(eof.is_none());
            serve.await.unwrap().unwrap();

            runner.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn managed_sessions_never_evict_attached_actors() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let created = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (manager, receiver) = SessionManager::new();
            let factory_created = created.clone();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, move |target| {
                factory_created.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let SessionTarget::Conversation(id) = target else {
                    return Err("explicit conversation required".into());
                };
                Ok(fake_managed_runtime(id, active.clone(), max_active.clone()))
            }));

            let mut attachments = Vec::new();
            for id in 1..=MAX_CACHED_ACTORS as i64 + 1 {
                attachments.push(
                    manager
                        .attach(SessionTarget::Conversation(id))
                        .await
                        .unwrap(),
                );
            }
            drop(
                manager
                    .attach(SessionTarget::Conversation(1))
                    .await
                    .unwrap(),
            );

            assert_eq!(
                created.load(std::sync::atomic::Ordering::SeqCst),
                MAX_CACHED_ACTORS + 1
            );
            drop(attachments);
            runner.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn managed_sessions_evict_the_oldest_disconnected_actor() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let created = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (manager, receiver) = SessionManager::new();
            let factory_created = created.clone();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, move |target| {
                factory_created.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let SessionTarget::Conversation(id) = target else {
                    return Err("explicit conversation required".into());
                };
                Ok(fake_managed_runtime(id, active.clone(), max_active.clone()))
            }));

            for id in 1..=MAX_CACHED_ACTORS as i64 + 1 {
                drop(
                    manager
                        .attach(SessionTarget::Conversation(id))
                        .await
                        .unwrap(),
                );
            }
            assert_eq!(
                created.load(std::sync::atomic::Ordering::SeqCst),
                MAX_CACHED_ACTORS + 1
            );

            // Actor 1 was the least recently attached idle entry and must be
            // reconstructed rather than retained beyond the cache bound.
            drop(
                manager
                    .attach(SessionTarget::Conversation(1))
                    .await
                    .unwrap(),
            );
            assert_eq!(
                created.load(std::sync::atomic::Ordering::SeqCst),
                MAX_CACHED_ACTORS + 2
            );

            runner.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn managed_sessions_do_not_evict_disconnected_running_actor() {
    struct DropFlag(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let created = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let factory_created = created.clone();
            let factory_dropped = dropped.clone();
            let (manager, receiver) = SessionManager::new();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, move |target| {
                factory_created.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let SessionTarget::Conversation(id) = target else {
                    return Err("explicit conversation required".into());
                };
                let (hub, mut commands) = Hub::new();
                if id != 1 {
                    return Ok(ManagedRuntime {
                        conversation_id: id,
                        hub,
                        initial: Arc::new(Vec::new),
                        task: Box::pin(std::future::pending()),
                    });
                }

                let publisher = hub.publisher();
                let dropped = factory_dropped.clone();
                Ok(ManagedRuntime {
                    conversation_id: id,
                    hub,
                    initial: Arc::new(Vec::new),
                    task: Box::pin(async move {
                        if matches!(
                            commands.recv().await,
                            Some(RuntimeCommand::SubmitPrompt { .. })
                        ) {
                            let _turn = publisher.begin_turn();
                            publisher.publish(RuntimeEvent::Started {
                                request_id: None,
                                approval: "safe".into(),
                                task: String::new(),
                                model: "test".into(),
                                display: None,
                            });
                            let _drop_flag = DropFlag(dropped);
                            std::future::pending::<()>().await;
                        }
                    }),
                })
            }));

            let mut running = manager
                .attach(SessionTarget::Conversation(1))
                .await
                .unwrap();
            running
                .commands
                .send(RuntimeCommand::SubmitPrompt {
                    request_id: None,
                    text: "keep running".into(),
                    images: vec![],
                })
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while !matches!(
                    running.events.recv().await,
                    Ok(RuntimeEvent::Started { .. })
                ) {}
            })
            .await
            .expect("actor did not start");
            drop(running);

            for id in 2..=MAX_CACHED_ACTORS as i64 + 1 {
                drop(
                    manager
                        .attach(SessionTarget::Conversation(id))
                        .await
                        .unwrap(),
                );
            }
            drop(
                manager
                    .attach(SessionTarget::Conversation(1))
                    .await
                    .unwrap(),
            );

            assert_eq!(
                created.load(std::sync::atomic::Ordering::SeqCst),
                MAX_CACHED_ACTORS + 1
            );
            assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));

            runner.abort();
        })
        .await;
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn process_commands_are_conversation_scoped() {
    let _guard = crate::util::test_env_lock();
    let old_bone = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    let conversation_id = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        % i64::MAX as u128) as i64;
    let scope = crate::processes::conversation_scope(Some(conversation_id));
    let foreign_scope = crate::processes::conversation_scope(Some(conversation_id + 1));
    let registry = crate::processes::registry();
    let own_id = registry.spawn("sleep 30".into(), scope.clone(), 60_000, None);
    let foreign_id = registry.spawn("sleep 30".into(), foreign_scope.clone(), 60_000, None);

    let extensions = crate::ext::ExtensionManager::unloaded();
    let submit_inbox = extensions.submit_inbox();
    let config = crate::config::store::ConfigStore::new(extensions.clone()).unwrap();
    let (hub, mut commands) = Hub::new();
    let mut events = hub.subscribe();
    let mut session = crate::runtime::RuntimeSession::new(
        crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
    );
    session.conversation_id = Some(conversation_id);
    let mut ctx = DaemonCtx {
        hub: hub.publisher(),
        llm: Arc::new(ConfigTestProvider),
        extensions,
        submit_inbox,
        session: Arc::new(Mutex::new(session)),
        mode: crate::tools::SharedApprovalMode::new(crate::tools::ApprovalMode::Safe),
        approval_registry: crate::runtime::ApprovalReplyRegistry::new(),
        key_registry: crate::runtime::KeyReplyRegistry::new(),
        pending_interactions: PendingInteractions::default(),
        pending_commands: std::collections::VecDeque::new(),
        reload_inbox: None,
        forward_view_diffs: false,
        config,
        processes_seen: None,
        jobs_seen: None,
    };

    ctx.handle_idle_command(RuntimeCommand::GetProcesses, &mut commands)
        .await;
    let RuntimeEvent::ProcessesSnapshot { processes, .. } = events.recv().await.unwrap() else {
        panic!("expected process snapshot");
    };
    assert_eq!(processes.len(), 1);
    assert_eq!(processes[0].id, own_id);
    assert_eq!(processes[0].owner, scope);

    ctx.handle_idle_command(
        RuntimeCommand::CancelProcess {
            id: foreign_id.clone(),
        },
        &mut commands,
    )
    .await;
    assert!(registry.get(&foreign_id).unwrap().running);

    ctx.handle_idle_command(
        RuntimeCommand::CancelProcess { id: own_id.clone() },
        &mut commands,
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while registry.get(&own_id).is_some_and(|process| process.running) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scoped process was not cancelled");

    registry.kill_scoped(&foreign_scope, &foreign_id);
    unsafe {
        match old_bone {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn set_incognito_publishes_status_and_snapshot_and_blocks_loads() {
    let _guard = crate::util::test_env_lock();
    let extensions = crate::ext::ExtensionManager::unloaded();
    let session = crate::runtime::RuntimeSession::new(crate::tools::registry::ToolHandler::new(
        crate::tools::builtin_tools(),
    ));
    let (mut ctx, hub, mut commands) =
        test_daemon_ctx(Arc::new(ConfigTestProvider), extensions, session);
    let mut events = hub.subscribe();

    ctx.handle_idle_command(
        RuntimeCommand::SetIncognito { enabled: true },
        &mut commands,
    )
    .await;
    assert!(matches!(
        events.recv().await.unwrap(),
        RuntimeEvent::Status { message } if message.contains("Incognito on")
    ));
    let RuntimeEvent::StateSnapshot { snapshot } = events.recv().await.unwrap() else {
        panic!("expected snapshot");
    };
    assert!(snapshot.incognito);

    // While incognito the daemon refuses to re-attach a conversation: that
    // would silently resume DB writes behind the INC badge.
    ctx.handle_idle_command(RuntimeCommand::LoadConversation { id: 7 }, &mut commands)
        .await;
    assert!(matches!(
        events.recv().await.unwrap(),
        RuntimeEvent::ConversationLoadFailed { id: 7, message } if message.contains("incognito")
    ));

    ctx.handle_idle_command(
        RuntimeCommand::SetIncognito { enabled: false },
        &mut commands,
    )
    .await;
    assert!(matches!(
        events.recv().await.unwrap(),
        RuntimeEvent::Status { message } if message.contains("Incognito off")
    ));
    let RuntimeEvent::StateSnapshot { snapshot } = events.recv().await.unwrap() else {
        panic!("expected snapshot");
    };
    assert!(!snapshot.incognito);
}

#[test]
fn incognito_transitions_cancel_jobs_in_the_departing_scope() {
    let _guard = crate::util::test_env_lock();
    let scope = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        % i64::MAX as u128) as i64;
    let extensions = crate::ext::ExtensionManager::unloaded();
    let mut session = crate::runtime::RuntimeSession::new(
        crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
    );
    session.conversation_id = Some(scope);
    let (mut ctx, _hub, _commands) =
        test_daemon_ctx(Arc::new(ConfigTestProvider), extensions, session);
    let registry = crate::ext::jobs::registry();

    let persisted_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let persisted_job = registry.create(crate::ext::jobs::NewJob {
        agent: "test".into(),
        task: "persisted scope".into(),
        title: "persisted scope".into(),
        provider: "test".into(),
        scope: Some(scope),
        cancel_flag: persisted_flag.clone(),
    });
    assert!(registry.start(&persisted_job));

    ctx.set_incognito(true);
    assert!(persisted_flag.load(std::sync::atomic::Ordering::Relaxed));
    assert!(ctx.session.lock().unwrap().incognito);

    let temp = tempfile::tempdir().unwrap();
    let db = crate::session_db::SessionDb::open(&temp.path().join("sessions.db")).unwrap();
    ctx.session.lock().unwrap().session_db = Some(db);
    let incognito_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let incognito_scope = ctx.session.lock().unwrap().background_scope();
    let incognito_job = registry.create(crate::ext::jobs::NewJob {
        agent: "test".into(),
        task: "incognito scope".into(),
        title: "incognito scope".into(),
        provider: "test".into(),
        scope: Some(incognito_scope),
        cancel_flag: incognito_flag.clone(),
    });
    assert!(registry.start(&incognito_job));

    // Re-applying the current mode is a no-op and must not cancel its work.
    ctx.set_incognito(true);
    assert!(!incognito_flag.load(std::sync::atomic::Ordering::Relaxed));

    ctx.set_incognito(false);
    assert!(incognito_flag.load(std::sync::atomic::Ordering::Relaxed));
    let session = ctx.session.lock().unwrap();
    assert!(!session.incognito);
    assert!(session.conversation_id.is_some());
    drop(session);

    registry.complete(&persisted_job, Err("cancelled".into()));
    registry.complete(&incognito_job, Err("cancelled".into()));
}

#[test]
fn incognito_actors_have_distinct_background_scopes() {
    let _guard = crate::util::test_env_lock();
    let extensions_a = crate::ext::ExtensionManager::unloaded();
    let extensions_b = crate::ext::ExtensionManager::unloaded();
    let mut session_a = crate::runtime::RuntimeSession::new(
        crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
    );
    let mut session_b = crate::runtime::RuntimeSession::new(
        crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
    );
    session_a.conversation_id = Some(-81_001);
    session_b.conversation_id = Some(-81_002);
    let (mut actor_a, hub_a, _commands_a) =
        test_daemon_ctx(Arc::new(ConfigTestProvider), extensions_a, session_a);
    let (mut actor_b, hub_b, _commands_b) =
        test_daemon_ctx(Arc::new(ConfigTestProvider), extensions_b, session_b);

    actor_a.set_incognito(true);
    actor_b.set_incognito(true);
    let scope_a = actor_a.session.lock().unwrap().background_scope();
    let scope_b = actor_b.session.lock().unwrap().background_scope();
    assert_ne!(scope_a, scope_b);
    assert_ne!(
        crate::processes::conversation_scope(Some(scope_a)),
        crate::processes::conversation_scope(Some(scope_b))
    );

    let registry = crate::ext::jobs::registry();
    let cancel_a = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_b = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let job_a = registry.create(crate::ext::jobs::NewJob {
        agent: "actor-a".into(),
        task: "actor A private task".into(),
        title: "A".into(),
        provider: "test".into(),
        scope: Some(scope_a),
        cancel_flag: cancel_a.clone(),
    });
    let job_b = registry.create(crate::ext::jobs::NewJob {
        agent: "actor-b".into(),
        task: "actor B private task".into(),
        title: "B".into(),
        provider: "test".into(),
        scope: Some(scope_b),
        cancel_flag: cancel_b.clone(),
    });
    let mut events_a = hub_a.subscribe();
    let mut events_b = hub_b.subscribe();

    actor_a.publish_jobs(true);
    let RuntimeEvent::JobsSnapshot { jobs, .. } = events_a.try_recv().unwrap() else {
        panic!("expected actor A jobs snapshot");
    };
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, job_a);

    actor_b.publish_jobs(true);
    let RuntimeEvent::JobsSnapshot { jobs, .. } = events_b.try_recv().unwrap() else {
        panic!("expected actor B jobs snapshot");
    };
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, job_b);

    actor_a.cancel_job(&job_b);
    assert!(!cancel_b.load(std::sync::atomic::Ordering::Relaxed));
    actor_a.cancel_job(&job_a);
    assert!(cancel_a.load(std::sync::atomic::Ordering::Relaxed));

    registry.complete(&job_a, Err("cancelled".into()));
    registry.complete(&job_b, Err("cleaned up".into()));
}

#[test]
fn jobs_snapshots_are_scoped_filtered_and_refreshed_after_cancellation() {
    let _guard = crate::util::test_env_lock();
    let scope = -((std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        % (i64::MAX as u128 - 1)) as i64
        + 1);
    let foreign_scope = scope - 1;
    let extensions = crate::ext::ExtensionManager::unloaded();
    let mut session = crate::runtime::RuntimeSession::new(
        crate::tools::registry::ToolHandler::new(crate::tools::builtin_tools()),
    );
    session.conversation_id = Some(scope);
    let (mut ctx, hub, _commands) =
        test_daemon_ctx(Arc::new(ConfigTestProvider), extensions, session);
    let mut events = hub.subscribe();
    let registry = crate::ext::jobs::registry();

    let queued = registry.create(crate::ext::jobs::NewJob {
        agent: "queued-agent".into(),
        task: "queued task".into(),
        title: "Queued".into(),
        provider: "queued-provider".into(),
        scope: Some(scope),
        cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    let running = registry.create(crate::ext::jobs::NewJob {
        agent: "running-agent".into(),
        task: "running task".into(),
        title: "Running".into(),
        provider: "running-provider".into(),
        scope: Some(scope),
        cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    assert!(registry.start(&running));
    registry.note_event(
        &running,
        RuntimeEvent::ToolOutput {
            call_id: "call-1".into(),
            content: "incremental".into(),
            stderr: false,
        },
        None,
    );
    registry.note_event(
        &running,
        RuntimeEvent::ToolCall {
            id: "call-1".into(),
            name: "edit_file".into(),
            summary: "editing".into(),
            arguments: serde_json::json!({ "path": "file" }),
        },
        Some("diff".into()),
    );
    let foreign = registry.create(crate::ext::jobs::NewJob {
        agent: "foreign-agent".into(),
        task: "foreign task".into(),
        title: "Foreign".into(),
        provider: "foreign-provider".into(),
        scope: Some(foreign_scope),
        cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });

    ctx.publish_jobs(true);
    let RuntimeEvent::JobsSnapshot { jobs, .. } = events.try_recv().unwrap() else {
        panic!("expected jobs snapshot");
    };
    assert_eq!(jobs.len(), 2);
    assert!(
        jobs.iter()
            .any(|job| { job.id == queued && job.status == bone_protocol::JobStatus::Queued })
    );
    let running_snapshot = jobs.iter().find(|job| job.id == running).unwrap();
    assert_eq!(running_snapshot.status, bone_protocol::JobStatus::Running);
    assert_eq!(running_snapshot.provider, "running-provider");
    assert!(matches!(
        running_snapshot.events.as_slice(),
        [bone_protocol::JobEventSnapshot::ToolCall {
            id,
            name,
            edit_preview: Some(preview),
            ..
        }] if id == "call-1" && name == "edit_file" && preview == "diff"
    ));
    assert!(jobs.iter().all(|job| job.id != foreign));

    ctx.cancel_background_work();
    let cancelled_snapshot = loop {
        match events.try_recv().unwrap() {
            RuntimeEvent::JobsSnapshot { jobs, .. } => break jobs,
            RuntimeEvent::Status { .. } => {}
            other => panic!("unexpected event after cancellation: {other:?}"),
        }
    };
    assert!(cancelled_snapshot.is_empty());

    registry.complete(&queued, Err("cancelled".into()));
    registry.complete(&running, Err("cancelled".into()));
    registry.complete(&foreign, Err("cleaned up".into()));
}

#[test]
fn oversized_jobs_snapshot_drops_oldest_events_to_fit() {
    let snapshot = bone_protocol::JobSnapshot {
        id: "job".into(),
        agent: "agent".into(),
        task: "task".into(),
        title: "title".into(),
        status: bone_protocol::JobStatus::Running,
        started_at: 0,
        token_sent: 0,
        token_received: 0,
        provider: "provider".into(),
        activity: None,
        events: ["old", "middle", "new"]
            .into_iter()
            .map(|prefix| bone_protocol::JobEventSnapshot::TextDelta {
                text: format!("{prefix}:{}", "\\\"".repeat(256)),
            })
            .collect(),
    };
    let mut newest_only = snapshot.clone();
    newest_only.events.drain(..2);
    let max_bytes = serde_json::to_vec(&RuntimeEvent::JobsSnapshot {
        version: 7,
        jobs: vec![newest_only],
    })
    .unwrap()
    .len();

    let event = super::bounded_jobs_snapshot(7, vec![snapshot], max_bytes);
    let encoded = serde_json::to_vec(&event).unwrap();
    assert!(encoded.len() <= max_bytes);
    let RuntimeEvent::JobsSnapshot { jobs, .. } = event else {
        unreachable!()
    };
    assert!(matches!(
        jobs[0].events.as_slice(),
        [bone_protocol::JobEventSnapshot::TextDelta { text }] if text.starts_with("new:")
    ));
}
