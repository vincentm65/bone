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

/// The persistent headless runtime: owns one [`RuntimeSession`] across turns and
/// drives each [`RuntimeCommand::SubmitPrompt`] to completion, broadcasting the
/// turn's [`RuntimeEvent`]s to every attached client.
///
/// Interaction (tool approval, `ctx.ui.key`) works over the wire: a turn runs
/// through a [`LocalConn`] on this task (the Lua VM is `!Send`, so the turn is
/// never spawned), while the daemon keeps reading the merged command stream and
/// routes `ApprovalReply` / `KeyReply` / `Cancel` into the connection. After the
/// turn, the session reabsorbs the outcome (transcript/token-stats/tool-state +
/// DB persistence) so the next turn — and any newly attached client — sees the
/// accumulated conversation. This is the server half of "the TUI is a client".
pub async fn run_daemon(
    hub: Hub,
    mut commands: mpsc::UnboundedReceiver<RuntimeCommand>,
    llm: Arc<dyn crate::llm::provider::LlmProvider>,
    extensions: crate::ext::ExtensionManager,
    mut session: crate::runtime::RuntimeSession,
    approval_mode: crate::tools::ApprovalMode,
) {
    use crate::runtime::{
        ApprovalReplyRegistry, ChannelApprovalGate, KeyReplyRegistry, LocalConn, RuntimeConn,
    };
    use std::sync::atomic::AtomicBool;

    let approval_registry = ApprovalReplyRegistry::new();
    let key_registry = KeyReplyRegistry::new();
    let mode = crate::tools::SharedApprovalMode::new(approval_mode);

    while let Some(cmd) = commands.recv().await {
        let RuntimeCommand::SubmitPrompt { text } = cmd else {
            // No turn is running; only a submit starts work. Acknowledge other
            // commands so a client isn't left wondering.
            if !matches!(cmd, RuntimeCommand::Cancel) {
                hub.publish(RuntimeEvent::Status {
                    message: format!("ignored (idle): {cmd:?}"),
                });
            }
            continue;
        };

        let (rt_tx, rt_rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        let cancel = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(ChannelApprovalGate::new(
            rt_tx.clone(),
            approval_registry.clone(),
        ));
        let persist_from = session.transcript.len();
        let driver = session.build_driver(
            llm.clone(),
            extensions.clone(),
            mode.clone(),
            gate,
            rt_tx,
            key_registry.clone(),
            cancel.clone(),
            Arc::new(crate::session_sink::NullSessionSink),
        );
        let mut conn = LocalConn::new(
            rt_rx,
            driver,
            cancel,
            approval_registry.clone(),
            key_registry.clone(),
        );
        conn.send(RuntimeCommand::SubmitPrompt { text });

        // Pump the turn: publish its events, and concurrently route interactive
        // replies (and cancel) from any client back into the running turn.
        loop {
            tokio::select! {
                ev = conn.next_event() => match ev {
                    Some(ev) => hub.publish(ev),
                    None => break, // turn drained
                },
                cmd = commands.recv() => match cmd {
                    Some(cmd @ (RuntimeCommand::ApprovalReply { .. }
                    | RuntimeCommand::KeyReply { .. }
                    | RuntimeCommand::Cancel)) => conn.send(cmd),
                    // A second submit mid-turn is dropped (the runtime is busy
                    // running one turn at a time). Tell the client so it isn't
                    // left waiting on a prompt that will never run, mirroring the
                    // idle-path acknowledgement above.
                    Some(RuntimeCommand::SubmitPrompt { .. }) => hub.publish(RuntimeEvent::Status {
                        message: "busy: a turn is in progress; prompt ignored".into(),
                    }),
                    Some(_) => {}
                    None => break,
                },
            }
        }

        if let Some(outcome) = conn.take_outcome() {
            let _ = session.apply_outcome(outcome, persist_from);
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
