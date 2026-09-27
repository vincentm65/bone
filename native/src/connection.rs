//! Socket lifecycle stays off the GUI thread. Every delivered event wakes egui.
//!
//! A chat reaches its daemon either over loopback TCP or through
//! `ssh <host> -- bone stdio` ([`bone_client::ssh`]); both yield the same
//! newline-JSON byte stream, so everything after the connect is shared.
use bone_client::SocketConn;
use bone_client::ssh::SshSession;
use bone_protocol::{RuntimeCommand, RuntimeEvent};
use eframe::egui;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

/// How long an SSH connect may take, login included, before the daemon's first
/// event arrives.
const SSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a chat's daemon lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A loopback `host:port`.
    Local(String),
    /// An SSH destination (`~/.ssh/config` alias or `user@host`) whose
    /// `bone stdio` bridges to that machine's loopback daemon.
    Ssh(String),
}

impl Target {
    /// Short label for the status bar and messages.
    pub fn label(&self) -> String {
        match self {
            Self::Local(address) => address.clone(),
            Self::Ssh(host) => format!("ssh: {host}"),
        }
    }
}

type Reader = Box<dyn AsyncRead + Unpin + Send>;
type Writer = Box<dyn AsyncWrite + Unpin + Send>;

/// Result of trying to open one connection.
enum Opened {
    Ready {
        read: Reader,
        write: Writer,
        ssh: Option<SshSession>,
    },
    Failed(String),
    Cancelled,
    /// The command channel closed: the tab is gone.
    Closed,
}

// Both are IPC message enums sent over a channel; `Box`ing the large runtime
// payloads would allocate per message and churn every match arm, so keep the
// payloads inline.
#[allow(clippy::large_enum_variant)]
pub enum Command {
    Connect(Target),
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
        let Command::Connect(target) = command else {
            continue;
        };
        let opened = match &target {
            Target::Local(address) => open_tcp(address, &mut commands).await,
            Target::Ssh(host) => open_ssh(host),
        };
        let (read, write, mut ssh) = match opened {
            Opened::Ready { read, write, ssh } => (read, write, ssh),
            Opened::Failed(reason) => {
                if !sink.send_control(Event::Disconnected(format!("Connect failed: {reason}"))) {
                    return;
                }
                ctx.request_repaint();
                continue;
            }
            Opened::Cancelled => {
                if !sink.send_control(Event::Disconnected("Connection cancelled".into())) {
                    return;
                }
                ctx.request_repaint();
                continue;
            }
            Opened::Closed => return,
        };
        let mut conn = SocketConn::new(read, write);
        let sender = conn.command_sender();
        // The ssh child starts instantly even when its login will fail, so an
        // SSH link counts as connected only once the daemon's first event
        // (sent unprompted on attach) arrives; otherwise report ssh's reason.
        let mut first = None;
        if let Some(session) = ssh.take() {
            let result = tokio::select! {
                result = tokio::time::timeout(SSH_CONNECT_TIMEOUT, conn.next_event()) => result,
                command = commands.recv() => {
                    if command.is_none() { return; }
                    if !sink.send_control(Event::Disconnected("Connection cancelled".into())) {
                        return;
                    }
                    ctx.request_repaint();
                    continue;
                }
            };
            let failure = match result {
                Ok(Some(event)) => {
                    first = Some(event);
                    ssh = Some(session);
                    None
                }
                // EOF: ssh exited before the daemon answered.
                Ok(None) => Some(session.failure().await),
                Err(_) => Some(format!(
                    "{} did not reach the daemon within {} seconds",
                    target.label(),
                    SSH_CONNECT_TIMEOUT.as_secs()
                )),
            };
            if let Some(reason) = failure {
                if !sink.send_control(Event::Disconnected(format!("Connect failed: {reason}"))) {
                    return;
                }
                ctx.request_repaint();
                continue;
            }
        }
        if !sink.send_control(Event::Connected) {
            return;
        }
        if let Some(event) = first
            && !sink.send_runtime(event)
        {
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
        let reason = match ssh.take() {
            Some(session) => format!("{reason} ({})", session.failure().await),
            None => reason.into(),
        };
        if !sink.send_control(Event::Disconnected(reason)) {
            return;
        }
        ctx.request_repaint();
    }
}

/// Try every loopback candidate (a `localhost` host may be IPv4 or IPv6) until
/// one connects; a refusal on one family must not hide a daemon on the other.
async fn open_tcp(address: &str, commands: &mut mpsc::UnboundedReceiver<Command>) -> Opened {
    let endpoints = match crate::daemon::local_endpoints(address) {
        Ok(endpoints) => endpoints,
        Err(reason) => return Opened::Failed(reason),
    };
    let mut last_error = String::new();
    for endpoint in endpoints {
        let result = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(endpoint)) => result,
            command = commands.recv() => {
                return if command.is_none() { Opened::Closed } else { Opened::Cancelled };
            }
        };
        match result {
            Ok(Ok(stream)) => {
                let (read, write) = stream.into_split();
                return Opened::Ready {
                    read: Box::new(read),
                    write: Box::new(write),
                    ssh: None,
                };
            }
            Ok(Err(error)) => last_error = error.to_string(),
            Err(_) => last_error = "connection timed out after 10 seconds".into(),
        }
    }
    Opened::Failed(last_error)
}

/// Start `ssh <host> -- bone stdio`; the worker confirms the link.
fn open_ssh(host: &str) -> Opened {
    match bone_client::ssh::ssh_connect(host) {
        Ok((read, write, session)) => Opened::Ready {
            read: Box::new(read),
            write: Box::new(write),
            ssh: Some(session),
        },
        Err(error) => Opened::Failed(format!("could not start ssh: {error}")),
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
        tx.send(Command::Connect(Target::Local(address))).unwrap();
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
        drop(tx);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }

    /// A stand-in `ssh` program: `script` runs in place of the remote
    /// `bone stdio`.
    #[cfg(unix)]
    fn fake_ssh(name: &str, script: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("bone-fake-ssh-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ssh");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// One test owns `BONE_SSH` so parallel tests never race on it.
    #[cfg(unix)]
    #[tokio::test]
    async fn ssh_target_connects_on_the_first_event_and_reports_ssh_failures() {
        let hello = serde_json::to_string(&RuntimeEvent::TextDelta {
            text: "hello".into(),
        })
        .unwrap();
        let ok = fake_ssh(
            "ok",
            &format!("printf '%s\\n' '{hello}'; exec cat >/dev/null"),
        );
        let denied = fake_ssh(
            "denied",
            "echo 'devbox: Permission denied (publickey).' >&2; exit 255",
        );
        let (tx, rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(256);
        let task = tokio::spawn(worker(rx, events, egui::Context::default()));
        let next = async |received: &mut mpsc::Receiver<Event>| {
            tokio::time::timeout(Duration::from_secs(5), received.recv())
                .await
                .unwrap()
        };

        // SAFETY: only this test reads or writes `BONE_SSH`.
        unsafe { std::env::set_var(bone_client::ssh::SSH_PROGRAM_ENV, &ok) };
        tx.send(Command::Connect(Target::Ssh("devbox".into())))
            .unwrap();
        assert!(matches!(next(&mut received).await, Some(Event::Connected)));
        assert!(matches!(
            next(&mut received).await,
            Some(Event::Runtime(RuntimeEvent::TextDelta { text })) if text == "hello"
        ));

        drop(tx);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();

        let (tx, rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(256);
        let task = tokio::spawn(worker(rx, events, egui::Context::default()));
        unsafe { std::env::set_var(bone_client::ssh::SSH_PROGRAM_ENV, &denied) };
        tx.send(Command::Connect(Target::Ssh("devbox".into())))
            .unwrap();
        match next(&mut received).await {
            Some(Event::Disconnected(reason)) => {
                assert!(reason.starts_with("Connect failed: ssh"), "{reason}");
                assert!(
                    reason.ends_with("Permission denied (publickey)."),
                    "{reason}"
                );
            }
            _ => panic!("expected a connect failure"),
        }
        unsafe { std::env::remove_var(bone_client::ssh::SSH_PROGRAM_ENV) };
        drop(tx);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        for path in [ok, denied] {
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }
}
