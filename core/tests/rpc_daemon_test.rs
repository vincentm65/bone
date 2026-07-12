//! Phase 3 acceptance: the runtime works as a *persistent* daemon over the RPC
//! protocol.
//!
//! A `MockProvider` is injected into `run_daemon` (which now owns a
//! `RuntimeSession` across turns), the hub is served over a real TCP socket, and
//! clients connect to submit prompts, approve tool calls over the wire, and
//! attach concurrently — proving the `nvim --embed`-style attach path end to end
//! with no real provider and no terminal.

use async_trait::async_trait;
use futures_util::StreamExt; // for .boxed()
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bone_core::ext::{BootedTools, ExtensionManager};
use bone_core::llm::provider::LlmProvider;
use bone_core::llm::{ChatEvent, ChatMessage, LlmError, ResponseStream};
use bone_core::rpc::codec::{MessageReader, write_message};
use bone_core::rpc::{Hub, run_daemon, serve_connection};
use bone_core::runtime::{RuntimeCommand, RuntimeEvent, RuntimeSession};
use bone_core::tools::registry::{ToolHandler, ToolRegistry};
use bone_core::tools::{ApprovalMode, CallOutcome, ToolCall, ToolDefinition, builtin_tools};

/// A provider whose `chat_stream` replays one scripted attempt per call (later
/// turns pop the next attempt), so a tool turn can answer once with a tool call
/// and then finish on the follow-up request.
struct MockProvider {
    script: Mutex<Vec<Vec<ChatEvent>>>,
}

impl MockProvider {
    fn single(events: Vec<ChatEvent>) -> Self {
        Self {
            script: Mutex::new(vec![events]),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }
    fn name(&self) -> &str {
        "Mock"
    }
    fn model(&self) -> &str {
        "mock-1"
    }
    fn set_model(&mut self, _model: String) {}
    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        let events = {
            let mut script = self.script.lock().unwrap();
            if script.is_empty() {
                Vec::new()
            } else {
                script.remove(0)
            }
        };
        Ok(futures_util::stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

/// Spawn a daemon owning a fresh persistent session backed by `provider`, plus a
/// TCP listener serving every client against the hub. Returns the bound address.
async fn spawn_daemon(provider: Arc<dyn LlmProvider>) -> (std::net::SocketAddr, Hub) {
    let (hub, commands_rx) = Hub::new();
    let session = std::sync::Arc::new(std::sync::Mutex::new(RuntimeSession::new(
        ToolHandler::new(builtin_tools()),
    )));
    tokio::spawn(run_daemon(
        hub.publisher(),
        commands_rx,
        provider,
        ExtensionManager::unloaded(),
        session,
        ApprovalMode::Safe,
        None,
        false,
        false,
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let listener_hub = hub.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let hub = listener_hub.clone();
            tokio::spawn(async move {
                let _ = serve_connection(stream, hub, Vec::new()).await;
            });
        }
    });
    (addr, hub)
}

async fn wait_for_clients(hub: &Hub, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while hub.client_count() < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("clients did not subscribe");
}

#[tokio::test]
async fn daemon_stops_when_last_command_sender_is_dropped() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::single(Vec::new()));
    let (hub, commands_rx) = Hub::new();
    let publisher = hub.publisher();
    let command_tx = hub.command_sender();
    let session = Arc::new(Mutex::new(RuntimeSession::new(ToolHandler::new(
        builtin_tools(),
    ))));
    let weak_session = Arc::downgrade(&session);

    let daemon = tokio::spawn(run_daemon(
        publisher,
        commands_rx,
        provider,
        ExtensionManager::unloaded(),
        session.clone(),
        ApprovalMode::Safe,
        None,
        false,
        false,
    ));

    drop(session);
    drop(command_tx);
    drop(hub);

    tokio::time::timeout(Duration::from_secs(1), daemon)
        .await
        .expect("daemon did not stop after its clients disconnected")
        .expect("daemon task panicked");
    assert!(
        weak_session.upgrade().is_none(),
        "daemon retained its RuntimeSession after shutdown"
    );
}

#[tokio::test]
async fn client_submits_prompt_over_socket_and_receives_turn() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::single(vec![
        ChatEvent::TextDelta("daemon ".into()),
        ChatEvent::TextDelta("ok".into()),
    ]));
    let (addr, _hub) = spawn_daemon(provider).await;

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_message(
        &mut write_half,
        &RuntimeCommand::SubmitPrompt {
            text: "hello daemon".into(),
            images: vec![],
        },
    )
    .await
    .unwrap();

    let mut reader = MessageReader::new(read_half);
    let finished = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match reader.read::<RuntimeEvent>().await {
                Some(Ok(RuntimeEvent::Finished { content })) => break Some(content),
                Some(Ok(_)) => continue,
                Some(Err(_)) => continue,
                None => break None,
            }
        }
    })
    .await
    .expect("daemon turn timed out");

    assert_eq!(
        finished.as_deref(),
        Some("daemon ok"),
        "client receives the streamed Finished event with assembled content"
    );
}

#[tokio::test]
async fn client_approves_tool_call_over_socket() {
    // A real readable file so an approved read_file succeeds (is_error=false).
    let path = std::env::temp_dir().join(format!(
        "bone-rpc-approval-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, "hello").unwrap();

    // Turn 1 answers with a tool call; the follow-up request finishes the turn.
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider {
        script: Mutex::new(vec![vec![ChatEvent::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": path.to_string_lossy() }),
        })]]),
    });
    let (addr, _hub) = spawn_daemon(provider).await;

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_message(
        &mut write_half,
        &RuntimeCommand::SubmitPrompt {
            text: "read it".into(),
            images: vec![],
        },
    )
    .await
    .unwrap();

    let mut reader = MessageReader::new(read_half);
    let tool_ok = tokio::time::timeout(Duration::from_secs(20), async {
        let mut approved = false;
        loop {
            match reader.read::<RuntimeEvent>().await {
                // Approve the tool call by echoing the request id back.
                Some(Ok(RuntimeEvent::ApprovalRequest { id, name, .. })) => {
                    assert_eq!(name, "read_file");
                    write_message(
                        &mut write_half,
                        &RuntimeCommand::ApprovalReply {
                            id,
                            outcome: CallOutcome::Approve,
                        },
                    )
                    .await
                    .unwrap();
                    approved = true;
                }
                Some(Ok(RuntimeEvent::ToolResult { name, is_error, .. }))
                    if name == "read_file" =>
                {
                    break Some((approved, is_error));
                }
                Some(Ok(_)) | Some(Err(_)) => continue,
                None => break None,
            }
        }
    })
    .await
    .expect("approval round-trip timed out");

    std::fs::remove_file(&path).ok();
    assert_eq!(
        tool_ok,
        Some((true, false)),
        "the tool ran (no error) only after the client approved it over the socket"
    );
}

#[tokio::test]
async fn two_clients_both_see_the_turn() {
    let provider: Arc<dyn LlmProvider> =
        Arc::new(MockProvider::single(vec![ChatEvent::TextDelta(
            "shared".into(),
        )]));
    let (addr, hub) = spawn_daemon(provider).await;

    // Client A submits; both A and B (attached first) should see Finished.
    let a = tokio::net::TcpStream::connect(addr).await.unwrap();
    let b = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (a_read, mut a_write) = tokio::io::split(a);
    let (b_read, _b_write) = tokio::io::split(b);

    wait_for_clients(&hub, 2).await;
    write_message(
        &mut a_write,
        &RuntimeCommand::SubmitPrompt {
            text: "go".into(),
            images: vec![],
        },
    )
    .await
    .unwrap();

    async fn wait_finished<R: tokio::io::AsyncRead + Unpin>(read: R) -> Option<String> {
        let mut reader = MessageReader::new(read);
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match reader.read::<RuntimeEvent>().await {
                    Some(Ok(RuntimeEvent::Finished { content })) => break Some(content),
                    Some(Ok(_)) | Some(Err(_)) => continue,
                    None => break None,
                }
            }
        })
        .await
        .expect("turn timed out")
    }

    let (fa, fb) = tokio::join!(wait_finished(a_read), wait_finished(b_read));
    assert_eq!(
        fa.as_deref(),
        Some("shared"),
        "submitting client sees the turn"
    );
    assert_eq!(
        fb.as_deref(),
        Some("shared"),
        "a second attached client sees the same broadcast turn"
    );
}

/// Boot-dedup Step A: when an in-process frontend shares the Lua VM with the
/// daemon, `ReloadExtensions` adopts the booted result the frontend leaves in
/// the reload inbox instead of re-reading disk and booting a second VM. Proves
/// the inbox is consumed (drained) and the session adopts exactly its tools.
#[tokio::test]
async fn reload_extensions_adopts_inbox_without_disk_boot() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::single(Vec::new()));
    let (hub, commands_rx) = Hub::new();
    // Session starts with the full builtin tool set (non-empty).
    let mut initial = ToolHandler::new(builtin_tools());
    // Seed session-scoped state that must survive the registry swap.
    initial
        .state_map
        .set("task_list", "default", r#"{"items":["a"]}"#.into());
    let snapshots_before = initial.snapshots.clone();
    let session = Arc::new(Mutex::new(RuntimeSession::new(initial)));
    assert!(
        !session.lock().unwrap().tools.definitions().is_empty(),
        "precondition: session boots with builtin tools",
    );

    // The frontend's "single boot" result: an empty tool set, distinguishable
    // from both the builtin set and anything a disk boot would produce.
    let inbox = Arc::new(Mutex::new(Some(BootedTools {
        manager: ExtensionManager::unloaded(),
        tools: ToolHandler::new(ToolRegistry::new()),
    })));

    tokio::spawn(run_daemon(
        hub.publisher(),
        commands_rx,
        provider,
        ExtensionManager::unloaded(),
        session.clone(),
        ApprovalMode::Safe,
        Some(inbox.clone()),
        false,
        false,
    ));

    let mut events = hub.subscribe();
    hub.command_sender()
        .send(RuntimeCommand::ReloadExtensions)
        .unwrap();

    // The daemon reports the adopted (inbox) tool count, not a disk-boot count.
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap() {
                RuntimeEvent::Status { message } if message.contains("reloaded") => break message,
                _ => continue,
            }
        }
    })
    .await
    .expect("reload status timed out");
    assert!(
        status.contains("0 tools enabled"),
        "adopted the inbox's empty tool set, got: {status}",
    );

    // Inbox drained (taken), and the session swapped to the adopted handler.
    assert!(inbox.lock().unwrap().is_none(), "inbox should be consumed");
    let tools = &session.lock().unwrap().tools;
    assert_eq!(
        tools.definitions().len(),
        0,
        "session adopted the inbox's empty tool handler",
    );
    assert_eq!(
        tools.state_map.get("task_list", "default"),
        Some(r#"{"items":["a"]}"#),
        "host tool state_map must survive ReloadExtensions",
    );
    assert!(
        Arc::ptr_eq(&tools.snapshots, &snapshots_before),
        "snapshot store Arc must be preserved across ReloadExtensions",
    );
}

/// Phase 3-bridge: `RemoteClient` adapts a socket to a remote daemon into the
/// same `command_sender()` / `subscribe()` shape the in-process `Hub` exposes.
/// A prompt pushed through the bridge runs on the daemon and its events come
/// back over the broadcast receiver.
#[tokio::test]
async fn remote_client_bridges_commands_and_events() {
    let provider: Arc<dyn LlmProvider> =
        Arc::new(MockProvider::single(vec![ChatEvent::TextDelta(
            "bridged".into(),
        )]));
    let (addr, _hub) = spawn_daemon(provider).await;

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (read_half, write_half) = tokio::io::split(stream);
    let client = bone_core::rpc::RemoteClient::connect(read_half, write_half);
    // Subscribe before the first await so nothing is missed.
    let mut events = client.subscribe();
    client
        .command_sender()
        .send(RuntimeCommand::SubmitPrompt {
            text: "go".into(),
            images: vec![],
        })
        .unwrap();

    let finished = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match events.recv().await.unwrap() {
                RuntimeEvent::Finished { content } => break content,
                _ => continue,
            }
        }
    })
    .await
    .expect("bridged turn timed out");
    assert_eq!(
        finished, "bridged",
        "prompt ran on the daemon and streamed back"
    );
}

/// A `SwitchProvider` to an unknown id fails inside the daemon, but it must
/// still publish a `StateSnapshot` afterwards — otherwise the frontend's
/// `await_state_snapshot()` hangs forever on a bad provider id / config edit.
#[tokio::test]
async fn failed_provider_switch_still_publishes_snapshot() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::single(Vec::new()));
    let (addr, _hub) = spawn_daemon(provider).await;

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_message(
        &mut write_half,
        &RuntimeCommand::SwitchProvider {
            provider_id: "definitely-not-a-real-provider".into(),
        },
    )
    .await
    .unwrap();

    let mut reader = MessageReader::new(read_half);
    let got_snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match reader.read::<RuntimeEvent>().await {
                Some(Ok(RuntimeEvent::StateSnapshot { .. })) => break true,
                Some(Ok(_)) | Some(Err(_)) => continue,
                None => break false,
            }
        }
    })
    .await
    .expect("daemon never published a snapshot after a failed switch");
    assert!(
        got_snapshot,
        "a failed switch must still unblock the frontend"
    );
}

/// Phase 3-bridge: a fresh client receives the daemon's on-connect full-state
/// replay (the `initial` events `serve_connection` writes first), so the remote
/// App learns the conversation id / totals immediately instead of waiting for a
/// turn. Mirrors what `bone serve`'s accept loop now sends.
#[tokio::test]
async fn remote_client_receives_initial_state_replay() {
    use bone_core::runtime::SessionSnapshot;

    let (hub, _commands_rx) = Hub::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let initial = vec![RuntimeEvent::StateSnapshot {
        snapshot: SessionSnapshot {
            conversation_id: Some(42),
            ..Default::default()
        },
    }];
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = serve_connection(stream, hub, initial).await;
    });

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (read_half, write_half) = tokio::io::split(stream);
    let client = bone_core::rpc::RemoteClient::connect(read_half, write_half);
    let mut events = client.subscribe();

    let conv = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let RuntimeEvent::StateSnapshot { snapshot } = events.recv().await.unwrap() {
                break snapshot.conversation_id;
            }
        }
    })
    .await
    .expect("initial snapshot not delivered");
    assert_eq!(conv, Some(42), "client surfaces the replayed initial state");
}

/// Phase 3-pure: with `forward_view_diffs = true` (the `bone serve` config), the
/// daemon drains the Lua `UiState` and forwards pane/UI diffs as
/// `RuntimeEvent::ViewDiff`, so a remote client renders panes it can't drain
/// directly. Seeds a diff before the turn; the post-turn flush forwards it even
/// for an instant mock turn.
#[tokio::test]
async fn daemon_forwards_view_diffs_to_remote_client() {
    use bone_core::runtime::view::ViewDiff;

    let provider: Arc<dyn LlmProvider> =
        Arc::new(MockProvider::single(vec![ChatEvent::TextDelta(
            "ok".into(),
        )]));
    let (hub, commands_rx) = Hub::new();
    let session = Arc::new(Mutex::new(RuntimeSession::new(ToolHandler::new(
        builtin_tools(),
    ))));
    let extensions = ExtensionManager::unloaded();
    {
        let ui = extensions.ui_handle();
        bone_core::ext::api_ui::lock_shared(&ui).apply(ViewDiff::SetHighlight {
            name: "marker".into(),
            fg: Some("#abcdef".into()),
        });
    }
    tokio::spawn(run_daemon(
        hub.publisher(),
        commands_rx,
        provider,
        extensions,
        session,
        ApprovalMode::Safe,
        None,
        true, // forward view diffs
        false,
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_hub = hub.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = serve_connection(stream, serve_hub, Vec::new()).await;
    });

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (read_half, write_half) = tokio::io::split(stream);
    let client = bone_core::rpc::RemoteClient::connect(read_half, write_half);
    let mut events = client.subscribe();
    client
        .command_sender()
        .send(RuntimeCommand::SubmitPrompt {
            text: "go".into(),
            images: vec![],
        })
        .unwrap();

    let diff = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let RuntimeEvent::ViewDiff { diff } = events.recv().await.unwrap() {
                break diff;
            }
        }
    })
    .await
    .expect("daemon did not forward a view diff");
    match diff {
        ViewDiff::SetHighlight { name, fg } => {
            assert_eq!(name, "marker");
            assert_eq!(fg.as_deref(), Some("#abcdef"));
        }
        other => panic!("unexpected diff: {other:?}"),
    }
}

/// Phase 3-pure: a remote client runs a registered Lua slash command *in the
/// daemon* via `RunCommand`. The daemon finds the handler in its own VM, runs
/// it, and returns the output in `CommandComplete` — no local VM needed on the
/// client side. Boots a real ExtensionManager from a temp config dir with a
/// custom `echo` command.
#[tokio::test]
async fn daemon_runs_registered_command_over_socket() {
    let config_dir = std::env::temp_dir().join(format!(
        "bone-runcmd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let cmd_dir = config_dir.join("lua/commands");
    std::fs::create_dir_all(&cmd_dir).unwrap();
    std::fs::write(
        cmd_dir.join("echo.lua"),
        r#"
bone.register_command("echo", {
  description = "echo back the input",
  handler = function(input, ctx)
    return { display = "echo: " .. input, submit = false }
  end,
})
"#,
    )
    .unwrap();

    let mut custom = bone_core::config::custom::CustomConfigs::default();
    let booted = bone_core::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        true,
        bone_core::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );
    let extensions = booted.manager;
    let session = Arc::new(Mutex::new(RuntimeSession::new(booted.tools)));
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::single(Vec::new()));
    let (hub, commands_rx) = Hub::new();
    tokio::spawn(run_daemon(
        hub.publisher(),
        commands_rx,
        provider,
        extensions,
        session,
        ApprovalMode::Safe,
        None,
        true,
        false,
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_hub = hub.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = serve_connection(stream, serve_hub, Vec::new()).await;
    });

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (read_half, write_half) = tokio::io::split(stream);
    let client = bone_core::rpc::RemoteClient::connect(read_half, write_half);
    let mut events = client.subscribe();
    client
        .command_sender()
        .send(RuntimeCommand::RunCommand {
            name: "echo".into(),
            input: "hi".into(),
        })
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let RuntimeEvent::CommandComplete { output, submit, .. } =
                events.recv().await.unwrap()
            {
                break (output, submit);
            }
        }
    })
    .await
    .expect("command did not complete");

    std::fs::remove_dir_all(&config_dir).ok();
    assert_eq!(result.0, "echo: hi", "daemon ran the command in its VM");
    assert!(!result.1, "echo is a display command, not a submit");
}

/// A registered command whose handler returns a no-op (`{ submit = false }`
/// with no output or action — what an interactive picker returns after the user
/// applies/cancels) must complete cleanly. It must NOT be reported as "unknown
/// command", because `parse_lua_command_return` maps such a return to `None` and
/// the daemon previously conflated that with "handler not found". Regression for
/// the `/themes`-picker-over-RPC false "unknown command: themes".
#[tokio::test]
async fn registered_command_with_noop_return_is_not_unknown() {
    let config_dir = std::env::temp_dir().join(format!(
        "bone-noopcmd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let cmd_dir = config_dir.join("lua/commands");
    std::fs::create_dir_all(&cmd_dir).unwrap();
    // Handler runs side effects (a picker would) but returns only `submit=false`,
    // which `parse_lua_command_return` collapses to `None`.
    std::fs::write(
        cmd_dir.join("noop.lua"),
        r#"
bone.register_command("noop", {
  description = "do work, return nothing to submit",
  handler = function(_input, _ctx)
    return { submit = false }
  end,
})
"#,
    )
    .unwrap();

    let mut custom = bone_core::config::custom::CustomConfigs::default();
    let booted = bone_core::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        true,
        bone_core::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );
    let extensions = booted.manager;
    let session = Arc::new(Mutex::new(RuntimeSession::new(booted.tools)));
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::single(Vec::new()));
    let (hub, commands_rx) = Hub::new();
    tokio::spawn(run_daemon(
        hub.publisher(),
        commands_rx,
        provider,
        extensions,
        session,
        ApprovalMode::Safe,
        None,
        true,
        false,
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_hub = hub.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = serve_connection(stream, serve_hub, Vec::new()).await;
    });

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (read_half, write_half) = tokio::io::split(stream);
    let client = bone_core::rpc::RemoteClient::connect(read_half, write_half);
    let mut events = client.subscribe();
    client
        .command_sender()
        .send(RuntimeCommand::RunCommand {
            name: "noop".into(),
            input: String::new(),
        })
        .unwrap();

    // Collect events until the command completes; assert no "unknown command"
    // Status was emitted along the way.
    let saw_unknown = tokio::time::timeout(Duration::from_secs(10), async {
        let mut saw_unknown = false;
        loop {
            match events.recv().await.unwrap() {
                RuntimeEvent::Status { message } if message.contains("unknown command") => {
                    saw_unknown = true;
                }
                RuntimeEvent::CommandComplete { .. } => break saw_unknown,
                _ => {}
            }
        }
    })
    .await
    .expect("command did not complete");

    std::fs::remove_dir_all(&config_dir).ok();
    assert!(
        !saw_unknown,
        "a found handler returning a no-op must not be reported as unknown"
    );
}

/// `frontend_state` packages the daemon VM's boot-time display state (banner,
/// theme, keymap, command-list, config) into a `FrontendState` event so a
/// VM-less frontend can render the user's customizations over the wire. Boots a
/// real ExtensionManager from a temp config dir whose `init.lua` sets a theme
/// color, a banner, and registers a command, then asserts the event carries them.
#[tokio::test]
async fn frontend_state_carries_theme_banner_and_commands() {
    let config_dir = std::env::temp_dir().join(format!(
        "bone-frontend-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&config_dir).unwrap();
    // init.lua: a theme color, a banner, and a registered command.
    std::fs::write(
        config_dir.join("init.lua"),
        r##"
bone.theme = bone.theme or {}
bone.theme.tool_call = "#FF0000"
function bone.banner() return { "hello from the daemon" } end
bone.register_command("ping", {
  description = "pong",
  handler = function(_i, _c) return { submit = false } end,
})
"##,
    )
    .unwrap();

    let mut custom = bone_core::config::custom::CustomConfigs::default();
    let booted = bone_core::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        true,
        bone_core::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );
    let extensions = booted.manager;

    let tools = booted.tools;
    let ev = bone_core::rpc::frontend_state(&extensions, &tools);
    std::fs::remove_dir_all(&config_dir).ok();

    let RuntimeEvent::FrontendState {
        banner,
        theme,
        commands,
        tool_defs,
        ..
    } = ev
    else {
        panic!("expected FrontendState");
    };
    // Builtin tools (e.g. read_file) must reach a VM-less frontend for context
    // estimation + tool-row rendering.
    assert!(
        tool_defs.iter().any(|t| t.name == "read_file"),
        "tool definitions must cross the wire, got {:?}",
        tool_defs.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
    assert!(
        banner.contains("hello from the daemon"),
        "banner from bone.banner() must cross the wire, got {banner:?}"
    );
    assert_eq!(
        theme.get("tool_call").and_then(|v| v.as_str()),
        Some("#FF0000"),
        "theme color must be serialized into the event"
    );
    assert!(
        commands.iter().any(|(n, d)| n == "ping" && d == "pong"),
        "registered command must be listed for autocomplete, got {commands:?}"
    );

    // The client's `apply_frontend_state` deserializes the JSON blob back into
    // the same `Lua*Snapshot` type the boot path uses — prove that round-trips.
    let theme_snap: bone_core::ext::snapshots::LuaThemeSnapshot =
        serde_json::from_value(theme).expect("theme JSON deserializes back to LuaThemeSnapshot");
    assert_eq!(theme_snap.tool_call.as_deref(), Some("#FF0000"));
}
