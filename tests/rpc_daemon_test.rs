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

use bone::ext::ExtensionManager;
use bone::llm::provider::LlmProvider;
use bone::llm::{ChatEvent, ChatMessage, LlmError, ResponseStream};
use bone::rpc::codec::{MessageReader, write_message};
use bone::rpc::{Hub, run_daemon, serve_connection};
use bone::runtime::{RuntimeCommand, RuntimeEvent, RuntimeSession};
use bone::tools::registry::ToolHandler;
use bone::tools::{ApprovalMode, CallOutcome, ToolCall, ToolDefinition, builtin_tools};

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
    let session = RuntimeSession::new(ToolHandler::new(builtin_tools()));
    tokio::spawn(run_daemon(
        hub.clone(),
        commands_rx,
        provider,
        ExtensionManager::unloaded(),
        session,
        ApprovalMode::Safe,
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
        &RuntimeCommand::SubmitPrompt { text: "go".into() },
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
