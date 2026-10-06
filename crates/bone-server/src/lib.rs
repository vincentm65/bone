//! Hosts a [`Core`] and serves it to clients over any [`Connection`].
//!
//! Per connection the server enforces the `initialize` handshake, routes
//! requests to the core, and forwards core events as notifications. After the
//! handshake each request runs on its own task, so a slow one (a health check
//! waiting on the network) never holds up the others; replies carry their
//! request's id and may come back in any order.
//! Connections come from [`Server::connect_in_process`], [`Server::serve_stdio`]
//! or a Unix socket [`Listener`].

mod unix;

use std::sync::Arc;

use bone_core::{Core, Event, dispatch};
use bone_proto::methods::{
    Initialize, InitializeParams, InitializeResult, SessionUpdated, SessionUpdatedParams, Shutdown,
};
use bone_proto::{Connection, Message, Method, PROTOCOL_VERSION, RequestId, RpcError, transport};
use serde_json::{Value, json};
use tokio::sync::broadcast;

pub use unix::Listener;

pub const SERVER_NAME: &str = "bone";

#[derive(Clone)]
pub struct Server {
    core: Arc<Core>,
}

impl Server {
    pub fn new(core: Arc<Core>) -> Self {
        Server { core }
    }

    /// Start serving an in-process connection on a background task and return
    /// the client end.
    pub fn connect_in_process(&self) -> Connection {
        let (client, server) = transport::in_process();
        let this = self.clone();
        tokio::spawn(async move { this.serve(server).await });
        client
    }

    /// Serve one client over this process's stdin/stdout (like `nvim --embed`).
    /// Nothing else may write to stdout meanwhile.
    pub async fn serve_stdio(&self) {
        let (conn, writer) = transport::framed_with_writer(tokio::io::stdin(), tokio::io::stdout());
        self.serve(conn).await;
        // The process exits right after this; make sure the last replies
        // (e.g. to `shutdown`) have reached stdout.
        let _ = writer.await;
    }

    /// Serve one connection until the client disconnects or sends `shutdown`.
    pub async fn serve(&self, conn: Connection) {
        let Connection { tx, mut rx } = conn;
        let mut events: Option<broadcast::Receiver<Event>> = None;

        loop {
            let msg = tokio::select! {
                msg = rx.recv() => match msg {
                    Some(msg) => msg,
                    None => return,
                },
                event = recv_event(&mut events) => {
                    let note = Message::Notification {
                        method: event.method.to_owned(),
                        params: Some(event.params),
                    };
                    if tx.send(note).await.is_err() {
                        return;
                    }
                    continue;
                }
            };

            // Clients send no notifications or responses yet; ignore them.
            let Message::Request { id, method, params } = msg else {
                continue;
            };

            let initialized = events.is_some();
            let result = match method.as_str() {
                Initialize::METHOD if initialized => Err(RpcError::new(
                    RpcError::INVALID_REQUEST,
                    "connection already initialized",
                )),
                Initialize::METHOD => {
                    let result = dispatch::<Initialize, _>(params, initialize);
                    if result.is_ok() {
                        events = Some(self.core.subscribe());
                    }
                    result
                }
                _ if !initialized => Err(RpcError::new(
                    RpcError::NOT_INITIALIZED,
                    "first request must be initialize",
                )),
                Shutdown::METHOD => {
                    let result = dispatch::<Shutdown, _>(params, |_| Ok(()));
                    let _ = respond(&tx, id, result).await;
                    return;
                }
                _ => {
                    let core = self.core.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = core.handle(&method, params).await;
                        let _ = respond(&tx, id, result).await;
                    });
                    continue;
                }
            };

            if respond(&tx, id, result).await.is_err() {
                return;
            }
        }
    }
}

fn initialize(params: InitializeParams) -> Result<InitializeResult, RpcError> {
    if params.protocol_version != PROTOCOL_VERSION {
        let mut err = RpcError::new(
            RpcError::VERSION_MISMATCH,
            format!(
                "protocol version mismatch: client {}, server {PROTOCOL_VERSION}",
                params.protocol_version
            ),
        );
        err.data = Some(json!({ "server_protocol_version": PROTOCOL_VERSION }));
        return Err(err);
    }
    Ok(InitializeResult {
        protocol_version: PROTOCOL_VERSION,
        server_name: SERVER_NAME.to_owned(),
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

async fn respond(
    tx: &tokio::sync::mpsc::Sender<Message>,
    id: RequestId,
    result: Result<Value, RpcError>,
) -> Result<(), ()> {
    tx.send(Message::Response { id, result })
        .await
        .map_err(|_| ())
}

/// Next core event, or pending forever before the handshake. A receiver that
/// fell behind gets `session/updated` with `reason: "lagged"` and no session
/// in place of what it missed, so it loads its sessions again.
async fn recv_event(events: &mut Option<broadcast::Receiver<Event>>) -> Event {
    let Some(rx) = events else {
        return std::future::pending().await;
    };
    match rx.recv().await {
        Ok(event) => event,
        Err(broadcast::error::RecvError::Lagged(_)) => {
            Event::new::<SessionUpdated>(SessionUpdatedParams {
                session_id: String::new(),
                reason: "lagged".into(),
            })
        }
        Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
    }
}
