//! Transports carry [`Message`]s between a client and a server.
//!
//! Every transport is exposed as a [`Connection`]: a sender and a receiver of
//! messages. Servers and clients only ever see a `Connection`, so the in-process
//! channel, Unix sockets and stdio are interchangeable.
//!
//! - [`in_process`]: channels, no serialization.
//! - [`framed`]: NDJSON over any byte stream (Unix socket, stdio, child pipes).

use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::{Message, RequestId, RpcError, codec};

/// Bounded so a stalled peer applies backpressure instead of growing memory.
const CHANNEL_CAPACITY: usize = 256;
/// Enough for a 20 MiB image's base64 and envelope, with a hard allocation cap.
const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

/// One end of a bidirectional message stream.
///
/// `rx` yields `None` once the peer is gone. Dropping `tx` closes our sending
/// direction, which the peer sees as end of stream.
#[derive(Debug)]
pub struct Connection {
    pub tx: mpsc::Sender<Message>,
    pub rx: mpsc::Receiver<Message>,
}

/// In-process transport: two connected ends with no serialization.
///
/// Returns `(client_end, server_end)`.
pub fn in_process() -> (Connection, Connection) {
    let (client_tx, server_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (server_tx, client_rx) = mpsc::channel(CHANNEL_CAPACITY);
    (
        Connection {
            tx: client_tx,
            rx: client_rx,
        },
        Connection {
            tx: server_tx,
            rx: server_rx,
        },
    )
}

/// NDJSON over a byte stream. Spawns a reader and a writer task, so a tokio
/// runtime must be running.
///
/// Lines that do not decode are answered with a JSON-RPC parse error and
/// otherwise skipped. The writer shuts the stream down once every `tx` clone
/// is dropped; the reader stops at end of stream or when `rx` is dropped.
pub fn framed<R, W>(reader: R, writer: W) -> Connection
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    framed_with_writer(reader, writer).0
}

/// [`framed`], plus the writer task. It finishes once every `tx` is dropped
/// and everything queued has been written and flushed; await it before
/// exiting the process so the last messages are not lost.
pub fn framed_with_writer<R, W>(reader: R, writer: W) -> (Connection, tokio::task::JoinHandle<()>)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (in_tx, in_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (out_tx, out_rx) = mpsc::channel(CHANNEL_CAPACITY);
    // Weak, so the reader never keeps the writer (and the stream) open.
    let errors = out_tx.downgrade();
    let writer = tokio::spawn(write_loop(writer, out_rx));
    tokio::spawn(read_loop(reader, in_tx, errors));
    (
        Connection {
            tx: out_tx,
            rx: in_rx,
        },
        writer,
    )
}

async fn read_loop<R: AsyncRead + Unpin>(
    reader: R,
    tx: mpsc::Sender<Message>,
    errors: mpsc::WeakSender<Message>,
) {
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let mut limited = (&mut reader).take(MAX_FRAME_BYTES + 1);
        let size = tokio::select! {
            size = limited.read_until(b'\n', &mut buffer) => match size {
                Ok(0) | Err(_) => return,
                Ok(size) => size,
            },
            _ = tx.closed() => return,
        };
        if size as u64 > MAX_FRAME_BYTES {
            if let Some(out) = errors.upgrade() {
                let _ = out
                    .send(Message::Response {
                        id: RequestId::Null,
                        result: Err(RpcError::new(
                            RpcError::INVALID_REQUEST,
                            "JSON frame exceeds the 32 MiB limit",
                        )),
                    })
                    .await;
            }
            return;
        }
        let line = match std::str::from_utf8(&buffer) {
            Ok(line) => line,
            Err(_) => return,
        };
        if line.trim().is_empty() {
            continue;
        }
        match codec::decode(line) {
            Ok(msg) => {
                if tx.send(msg).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                let Some(out) = errors.upgrade() else {
                    continue;
                };
                let reply = Message::Response {
                    id: RequestId::Null,
                    result: Err(RpcError::new(
                        RpcError::PARSE_ERROR,
                        format!("parse error: {e}"),
                    )),
                };
                let _ = out.send(reply).await;
            }
        }
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(mut writer: W, mut rx: mpsc::Receiver<Message>) {
    while let Some(msg) = rx.recv().await {
        let line = codec::encode(&msg);
        if writer.write_all(line.as_bytes()).await.is_err() {
            return;
        }
        // Batch whatever is already queued into one flush.
        let mut ok = true;
        while let Ok(msg) = rx.try_recv() {
            if writer
                .write_all(codec::encode(&msg).as_bytes())
                .await
                .is_err()
            {
                ok = false;
                break;
            }
        }
        if !ok || writer.flush().await.is_err() {
            return;
        }
    }
    let _ = writer.shutdown().await;
}

/// `$XDG_RUNTIME_DIR/bone3/bone.sock`, falling back to `/tmp/bone3-$USER/bone.sock`.
pub fn default_socket_path() -> PathBuf {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("bone3"),
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            std::env::temp_dir().join(format!("bone3-{user}"))
        }
    };
    dir.join("bone.sock")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    async fn recv(rx: &mut mpsc::Receiver<Message>) -> Option<Message> {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out")
    }

    #[tokio::test]
    async fn framed_round_trip_over_a_byte_stream() {
        let (a, b) = tokio::io::duplex(64);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let mut left = framed(ar, aw);
        let mut right = framed(br, bw);

        // Larger than the duplex buffer, so it crosses several writes.
        let big = "x".repeat(10_000);
        let msg = Message::Notification {
            method: "n".into(),
            params: Some(json!({ "text": big })),
        };
        left.tx.send(msg.clone()).await.unwrap();
        assert_eq!(recv(&mut right.rx).await, Some(msg));

        // Dropping one side's sender ends the other side's stream.
        drop(left.tx);
        assert_eq!(recv(&mut right.rx).await, None);
        // The reverse direction still works.
        right
            .tx
            .send(Message::Notification {
                method: "back".into(),
                params: None,
            })
            .await
            .unwrap();
        assert!(matches!(
            recv(&mut left.rx).await,
            Some(Message::Notification { .. })
        ));
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_decoding() {
        let (mut raw, b) = tokio::io::duplex(64 * 1024);
        let (br, bw) = tokio::io::split(b);
        let mut conn = framed(br, bw);
        let writer = tokio::spawn(async move {
            let block = vec![b'x'; 64 * 1024];
            for _ in 0..MAX_FRAME_BYTES / block.len() as u64 {
                raw.write_all(&block).await.unwrap();
            }
            raw.write_all(b"x").await.unwrap();
            let mut reply = BufReader::new(raw).lines();
            codec::decode(&reply.next_line().await.unwrap().unwrap()).unwrap()
        });
        assert_eq!(recv(&mut conn.rx).await, None);
        assert!(matches!(
            writer.await.unwrap(),
            Message::Response {
                result: Err(RpcError {
                    code: RpcError::INVALID_REQUEST,
                    ..
                }),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn malformed_lines_get_a_parse_error() {
        let (raw, b) = tokio::io::duplex(1024);
        let (br, bw) = tokio::io::split(b);
        let mut conn = framed(br, bw);
        let (raw_r, mut raw_w) = tokio::io::split(raw);
        let mut replies = BufReader::new(raw_r).lines();

        raw_w
            .write_all(b"garbage\n\n{\"jsonrpc\":\"2.0\",\"method\":\"ok\"}\n")
            .await
            .unwrap();
        let reply = codec::decode(&replies.next_line().await.unwrap().unwrap()).unwrap();
        assert!(matches!(
            reply,
            Message::Response {
                id: RequestId::Null,
                result: Err(RpcError {
                    code: RpcError::PARSE_ERROR,
                    ..
                })
            }
        ));
        assert!(
            matches!(recv(&mut conn.rx).await, Some(Message::Notification { method, .. }) if method == "ok")
        );
    }
}
