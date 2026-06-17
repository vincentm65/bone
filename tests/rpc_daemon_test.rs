//! Phase 5 acceptance: the runtime works as a daemon over the RPC protocol.
//!
//! A `MockProvider` is injected into `run_daemon`, the hub is served over a real
//! TCP socket, a client connects, submits a prompt, and receives the streamed
//! turn — proving the `nvim --embed`-style attach path end to end with no real
//! provider and no terminal.

use async_trait::async_trait;
use futures_util::StreamExt; // for .boxed()
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bone::llm::provider::LlmProvider;
use bone::llm::{ChatEvent, ChatMessage, LlmError, ResponseStream};
use bone::rpc::codec::{MessageReader, write_message};
use bone::rpc::{Hub, run_daemon, serve_connection};
use bone::runtime::{RuntimeCommand, RuntimeEvent};
use bone::tools::{ApprovalMode, ToolDefinition};

struct MockProvider {
    script: Mutex<Vec<ChatEvent>>,
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
        let events = self.script.lock().unwrap().drain(..).collect::<Vec<_>>();
        Ok(futures_util::stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

#[tokio::test]
#[ignore]
async fn client_submits_prompt_over_socket_and_receives_turn() {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider {
        script: Mutex::new(vec![
            ChatEvent::TextDelta("daemon ".into()),
            ChatEvent::TextDelta("ok".into()),
        ]),
    });

    let (hub, commands_rx) = Hub::new();
    tokio::spawn(run_daemon(
        hub.clone(),
        commands_rx,
        Some(provider),
        ApprovalMode::Safe,
    ));

    // Bind an ephemeral port and serve one client against the hub.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    {
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let _ = serve_connection(stream, hub, Vec::new()).await;
            }
        });
    }

    // Client connects, submits a prompt, and reads events until Finished.
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
        "client must receive the streamed Finished event with assembled content"
    );
}
