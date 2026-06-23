//! RPC transport for the runtime protocol.
//!
//! Carries [`RuntimeEvent`] (core → frontend) and [`RuntimeCommand`]
//! (frontend → core) over a byte stream as newline-delimited JSON. The same
//! `serde` types flow over an in-process channel (Phase 3) and here over a
//! socket — only the framing differs. (msgpack via `rmpv` could replace the
//! JSONL codec later without touching the protocol types.)
//!
//! Pieces:
//! - [`codec`]: read/write one framed message over any `AsyncRead`/`AsyncWrite`.
//! - [`Hub`]: fan out events to every attached client and merge their commands
//!   into one stream — the multi-client core of `nvim --embed`-style attach.
//! - [`serve_connection`]: glue one client stream to a `Hub`.
//! - [`run_daemon`]: a working headless daemon — each `SubmitPrompt` runs the
//!   agent and streams its events back to all clients.
//!
//! This module is part of core (no `crate::ui`); it compiles ratatui-free.

pub mod codec;

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc};

use crate::runtime::{RuntimeCommand, RuntimeEvent};

/// Fans [`RuntimeEvent`]s out to all attached clients and merges every client's
/// [`RuntimeCommand`]s into a single receiver the runtime consumes.
#[derive(Clone)]
pub struct Hub {
    events_tx: broadcast::Sender<RuntimeEvent>,
    commands_tx: mpsc::UnboundedSender<RuntimeCommand>,
}

impl Hub {
    /// Create a hub and the single command receiver the runtime reads from.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<RuntimeCommand>) {
        let (events_tx, _) = broadcast::channel(1024);
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        (
            Self {
                events_tx,
                commands_tx,
            },
            commands_rx,
        )
    }

    /// Broadcast an event to all attached clients. No-op if none are attached.
    pub fn publish(&self, event: RuntimeEvent) {
        let _ = self.events_tx.send(event);
    }

    /// Subscribe a new client to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.events_tx.subscribe()
    }

    /// A sender a client uses to push commands into the merged stream.
    pub fn command_sender(&self) -> mpsc::UnboundedSender<RuntimeCommand> {
        self.commands_tx.clone()
    }

    /// Current attached-client count (event subscribers).
    pub fn client_count(&self) -> usize {
        self.events_tx.receiver_count()
    }
}

/// Serve one client connection against `hub`.
///
/// Late-joiners get `initial` events first (full-state sync), then the live
/// broadcast. Reads run until the client disconnects; writes run until the
/// broadcast closes or the socket errors. Returns when the read side ends.
pub async fn serve_connection<S>(
    stream: S,
    hub: Hub,
    initial: Vec<RuntimeEvent>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let commands_tx = hub.command_sender();
    let mut events_rx = hub.subscribe();

    // Writer task: replay initial state, then stream live events.
    let writer = tokio::spawn(async move {
        let mut w = write_half;
        for ev in initial {
            if codec::write_message(&mut w, &ev).await.is_err() {
                return;
            }
        }
        loop {
            match events_rx.recv().await {
                Ok(ev) => {
                    if codec::write_message(&mut w, &ev).await.is_err() {
                        return;
                    }
                }
                // Dropped messages under backpressure: keep going.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    // Reader: decode commands until the client disconnects.
    let mut reader = codec::MessageReader::new(read_half);
    while let Some(result) = reader.read::<RuntimeCommand>().await {
        match result {
            Ok(cmd) => {
                if commands_tx.send(cmd).is_err() {
                    break; // runtime gone
                }
            }
            // Skip malformed frames rather than dropping the connection.
            Err(codec::ReadError::Decode(_)) => continue,
            Err(codec::ReadError::Io(e)) => {
                writer.abort();
                return Err(e);
            }
        }
    }

    writer.abort();
    Ok(())
}

/// A minimal but real headless daemon: consume merged commands, and for each
/// [`RuntimeCommand::SubmitPrompt`] run the agent, mapping its
/// [`crate::agent::AgentRunEvent`]s to [`RuntimeEvent`]s broadcast to clients.
///
/// Other commands are acknowledged via a `Status` event for now; the daemon is
/// intentionally minimal (no interactive approval/interaction routing yet). It
/// proves the end-to-end RPC path: a client submits a prompt over a socket and
/// receives the streamed turn.
pub async fn run_daemon(
    hub: Hub,
    mut commands: mpsc::UnboundedReceiver<RuntimeCommand>,
    provider: Option<Arc<dyn crate::llm::provider::LlmProvider>>,
    approval_mode: crate::tools::ApprovalMode,
) {
    while let Some(cmd) = commands.recv().await {
        match cmd {
            RuntimeCommand::SubmitPrompt { text } => {
                let (tx, mut rx) = mpsc::unbounded_channel();
                let hub_for_events = hub.clone();
                let pump = tokio::spawn(async move {
                    // `AgentRunEvent` is a type alias for `RuntimeEvent`, so the
                    // agent's events are already in the hub's wire form.
                    while let Some(ev) = rx.recv().await {
                        hub_for_events.publish(ev);
                    }
                });

                let request = crate::agent::AgentRequest {
                    prompt: text,
                    approval_mode,
                    provider: None,
                    model: None,
                    system_prompt: None,
                    events: false,
                    event_sender: Some(tx),
                    agent_depth: 0,
                    on_token_usage: None,
                    activity: None,
                    llm: provider.clone(),
                    session_sink: None,
                    tool_allowlist: None,
                    max_tokens: None,
                };

                match crate::agent::run_agent(request).await {
                    Ok(_resp) => {}
                    Err(e) => hub.publish(RuntimeEvent::Failed { message: e }),
                }
                let _ = pump.await;
            }
            RuntimeCommand::Cancel => {
                hub.publish(RuntimeEvent::Status {
                    message: "cancel requested".into(),
                });
            }
            other => {
                hub.publish(RuntimeEvent::Status {
                    message: format!("unhandled command: {other:?}"),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

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
        codec::write_message(&mut wc, &RuntimeCommand::SubmitPrompt { text: "hi".into() })
            .await
            .unwrap();

        let cmd = commands_rx.recv().await.unwrap();
        assert!(matches!(cmd, RuntimeCommand::SubmitPrompt { text } if text == "hi"));
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
}
