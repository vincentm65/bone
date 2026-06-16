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
mod tests {
    use super::*;
    use crate::runtime::{RuntimeCommand, RuntimeEvent};

    #[tokio::test]
    async fn round_trips_event_over_duplex() {
        // Use whole duplex ends: dropping `b` fully signals EOF to `a`'s
        // reader. (A `split` write-half drop alone never closes the stream.)
        let (a, mut b) = tokio::io::duplex(1024);

        write_message(&mut b, &RuntimeEvent::TextDelta { text: "hi".into() })
            .await
            .unwrap();
        drop(b);

        let mut reader = MessageReader::new(a);
        let ev: RuntimeEvent = reader.read().await.unwrap().unwrap();
        assert!(matches!(ev, RuntimeEvent::TextDelta { text } if text == "hi"));
        assert!(
            reader.read::<RuntimeEvent>().await.is_none(),
            "EOF after one"
        );
    }

    #[tokio::test]
    async fn decode_error_is_recoverable() {
        let (a, mut b) = tokio::io::duplex(1024);

        b.write_all(b"garbage\n").await.unwrap();
        write_message(&mut b, &RuntimeCommand::Cancel)
            .await
            .unwrap();
        drop(b);

        let mut reader = MessageReader::new(a);
        assert!(matches!(
            reader.read::<RuntimeCommand>().await,
            Some(Err(ReadError::Decode(_)))
        ));
        assert!(matches!(
            reader.read::<RuntimeCommand>().await,
            Some(Ok(RuntimeCommand::Cancel))
        ));
    }
}
