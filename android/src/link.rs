//! The phone client's daemon connection, run off the UI thread.
//!
//! Both targets produce the same newline-JSON stream handled by
//! [`bone_client::SocketConn`]: loopback TCP for local development, or
//! `ssh <host> -- bone stdio` ([`bone_client::ssh`]). On Android the `ssh`
//! program is replaced by an in-app SSH client; nothing past the byte stream
//! changes.

use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use bone_client::SocketConn;
use bone_client::ssh::SshSession;
use bone_protocol::{RuntimeCommand, RuntimeEvent};
use eframe::egui;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

/// How long an SSH login may take before the daemon's first event arrives.
const SSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the daemon lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Loopback `host:port`, for development beside a local daemon.
    Local(String),
    /// SSH destination whose `bone stdio` bridges to its loopback daemon.
    Ssh(String),
}

impl Target {
    pub fn label(&self) -> String {
        match self {
            Self::Local(address) => address.clone(),
            Self::Ssh(host) => format!("ssh: {host}"),
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
    pub fn open(target: Target, ctx: egui::Context) -> Self {
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
                Ok(runtime) => runtime.block_on(run(target, command_rx, &sink)),
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

async fn run(target: Target, mut commands: mpsc::UnboundedReceiver<RuntimeCommand>, sink: &Sink) {
    let (read, write, ssh): (Reader, Writer, Option<SshSession>) = match &target {
        Target::Local(address) => match tokio::net::TcpStream::connect(address).await {
            Ok(stream) => {
                let (read, write) = stream.into_split();
                (Box::new(read), Box::new(write), None)
            }
            Err(error) => {
                sink.send(LinkEvent::Disconnected(format!(
                    "could not connect to {address}: {error}"
                )));
                return;
            }
        },
        Target::Ssh(host) => match bone_client::ssh::ssh_connect(host) {
            Ok((read, write, session)) => (Box::new(read), Box::new(write), Some(session)),
            Err(error) => {
                sink.send(LinkEvent::Disconnected(format!(
                    "could not start ssh: {error}"
                )));
                return;
            }
        },
    };
    let mut conn = SocketConn::new(read, write);
    let sender = conn.command_sender();

    // `ssh` starts instantly even when its login will fail, so the link counts
    // as up only once the daemon's first event (sent unprompted on attach)
    // arrives; otherwise report ssh's own reason.
    if ssh.is_some() {
        match tokio::time::timeout(SSH_CONNECT_TIMEOUT, conn.next_event()).await {
            Ok(Some(event)) => {
                sink.send(LinkEvent::Connected);
                sink.send(LinkEvent::Runtime(event));
            }
            Ok(None) => {
                let reason = match ssh {
                    Some(session) => session.failure().await,
                    None => "ssh session ended".into(),
                };
                sink.send(LinkEvent::Disconnected(reason));
                return;
            }
            Err(_) => {
                sink.send(LinkEvent::Disconnected(format!(
                    "{} did not reach the daemon within {} seconds",
                    target.label(),
                    SSH_CONNECT_TIMEOUT.as_secs()
                )));
                return;
            }
        }
    } else {
        sink.send(LinkEvent::Connected);
    }

    let reason = loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => {
                    if sender.send(command).is_err() {
                        break "connection writer closed".to_string();
                    }
                }
                // The UI dropped the link.
                None => return,
            },
            event = conn.next_event() => match event {
                Some(event) => sink.send(LinkEvent::Runtime(event)),
                None => break "connection lost".to_string(),
            },
        }
    };
    drop(conn);
    let reason = match ssh {
        Some(session) => format!("{reason} ({})", session.failure().await),
        None => reason,
    };
    sink.send(LinkEvent::Disconnected(reason));
}
