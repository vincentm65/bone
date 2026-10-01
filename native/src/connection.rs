//! Socket lifecycle stays off the GUI thread. Every delivered event wakes egui.
//!
//! A chat reaches its daemon over loopback TCP, through
//! `ssh <host> -- bone stdio` ([`bone_client::ssh`]), or through an embedder's
//! [`Connector`] (the Android app's in-app SSH). All yield the same
//! newline-JSON byte stream, so everything after the connect is shared.
use bone_client::SocketConn;
use bone_client::ssh::SshSession;
use bone_protocol::{RuntimeCommand, RuntimeEvent};
use eframe::egui;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

/// How long a remote connect may take, login included, before the daemon's
/// first event arrives.
const SSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a local connect may wait for the daemon's first event after the
/// TCP connection opens. A local daemon that accepts TCP but never answers is
/// wedged; the coordinator respawns one it started itself.
const LOCAL_FIRST_EVENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Where a chat's daemon lives.
#[derive(Clone)]
pub enum Target {
    /// A loopback `host:port`.
    Local(String),
    /// An SSH destination (`~/.ssh/config` alias or `user@host`) whose
    /// `bone stdio` bridges to that machine's loopback daemon.
    Ssh(String),
    /// A stream opened by the embedding app.
    Custom(Arc<dyn Connector>),
}

impl Target {
    /// Short label for the status bar and messages.
    pub fn label(&self) -> String {
        match self {
            Self::Local(address) => address.clone(),
            Self::Ssh(host) => format!("ssh: {host}"),
            Self::Custom(connector) => connector.label(),
        }
    }

    /// Whether the daemon runs on another machine (never auto-started here).
    pub fn is_remote(&self) -> bool {
        !matches!(self, Self::Local(_))
    }
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

pub type Reader = Box<dyn AsyncRead + Unpin + Send>;
pub type Writer = Box<dyn AsyncWrite + Unpin + Send>;

/// An open daemon byte stream from a [`Connector`]; `guard` (e.g. the SSH
/// session) is kept alive for as long as the stream is used.
pub struct Stream {
    pub read: Reader,
    pub write: Writer,
    pub guard: Box<dyn Send>,
    /// Why the far end went away (e.g. remote stderr or exit status), asked
    /// once the stream has closed; `None` when there is nothing to add.
    pub failure: Option<Failure>,
}

pub type Failure = Box<dyn Fn() -> Option<String> + Send>;

/// Opens the newline-JSON daemon stream for [`Target::Custom`]. The link
/// counts as connected once the daemon's first event arrives.
pub trait Connector: Send + Sync {
    fn label(&self) -> String;
    fn connect(&self) -> Pin<Box<dyn Future<Output = Result<Stream, String>> + Send>>;
}

/// Result of trying to open one connection; short-lived, so kept unboxed.
#[allow(clippy::large_enum_variant)]
enum Opened {
    Ready {
        read: Reader,
        write: Writer,
        ssh: Option<SshSession>,
        /// A custom connector's guard; also marks a link to confirm.
        guard: Option<Box<dyn Send>>,
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
/// Uncorrelated runtime events are lossy by design: once the local queue is
/// full, we drop them and enqueue a synthetic `StreamLagged` marker when space
/// becomes available.  The state reducer responds to that marker with an
/// authoritative `Synchronize`, so the dropped deltas do not become permanent
/// state loss.  Replies correlated with a request (see [`is_correlated`]) are
/// never repaired by a resync, so they are held and delivered in order ahead
/// of the marker and any later runtime event instead of being dropped.
/// One queue slot is reserved for lifecycle events (`Connected` and
/// `Disconnected`) so those events remain deliverable during a flood.
struct EventSink {
    events: mpsc::Sender<Event>,
    dropped_runtime: u64,
    /// Correlated replies that arrived while the queue was full.
    deferred: VecDeque<RuntimeEvent>,
}

/// Replies a client waits on by request id; `Synchronize` cannot replay them.
fn is_correlated(event: &RuntimeEvent) -> bool {
    matches!(
        event,
        RuntimeEvent::StateSynchronized { .. }
            | RuntimeEvent::HostResponse { .. }
            | RuntimeEvent::OlderMessagesLoaded { .. }
            | RuntimeEvent::TurnCompleted { .. }
            | RuntimeEvent::Started {
                request_id: Some(_),
                ..
            }
            | RuntimeEvent::CommandComplete {
                request_id: Some(_),
                ..
            }
            | RuntimeEvent::KeymapDispatched {
                request_id: Some(_),
                ..
            }
            | RuntimeEvent::ConfigChanged {
                request_id: Some(_),
                ..
            }
            | RuntimeEvent::ConfigMutationRejected {
                request_id: Some(_),
                ..
            }
    )
}

impl EventSink {
    const CONTROL_RESERVE: usize = 1;

    fn new(events: mpsc::Sender<Event>) -> Self {
        Self {
            events,
            dropped_runtime: 0,
            deferred: VecDeque::new(),
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

        // Held replies go first, so later events queue behind them.
        if !self.deferred.is_empty() || self.events.capacity() <= Self::CONTROL_RESERVE {
            self.hold(event);
            return true;
        }

        match self.events.try_send(Event::Runtime(event)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(event)) => {
                if let Event::Runtime(event) = event {
                    self.hold(event);
                }
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// Keep a correlated reply for later delivery; drop anything else as lag.
    /// Replies are never dropped: each answers one request, so the backlog
    /// stays small, and a lost reply would leave its waiter hanging.
    fn hold(&mut self, event: RuntimeEvent) {
        if is_correlated(&event) {
            self.deferred.push_back(event);
        } else {
            self.dropped_runtime = self.dropped_runtime.saturating_add(1);
        }
    }

    /// Deliver held replies, then the lag marker, as space allows.
    fn flush_lag(&mut self) -> bool {
        while self.events.capacity() > Self::CONTROL_RESERVE {
            let Some(event) = self.deferred.pop_front() else {
                break;
            };
            match self.events.try_send(Event::Runtime(event)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(event)) => {
                    if let Event::Runtime(event) = event {
                        self.deferred.push_front(event);
                    }
                    return true;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return false,
            }
        }
        if !self.deferred.is_empty()
            || self.dropped_runtime == 0
            || self.events.capacity() <= Self::CONTROL_RESERVE
        {
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

/// Outcome of waiting for the daemon's first attach event.
enum FirstEvent {
    /// The daemon's first event (sent unprompted on attach).
    Event(RuntimeEvent),
    /// The stream closed before the daemon answered.
    Closed,
    /// The first-event deadline elapsed: the daemon is wedged.
    TimedOut,
    /// A connect command arrived: cancel this attempt.
    Cancelled,
    /// The command channel closed: the tab is gone.
    Stopped,
}

/// Wait for the daemon's first event within `timeout`. Every target uses this:
/// the ssh child starts instantly even when its login will fail, a remote
/// `bone stdio` may be missing, and a local daemon that accepts TCP but never
/// answers is wedged.
async fn wait_first_event<R>(
    conn: &mut SocketConn<R>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    timeout: Duration,
) -> FirstEvent
where
    R: AsyncRead + Unpin,
{
    tokio::select! {
        result = tokio::time::timeout(timeout, conn.next_event()) => match result {
            Ok(Some(event)) => FirstEvent::Event(event),
            Ok(None) => FirstEvent::Closed,
            Err(_) => FirstEvent::TimedOut,
        },
        command = commands.recv() => {
            if command.is_none() { FirstEvent::Stopped } else { FirstEvent::Cancelled }
        }
    }
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
        let mut custom_failure: Option<Failure> = None;
        let opened = match &target {
            Target::Local(address) => open_tcp(address, &mut commands).await,
            Target::Ssh(host) => open_ssh(host),
            Target::Custom(connector) => tokio::select! {
                result = connector.connect() => match result {
                    Ok(stream) => {
                        custom_failure = stream.failure;
                        Opened::Ready {
                            read: stream.read,
                            write: stream.write,
                            ssh: None,
                            guard: Some(stream.guard),
                        }
                    }
                    Err(reason) => Opened::Failed(reason),
                },
                command = commands.recv() => {
                    if command.is_none() { Opened::Closed } else { Opened::Cancelled }
                }
            },
        };
        let (read, write, mut ssh, _guard) = match opened {
            Opened::Ready {
                read,
                write,
                ssh,
                guard,
            } => (read, write, ssh, guard),
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
        let first_timeout = if target.is_remote() {
            SSH_CONNECT_TIMEOUT
        } else {
            LOCAL_FIRST_EVENT_TIMEOUT
        };
        let first = match wait_first_event(&mut conn, &mut commands, first_timeout).await {
            FirstEvent::Event(event) => event,
            FirstEvent::Cancelled => {
                if !sink.send_control(Event::Disconnected("Connection cancelled".into())) {
                    return;
                }
                ctx.request_repaint();
                continue;
            }
            FirstEvent::Stopped => return,
            FirstEvent::Closed => {
                let reason = match ssh.take() {
                    Some(session) => session.failure().await,
                    None => {
                        let closed =
                            format!("{} closed before the daemon answered", target.label());
                        match custom_failure.as_ref().and_then(|failure| failure()) {
                            Some(detail) => format!("{closed}: {detail}"),
                            None if target.is_remote() => {
                                format!("{closed}; is `bone stdio` available there?")
                            }
                            None => closed,
                        }
                    }
                };
                if !sink.send_control(Event::Disconnected(format!("Connect failed: {reason}"))) {
                    return;
                }
                ctx.request_repaint();
                continue;
            }
            FirstEvent::TimedOut => {
                let reason = format!(
                    "{} did not reach the daemon within {} seconds",
                    target.label(),
                    first_timeout.as_secs()
                );
                if !sink.send_control(Event::Disconnected(format!("Connect failed: {reason}"))) {
                    return;
                }
                ctx.request_repaint();
                continue;
            }
        };
        if !sink.send_control(Event::Connected) {
            return;
        }
        if !sink.send_runtime(first) {
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
        let detail = custom_failure.as_ref().and_then(|failure| failure());
        let reason = match (ssh.take(), detail) {
            (Some(session), _) => format!("{reason} ({})", session.failure().await),
            (None, Some(detail)) => format!("{reason} ({detail})"),
            (None, None) => reason.into(),
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
                    guard: None,
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
            guard: None,
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

    #[test]
    fn correlated_replies_survive_a_full_queue() {
        let (events, mut received) = mpsc::channel(3);
        let mut sink = EventSink::new(events);

        assert!(sink.send_runtime(RuntimeEvent::TextDelta { text: "one".into() }));
        assert!(sink.send_runtime(RuntimeEvent::TextDelta { text: "two".into() }));
        assert!(sink.send_runtime(RuntimeEvent::TurnCompleted { request_id: 7 }));
        assert!(sink.send_runtime(RuntimeEvent::TextDelta {
            text: "dropped".into(),
        }));
        assert_eq!(received.len(), 2);
        assert!(matches!(
            received.try_recv(),
            Ok(Event::Runtime(RuntimeEvent::TextDelta { text })) if text == "one"
        ));
        assert!(matches!(
            received.try_recv(),
            Ok(Event::Runtime(RuntimeEvent::TextDelta { text })) if text == "two"
        ));

        // The held reply is delivered first; the lag marker follows as space
        // allows, never consuming the lifecycle slot.
        assert!(sink.flush_lag());
        assert!(matches!(
            received.try_recv(),
            Ok(Event::Runtime(RuntimeEvent::TurnCompleted {
                request_id: 7
            }))
        ));
        assert!(sink.flush_lag());
        assert!(matches!(
            received.try_recv(),
            Ok(Event::Runtime(RuntimeEvent::StreamLagged { skipped: 1 }))
        ));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_long_burst_of_replies_is_never_dropped() {
        let (events, mut received) = mpsc::channel(3);
        let mut sink = EventSink::new(events);
        for request_id in 0..200 {
            assert!(sink.send_runtime(RuntimeEvent::TurnCompleted { request_id }));
        }
        let mut delivered = Vec::new();
        while delivered.len() < 200 {
            assert!(sink.flush_lag());
            while let Ok(Event::Runtime(RuntimeEvent::TurnCompleted { request_id })) =
                received.try_recv()
            {
                delivered.push(request_id);
            }
        }
        assert_eq!(delivered, (0..200).collect::<Vec<_>>());
        assert!(received.try_recv().is_err());
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
        // The worker confirms the link on the daemon's first event, so the
        // fake daemon must speak before `Connected` is expected.
        bone_client::write_message(
            &mut write,
            &RuntimeEvent::TextDelta {
                text: "hello".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(received.recv().await, Some(Event::Connected)));
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

    /// Hands out one in-memory stream, like the Android app's SSH connector.
    struct FakeConnector(std::sync::Mutex<Option<tokio::io::DuplexStream>>);

    impl Connector for FakeConnector {
        fn label(&self) -> String {
            "ssh: phone-test".into()
        }

        fn connect(&self) -> Pin<Box<dyn Future<Output = Result<Stream, String>> + Send>> {
            let stream = self.0.lock().unwrap().take();
            Box::pin(async move {
                let (read, write) = tokio::io::split(stream.ok_or("already used")?);
                Ok(Stream {
                    read: Box::new(read),
                    write: Box::new(write),
                    guard: Box::new(()),
                    failure: Some(Box::new(|| {
                        Some("remote bone exited with status 127".into())
                    })),
                })
            })
        }
    }

    #[tokio::test]
    async fn custom_connectors_confirm_on_the_first_event_or_explain_an_early_close() {
        let next = async |received: &mut mpsc::Receiver<Event>| {
            tokio::time::timeout(Duration::from_secs(5), received.recv())
                .await
                .unwrap()
        };

        let (app_side, daemon_side) = tokio::io::duplex(4096);
        let (tx, rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(256);
        let task = tokio::spawn(worker(rx, events, egui::Context::default()));
        let connector = Arc::new(FakeConnector(std::sync::Mutex::new(Some(app_side))));
        tx.send(Command::Connect(Target::Custom(connector)))
            .unwrap();
        let (_daemon_read, mut daemon_write) = tokio::io::split(daemon_side);
        bone_client::write_message(
            &mut daemon_write,
            &RuntimeEvent::TextDelta { text: "hi".into() },
        )
        .await
        .unwrap();
        assert!(matches!(next(&mut received).await, Some(Event::Connected)));
        assert!(matches!(
            next(&mut received).await,
            Some(Event::Runtime(RuntimeEvent::TextDelta { text })) if text == "hi"
        ));
        drop(tx);
        task.await.unwrap();

        let (app_side, daemon_side) = tokio::io::duplex(4096);
        drop(daemon_side);
        let (tx, rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(256);
        let task = tokio::spawn(worker(rx, events, egui::Context::default()));
        let connector = Arc::new(FakeConnector(std::sync::Mutex::new(Some(app_side))));
        tx.send(Command::Connect(Target::Custom(connector)))
            .unwrap();
        match next(&mut received).await {
            Some(Event::Disconnected(reason)) => assert!(
                reason.starts_with(
                    "Connect failed: ssh: phone-test closed before the daemon answered: \
                     remote bone exited with status 127"
                ),
                "{reason}"
            ),
            _ => panic!("expected a connect failure"),
        }
        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn wait_first_event_times_out_against_a_silent_peer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (tx, mut commands) = mpsc::unbounded_channel();
        let (stream, _server) = tokio::join!(
            tokio::net::TcpStream::connect(&address),
            listener.accept()
        );
        let (read, write) = stream.unwrap().into_split();
        let read: Reader = Box::new(read);
        let write: Writer = Box::new(write);
        let mut conn = SocketConn::new(read, write);
        let first =
            wait_first_event(&mut conn, &mut commands, Duration::from_secs(2)).await;
        assert!(matches!(first, FirstEvent::TimedOut));
        drop(tx);
    }

    #[tokio::test]
    async fn wait_first_event_cancels_when_a_new_connect_arrives() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (tx, mut commands) = mpsc::unbounded_channel();
        let (stream, _server) = tokio::join!(
            tokio::net::TcpStream::connect(&address),
            listener.accept()
        );
        let (read, write) = stream.unwrap().into_split();
        let read: Reader = Box::new(read);
        let write: Writer = Box::new(write);
        let mut conn = SocketConn::new(read, write);
        let notifier = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            notifier
                .send(Command::Connect(Target::Local(address)))
                .unwrap();
        });
        let first =
            wait_first_event(&mut conn, &mut commands, Duration::from_secs(5)).await;
        assert!(matches!(first, FirstEvent::Cancelled));
    }
}
