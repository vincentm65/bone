//! Newline-delimited JSON framing for the runtime protocol.
//!
//! One message per line: `serde_json` payload + `\n`. Simple, debuggable, and
//! dependency-free. The `RuntimeEvent`/`RuntimeCommand` types are the contract;
//! swapping this codec for msgpack later does not change them.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Hard cap on a single framed line (bytes). Prevents a buggy client from
/// forcing unbounded memory growth on `bone serve`.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Error reading a framed message.
#[derive(Debug)]
pub enum ReadError {
    /// Transport error (treat as fatal for the connection).
    Io(std::io::Error),
    /// A line failed to decode (skip it; the connection is still healthy).
    Decode(serde_json::Error),
    /// A line exceeded [`MAX_LINE_BYTES`] (treat as fatal for the connection).
    TooLong { len: usize },
}

/// Write one message as a JSON line and flush it.
pub async fn write_message<W, T>(w: &mut W, msg: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut bytes = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if bytes.len() > MAX_LINE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "framed message is {} bytes; max is {MAX_LINE_BYTES}",
                bytes.len()
            ),
        ));
    }
    bytes.push(b'\n');
    w.write_all(&bytes).await?;
    w.flush().await
}

/// Reads framed messages from an `AsyncRead`, one JSON value per line.
///
/// Uses a byte buffer + delimiter scan so oversized frames can be rejected
/// without waiting for a newline that never arrives.
pub struct MessageReader<R> {
    reader: BufReader<R>,
    buf: Vec<u8>,
}

impl<R> MessageReader<R>
where
    R: AsyncRead + Unpin,
{
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::with_capacity(64 * 1024, reader),
            buf: Vec::new(),
        }
    }

    /// Read and decode the next message. `None` at end of stream.
    ///
    /// Blank lines are skipped. A decode failure is reported as
    /// [`ReadError::Decode`] without ending the stream, so callers can skip
    /// junk and keep reading. Oversized lines are [`ReadError::TooLong`].
    pub async fn read<T: DeserializeOwned>(&mut self) -> Option<Result<T, ReadError>> {
        loop {
            match self.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    return Some(serde_json::from_str::<T>(&line).map_err(ReadError::Decode));
                }
                Ok(None) => return None,
                Err(e) => return Some(Err(e)),
            }
        }
    }

    async fn next_line(&mut self) -> Result<Option<String>, ReadError> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line = self.buf.drain(..=pos).collect::<Vec<u8>>();
                line.pop(); // drop '\n'
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.len() > MAX_LINE_BYTES {
                    return Err(ReadError::TooLong { len: line.len() });
                }
                return String::from_utf8(line).map(Some).map_err(|e| {
                    ReadError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                });
            }
            if self.buf.len() > MAX_LINE_BYTES {
                return Err(ReadError::TooLong {
                    len: self.buf.len(),
                });
            }
            // Grow by filling the BufReader; stop if nothing more arrives.
            let mut tmp = [0_u8; 8 * 1024];
            match tokio::io::AsyncReadExt::read(&mut self.reader, &mut tmp).await {
                Ok(0) => {
                    if self.buf.is_empty() {
                        return Ok(None);
                    }
                    // EOF without trailing newline: treat remaining as a line.
                    if self.buf.len() > MAX_LINE_BYTES {
                        return Err(ReadError::TooLong {
                            len: self.buf.len(),
                        });
                    }
                    let line = std::mem::take(&mut self.buf);
                    return String::from_utf8(line).map(Some).map_err(|e| {
                        ReadError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                    });
                }
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(e) => return Err(ReadError::Io(e)),
            }
        }
    }
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod codec_tests;
