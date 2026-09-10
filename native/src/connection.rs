//! Socket lifecycle stays off the GUI thread. Every delivered event wakes egui.
use bone_client::SocketConn;
use bone_protocol::{RuntimeCommand, RuntimeEvent};
use eframe::egui;
use std::time::Duration;
use tokio::sync::mpsc;

// Both are IPC message enums sent over a channel; `Box`ing the large runtime
// payloads would allocate per message and churn every match arm, so keep the
// payloads inline.
#[allow(clippy::large_enum_variant)]
pub enum Command {
    Connect(String),
    Disconnect,
    Send(RuntimeCommand),
}
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Connected,
    Disconnected(String),
    Runtime(RuntimeEvent),
}

/// The GUI event queue must not be allowed to stall the socket worker.  A
/// stalled worker cannot service cancellation or disconnect commands, and a
/// long-running stream can otherwise leave the tab looking busy forever.
///
/// Runtime events are lossy by design: once the local queue is full, we drop
/// them and enqueue a synthetic `StreamLagged` marker when space becomes
/// available.  The state reducer responds to that marker with an authoritative
/// `Synchronize`, so the dropped deltas do not become permanent state loss.
/// One queue slot is reserved for lifecycle events (`Connected` and
/// `Disconnected`) so those events remain deliverable during a flood.
struct EventSink {
    events: mpsc::Sender<Event>,
    dropped_runtime: u64,
}

impl EventSink {
    const CONTROL_RESERVE: usize = 1;

    fn new(events: mpsc::Sender<Event>) -> Self {
        Self {
            events,
            dropped_runtime: 0,
        }
    }

    fn send_control(&self, event: Event) -> bool {
        // Runtime events always leave one slot free, making this non-blocking
        // send reliable for the worker's lifecycle events.
        self.events.try_send(event).is_ok()
    }

    fn send_runtime(&mut self, event: RuntimeEvent) -> bool {
        if !self.flush_lag() {
            return false;
        }

        if self.events.capacity() <= Self::CONTROL_RESERVE {
            self.dropped_runtime = self.dropped_runtime.saturating_add(1);
            return true;
        }

        match self.events.try_send(Event::Runtime(event)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_runtime = self.dropped_runtime.saturating_add(1);
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    fn flush_lag(&mut self) -> bool {
        if self.dropped_runtime == 0 {
            return true;
        }
        if self.events.capacity() <= Self::CONTROL_RESERVE {
            return true;
        }
        let marker = Event::Runtime(RuntimeEvent::StreamLagged {
            skipped: self.dropped_runtime,
        });
        match self.events.try_send(marker) {
            Ok(()) => {
                self.dropped_runtime = 0;
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => true,
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

pub fn spawn(ctx: egui::Context) -> (mpsc::UnboundedSender<Command>, mpsc::Receiver<Event>) {
    let (commands, rx) = mpsc::unbounded_channel();
    let (tx, events) = mpsc::channel(256);
    // A small current-thread runtime per connection: the worker is one select
    // loop, and multi-conversation tabs must not multiply OS thread pools.
    std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(worker(rx, tx, ctx)),
            Err(error) => {
                let _ = tx.blocking_send(Event::Disconnected(format!(
                    "Runtime initialization failed: {error}"
                )));
                ctx.request_repaint();
            }
        }
    });
    (commands, events)
}

async fn worker(
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::Sender<Event>,
    ctx: egui::Context,
) {
    let mut sink = EventSink::new(events);
    while let Some(command) = commands.recv().await {
        let Command::Connect(address) = command else {
            continue;
        };
        let endpoints = match crate::daemon::local_endpoints(&address) {
            Ok(endpoints) => endpoints,
            Err(reason) => {
                if !sink.send_control(Event::Disconnected(format!("Connect failed: {reason}"))) {
                    return;
                }
                ctx.request_repaint();
                continue;
            }
        };
        // Try every loopback candidate (a `localhost` host may be IPv4 or IPv6)
        // until one connects; a refusal on one family must not hide a daemon on
        // the other.
        let mut stream = None;
        let mut last_error = String::new();
        let mut cancelled = false;
        for endpoint in endpoints {
            let result = tokio::select! {
                result = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(endpoint)) => result,
                command = commands.recv() => {
                    if command.is_none() { return; }
                    cancelled = true;
                    break;
                }
            };
            match result {
                Ok(Ok(connected)) => {
                    stream = Some(connected);
                    break;
                }
                Ok(Err(error)) => last_error = error.to_string(),
                Err(_) => last_error = "connection timed out after 10 seconds".into(),
            }
        }
        if cancelled {
            if !sink.send_control(Event::Disconnected("Connection cancelled".into())) {
                return;
            }
            ctx.request_repaint();
            continue;
        }
        let Some(stream) = stream else {
            if !sink.send_control(Event::Disconnected(format!("Connect failed: {last_error}"))) {
                return;
            }
            ctx.request_repaint();
            continue;
        };
        let (read, write) = stream.into_split();
        let mut conn = SocketConn::new(read, write);
        let sender = conn.command_sender();
        if !sink.send_control(Event::Connected) {
            return;
        }
        ctx.request_repaint();
        let mut lag_flush = tokio::time::interval(Duration::from_millis(16));
        let reason = loop {
            tokio::select! {
                biased;
                command = commands.recv() => match command {
                    Some(Command::Send(command)) => {
                        if sender.send(command).is_err() { break "Connection writer closed; delivery may be uncertain."; }
                    }
                    Some(Command::Disconnect) => break "Disconnected",
                    Some(Command::Connect(_)) => {}, // UI disables Connect while attached
                    None => return,
                },
                event = conn.next_event() => match event {
                    Some(event) => {
                        if !sink.send_runtime(event) { return; }
                        ctx.request_repaint();
                    },
                    None => break "Connection lost. Delivery may be uncertain; reconnect manually. Prompts are never resent automatically.",
                },
                _ = lag_flush.tick() => if !sink.flush_lag() { return; },
            }
        };
        drop(conn);
        if !sink.send_control(Event::Disconnected(reason.into())) {
            return;
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_flood_reserves_lifecycle_slot_and_reports_lag() {
        let (events, mut received) = mpsc::channel(3);
        let mut sink = EventSink::new(events);

        assert!(sink.send_runtime(RuntimeEvent::TextDelta { text: "one".into() }));
        assert!(sink.send_runtime(RuntimeEvent::TextDelta { text: "two".into() }));
        assert!(sink.send_runtime(RuntimeEvent::TextDelta {
            text: "dropped".into(),
        }));
        assert_eq!(received.len(), 2);

        assert!(matches!(
            received.try_recv(),
            Ok(Event::Runtime(RuntimeEvent::TextDelta { text })) if text == "one"
        ));
        // A drained slot lets the sink publish the repair marker before more
        // deltas, while the reserved slot remains available for disconnect.
        assert!(sink.flush_lag());
        assert!(sink.send_control(Event::Disconnected("done".into())));
        assert!(matches!(
            received.try_recv(),
            Ok(Event::Runtime(RuntimeEvent::TextDelta { text })) if text == "two"
        ));
        assert!(matches!(
            received.try_recv(),
            Ok(Event::Runtime(RuntimeEvent::StreamLagged { skipped: 1 }))
        ));
        assert!(matches!(received.try_recv(), Ok(Event::Disconnected(_))));
    }

    #[tokio::test]
    async fn loopback_commands_events_and_disconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(256);
        let task = tokio::spawn(worker(rx, events, egui::Context::default()));
        tx.send(Command::Connect(address)).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        assert!(matches!(received.recv().await, Some(Event::Connected)));
        bone_client::write_message(
            &mut write,
            &RuntimeEvent::TextDelta {
                text: "hello".into(),
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(received.recv().await, Some(Event::Runtime(RuntimeEvent::TextDelta { text })) if text == "hello")
        );
        tx.send(Command::Send(RuntimeCommand::Cancel)).unwrap();
        let mut reader = bone_client::MessageReader::new(read);
        assert!(matches!(
            reader.read::<RuntimeCommand>().await.unwrap(),
            Ok(RuntimeCommand::Cancel)
        ));
        tx.send(Command::Disconnect).unwrap();
        assert!(matches!(
            received.recv().await,
            Some(Event::Disconnected(_))
        ));
        assert!(
            tokio::time::timeout(Duration::from_secs(2), reader.read::<RuntimeCommand>())
                .await
                .unwrap()
                .is_none()
        );
        drop(tx);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn disconnect_is_not_blocked_by_a_runtime_event_flood() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(256);
        let task = tokio::spawn(worker(rx, events, egui::Context::default()));
        tx.send(Command::Connect(address)).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let (_read, mut write) = stream.into_split();

        // The test receiver intentionally does not drain the GUI queue. Once
        // the reserved-capacity threshold is reached, additional runtime
        // events are dropped while the worker remains able to read commands.
        for index in 0..512 {
            bone_client::write_message(
                &mut write,
                &RuntimeEvent::TextDelta {
                    text: index.to_string(),
                },
            )
            .await
            .unwrap();
        }
        tx.send(Command::Disconnect).unwrap();
        drop(tx);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("disconnect must interrupt event delivery")
            .unwrap();

        // The lifecycle event occupies the reserved queue slot and remains
        // available to the GUI even though it never drained the flood.
        assert!(received.len() >= 2);
        assert!(matches!(received.try_recv(), Ok(Event::Connected)));
        assert!(
            std::iter::from_fn(|| received.try_recv().ok())
                .any(|event| matches!(event, Event::Disconnected(_)))
        );
    }
}
