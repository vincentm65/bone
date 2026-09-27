//! The phone client's daemon connection, run off the UI thread.
//!
//! Both targets produce the same newline-JSON stream handled by
//! [`bone_client::SocketConn`]: loopback TCP for local development, or an
//! in-app SSH channel running `bone stdio` on the computer ([`crate::ssh`]).

use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use bone_client::SocketConn;
use bone_protocol::{RuntimeCommand, RuntimeEvent};
use eframe::egui;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::ssh::{Destination, Identity};

/// How long `bone stdio` may take to answer once the SSH login succeeds.
const DAEMON_ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the daemon lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Loopback `host:port`, for development beside a local daemon.
    Local(String),
    /// SSH login whose `<bone> stdio` bridges to that machine's daemon.
    Ssh {
        destination: Destination,
        /// Path of `bone` on the computer (`bone` when it is on the PATH
        /// that SSH commands see).
        bone: String,
    },
}

impl Target {
    pub fn label(&self) -> String {
        match self {
            Self::Local(address) => address.clone(),
            Self::Ssh { destination, .. } => format!("ssh: {}", destination.host),
        }
    }
}

/// What the connection reports to the UI. Kept unboxed like the desktop's
/// channel enums: boxing would allocate per streamed event.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum LinkEvent {
    Connected,
    Disconnected(String),
    Runtime(RuntimeEvent),
}

/// A live connection attempt. Dropping it closes the connection.
pub struct Link {
    commands: mpsc::UnboundedSender<RuntimeCommand>,
    events: std_mpsc::Receiver<LinkEvent>,
}

impl Link {
    /// Connect on a background thread; every event wakes `ctx`.
    pub fn open(target: Target, identity: Arc<Identity>, ctx: egui::Context) -> Self {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (event_tx, events) = std_mpsc::channel();
        std::thread::spawn(move || {
            let sink = Sink {
                events: event_tx,
                ctx,
            };
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(run(target, &identity, command_rx, &sink)),
                Err(error) => sink.send(LinkEvent::Disconnected(format!(
                    "could not start the connection runtime: {error}"
                ))),
            }
        });
        Self { commands, events }
    }

    /// Queue a command; `false` once the connection has ended.
    pub fn send(&self, command: RuntimeCommand) -> bool {
        self.commands.send(command).is_ok()
    }

    /// Events received since the last call.
    pub fn drain(&self) -> Vec<LinkEvent> {
        self.events.try_iter().collect()
    }
}

struct Sink {
    events: std_mpsc::Sender<LinkEvent>,
    ctx: egui::Context,
}

impl Sink {
    fn send(&self, event: LinkEvent) {
        let _ = self.events.send(event);
        self.ctx.request_repaint();
    }
}

type Reader = Box<dyn AsyncRead + Unpin + Send>;
type Writer = Box<dyn AsyncWrite + Unpin + Send>;

async fn run(
    target: Target,
    identity: &Identity,
    mut commands: mpsc::UnboundedReceiver<RuntimeCommand>,
    sink: &Sink,
) {
    // Held for the life of the connection: dropping it closes the SSH session.
    let mut _session = None;
    let (read, write): (Reader, Writer) = match &target {
        Target::Local(address) => match tokio::net::TcpStream::connect(address).await {
            Ok(stream) => {
                let (read, write) = stream.into_split();
                (Box::new(read), Box::new(write))
            }
            Err(error) => {
                sink.send(LinkEvent::Disconnected(format!(
                    "could not connect to {address}: {error}"
                )));
                return;
            }
        },
        Target::Ssh { destination, bone } => {
            match crate::ssh::open(identity, destination, bone).await {
                Ok((stream, session)) => {
                    _session = Some(session);
                    let (read, write) = tokio::io::split(stream);
                    (Box::new(read), Box::new(write))
                }
                Err(reason) => {
                    sink.send(LinkEvent::Disconnected(reason));
                    return;
                }
            }
        }
    };
    let mut conn = SocketConn::new(read, write);
    let sender = conn.command_sender();

    // Count the link as up once the daemon's first event (sent unprompted on
    // attach) arrives, so a missing `bone` on the computer shows as an error.
    match tokio::time::timeout(DAEMON_ANSWER_TIMEOUT, conn.next_event()).await {
        Ok(Some(event)) => {
            sink.send(LinkEvent::Connected);
            sink.send(LinkEvent::Runtime(event));
        }
        Ok(None) => {
            let reason = match &target {
                Target::Ssh { bone, .. } => format!(
                    "`{bone} stdio` exited without answering; is bone installed at that path on the computer?"
                ),
                Target::Local(address) => format!("{address} closed the connection"),
            };
            sink.send(LinkEvent::Disconnected(reason));
            return;
        }
        Err(_) => {
            sink.send(LinkEvent::Disconnected(format!(
                "{} did not answer within {} seconds",
                target.label(),
                DAEMON_ANSWER_TIMEOUT.as_secs()
            )));
            return;
        }
    }

    let reason = loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => {
                    if sender.send(command).is_err() {
                        break "connection writer closed";
                    }
                }
                // The UI dropped the link.
                None => return,
            },
            event = conn.next_event() => match event {
                Some(event) => sink.send(LinkEvent::Runtime(event)),
                None => break "connection lost",
            },
        }
    };
    sink.send(LinkEvent::Disconnected(reason.into()));
}
