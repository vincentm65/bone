//! Client for a bone server over any [`Connection`].
//!
//! Get a connection from [`connect_unix`], [`spawn`] (a headless server as a
//! child process over its stdio), or a server's in-process transport.
//!
//! [`Client::new`] splits a connection into a request handle and an
//! [`EventStream`] of server notifications. Requests are typed by
//! [`Method`]; responses are matched to requests by id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use bone_proto::methods::{Initialize, InitializeParams, InitializeResult, Shutdown};
use bone_proto::{
    Connection, Message, Method, Notification, PROTOCOL_VERSION, RequestId, RpcError,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("connection closed")]
    Disconnected,
    #[error("malformed response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("server speaks protocol {server}, this client speaks {PROTOCOL_VERSION}")]
    VersionMismatch { server: u32 },
}

type Reply = oneshot::Sender<Result<Value, RpcError>>;

#[derive(Default)]
struct Pending {
    closed: bool,
    replies: HashMap<RequestId, Reply>,
}

pub struct Client {
    tx: mpsc::Sender<Message>,
    pending: Arc<Mutex<Pending>>,
    next_id: AtomicI64,
}

/// A notification received from the server.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub method: String,
    pub params: Option<Value>,
}

impl Event {
    /// Decode as `N` if this event is an `N`.
    pub fn parse<N: Notification>(&self) -> Option<Result<N::Params, serde_json::Error>> {
        (self.method == N::METHOD)
            .then(|| serde_json::from_value(self.params.clone().unwrap_or(Value::Null)))
    }
}

/// Server notifications in arrival order. Unbounded so a slow consumer never
/// stalls response delivery; ends when the connection closes.
pub struct EventStream {
    rx: mpsc::UnboundedReceiver<Event>,
}

impl EventStream {
    pub async fn recv(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}

impl Client {
    /// Take over a connection. Spawns a reader task, so a tokio runtime must
    /// be running.
    pub fn new(conn: Connection) -> (Client, EventStream) {
        let Connection { tx, rx } = conn;
        let pending = Arc::new(Mutex::new(Pending::default()));
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        tokio::spawn(read_loop(rx, pending.clone(), events_tx));
        let client = Client {
            tx,
            pending,
            next_id: AtomicI64::new(1),
        };
        (client, EventStream { rx: events_rx })
    }

    /// Send a typed request and wait for its response.
    pub async fn request<M: Method>(&self, params: M::Params) -> Result<M::Result, ClientError> {
        let value = self
            .request_raw(M::METHOD, serde_json::to_value(params)?)
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Send a request by method name with untyped params (for scripting).
    pub async fn request_raw(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (reply_tx, reply_rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.closed {
                return Err(ClientError::Disconnected);
            }
            pending.replies.insert(id.clone(), reply_tx);
        }
        let msg = Message::Request {
            id: id.clone(),
            method: method.to_owned(),
            params: Some(params),
        };
        if self.tx.send(msg).await.is_err() {
            self.pending.lock().unwrap().replies.remove(&id);
            return Err(ClientError::Disconnected);
        }
        Ok(reply_rx.await.map_err(|_| ClientError::Disconnected)??)
    }

    /// Perform the handshake with this build's protocol version.
    pub async fn initialize(&self, client_name: &str) -> Result<InitializeResult, ClientError> {
        let result = self
            .request::<Initialize>(InitializeParams {
                protocol_version: PROTOCOL_VERSION,
                client_name: client_name.to_owned(),
            })
            .await?;
        if result.protocol_version != PROTOCOL_VERSION {
            return Err(ClientError::VersionMismatch {
                server: result.protocol_version,
            });
        }
        Ok(result)
    }

    /// Ask the server to close the connection.
    pub async fn shutdown(&self) -> Result<(), ClientError> {
        self.request::<Shutdown>(Default::default()).await
    }
}

/// Connect to a server listening on a Unix socket.
pub async fn connect_unix(path: impl AsRef<std::path::Path>) -> std::io::Result<Connection> {
    let stream = tokio::net::UnixStream::connect(path).await?;
    let (r, w) = stream.into_split();
    Ok(bone_proto::transport::framed(r, w))
}

/// Start `cmd` (e.g. `bone --headless`) and talk to it over its stdin/stdout.
/// stderr is inherited. The child is killed when the returned handle drops.
pub fn spawn(
    mut cmd: tokio::process::Command,
) -> std::io::Result<(Connection, tokio::process::Child)> {
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    Ok((bone_proto::transport::framed(stdout, stdin), child))
}

async fn read_loop(
    mut rx: mpsc::Receiver<Message>,
    pending: Arc<Mutex<Pending>>,
    events: mpsc::UnboundedSender<Event>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            Message::Response { id, result } => {
                if let Some(reply) = pending.lock().unwrap().replies.remove(&id) {
                    let _ = reply.send(result);
                }
            }
            Message::Notification { method, params } => {
                let _ = events.send(Event { method, params });
            }
            // The server sends no requests yet.
            Message::Request { .. } => {}
        }
    }
    // Dropping the reply senders fails every in-flight request.
    let mut pending = pending.lock().unwrap();
    pending.closed = true;
    pending.replies.clear();
}
