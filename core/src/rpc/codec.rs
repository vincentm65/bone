//! Newline-delimited JSON framing for the runtime protocol.
//!
//! One message per line: `serde_json` payload + `\n`. Simple, debuggable, and
//! dependency-free. The `RuntimeEvent`/`RuntimeCommand` types are the contract;
//! swapping this codec for msgpack later does not change them.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};

/// Error reading a framed message.
#[derive(Debug)]
pub enum ReadError {
    /// Transport error (treat as fatal for the connection).
    Io(std::io::Error),
    /// A line failed to decode (skip it; the connection is still healthy).
    Decode(serde_json::Error),
}

/// Write one message as a JSON line and flush it.
pub async fn write_message<W, T>(w: &mut W, msg: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut bytes = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    bytes.push(b'\n');
    w.write_all(&bytes).await?;
    w.flush().await
}

/// Reads framed messages from an `AsyncRead`, one JSON value per line.
pub struct MessageReader<R> {
    lines: Lines<BufReader<R>>,
}

impl<R> MessageReader<R>
where
    R: AsyncRead + Unpin,
{
    pub fn new(reader: R) -> Self {
        Self {
            lines: BufReader::new(reader).lines(),
        }
    }

    /// Read and decode the next message. `None` at end of stream.
    ///
    /// Blank lines are skipped. A decode failure is reported as
    /// [`ReadError::Decode`] without ending the stream, so callers can skip
    /// junk and keep reading.
    pub async fn read<T: DeserializeOwned>(&mut self) -> Option<Result<T, ReadError>> {
        loop {
            match self.lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    return Some(serde_json::from_str::<T>(&line).map_err(ReadError::Decode));
                }
                Ok(None) => return None,
                Err(e) => return Some(Err(ReadError::Io(e))),
            }
        }
    }
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod codec_tests;
