//! Lightweight newline-JSON transport for Bone frontends.

use bone_protocol::{RuntimeCommand, RuntimeEvent};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum ReadError {
    Io(std::io::Error),
    Decode(serde_json::Error),
    TooLong { len: usize },
}

impl ReadError {
    pub fn is_recoverable(&self) -> bool {
        matches!(self, Self::Decode(_))
    }
    pub fn into_fatal_io(self) -> Option<std::io::Error> {
        match self {
            Self::Decode(_) => None,
            Self::Io(error) => Some(error),
            Self::TooLong { len } => Some(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("framed message is {len} bytes; max is {MAX_LINE_BYTES}"),
            )),
        }
    }
}

pub async fn write_message<W, T>(writer: &mut W, message: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut bytes = serde_json::to_vec(message)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
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
    writer.write_all(&bytes).await?;
    writer.flush().await
}

pub struct MessageReader<R> {
    reader: BufReader<R>,
    buf: Vec<u8>,
    scan_start: usize,
}
impl<R: AsyncRead + Unpin> MessageReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::with_capacity(64 * 1024, reader),
            buf: Vec::new(),
            scan_start: 0,
        }
    }
    pub async fn read<T: DeserializeOwned>(&mut self) -> Option<Result<T, ReadError>> {
        loop {
            match self.next_line().await {
                Ok(Some(line)) if line.trim().is_empty() => continue,
                Ok(Some(line)) => {
                    return Some(serde_json::from_str(&line).map_err(ReadError::Decode));
                }
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            }
        }
    }
    async fn next_line(&mut self) -> Result<Option<String>, ReadError> {
        loop {
            if let Some(offset) = self.buf[self.scan_start..]
                .iter()
                .position(|&byte| byte == b'\n')
            {
                let pos = self.scan_start + offset;
                let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
                self.scan_start = 0;
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.len() > MAX_LINE_BYTES {
                    return Err(ReadError::TooLong { len: line.len() });
                }
                return String::from_utf8(line).map(Some).map_err(|error| {
                    ReadError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
                });
            }
            if self.buf.len() > MAX_LINE_BYTES {
                return Err(ReadError::TooLong {
                    len: self.buf.len(),
                });
            }
            self.scan_start = self.buf.len();
            let mut chunk = [0_u8; 8 * 1024];
            match tokio::io::AsyncReadExt::read(&mut self.reader, &mut chunk).await {
                Ok(0) if self.buf.is_empty() => return Ok(None),
                Ok(0) => {
                    let line = std::mem::take(&mut self.buf);
                    self.scan_start = 0;
                    return String::from_utf8(line).map(Some).map_err(|error| {
                        ReadError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
                    });
                }
                Ok(count) => self.buf.extend_from_slice(&chunk[..count]),
                Err(error) => return Err(ReadError::Io(error)),
            }
        }
    }
}

pub struct SocketConn<R> {
    reader: MessageReader<R>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    writer: tokio::task::JoinHandle<()>,
}
impl<R: AsyncRead + Unpin> SocketConn<R> {
    pub fn new<W: AsyncWrite + Unpin + Send + 'static>(read_half: R, write_half: W) -> Self {
        let (command_tx, mut commands) = mpsc::unbounded_channel();
        let writer = tokio::spawn(async move {
            let mut writer = write_half;
            while let Some(command) = commands.recv().await {
                if write_message(&mut writer, &command).await.is_err() {
                    break;
                }
            }
        });
        Self {
            reader: MessageReader::new(read_half),
            command_tx,
            writer,
        }
    }
    pub fn command_sender(&self) -> mpsc::UnboundedSender<RuntimeCommand> {
        self.command_tx.clone()
    }
    pub fn send(&mut self, command: RuntimeCommand) {
        let _ = self.command_tx.send(command);
    }
    pub async fn next_event(&mut self) -> Option<RuntimeEvent> {
        loop {
            match self.reader.read().await {
                Some(Ok(event)) => return Some(event),
                Some(Err(error)) if error.is_recoverable() => continue,
                Some(Err(_)) | None => return None,
            }
        }
    }
}
impl<R> Drop for SocketConn<R> {
    fn drop(&mut self) {
        self.writer.abort();
    }
}

pub struct RemoteClient {
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    events_tx: Arc<std::sync::Mutex<Option<broadcast::Sender<RuntimeEvent>>>>,
    primary_rx: std::sync::Mutex<Option<broadcast::Receiver<RuntimeEvent>>>,
    forwarder: tokio::task::JoinHandle<()>,
}
impl RemoteClient {
    pub fn connect<R, W>(read_half: R, write_half: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let mut connection = SocketConn::new(read_half, write_half);
        let command_tx = connection.command_sender();
        let (sender, primary_rx) = broadcast::channel(1024);
        let events_tx = Arc::new(std::sync::Mutex::new(Some(sender)));
        let forward_events = Arc::clone(&events_tx);
        let forwarder = tokio::spawn(async move {
            while let Some(event) = connection.next_event().await {
                let sender = forward_events.lock().unwrap().as_ref().cloned();
                let Some(sender) = sender else { break };
                let _ = sender.send(event);
            }
            forward_events.lock().unwrap().take();
        });
        Self {
            command_tx,
            events_tx,
            primary_rx: std::sync::Mutex::new(Some(primary_rx)),
            forwarder,
        }
    }
    pub fn command_sender(&self) -> mpsc::UnboundedSender<RuntimeCommand> {
        self.command_tx.clone()
    }
    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        if let Some(receiver) = self.primary_rx.lock().unwrap().take() {
            receiver
        } else if let Some(sender) = self.events_tx.lock().unwrap().as_ref() {
            sender.subscribe()
        } else {
            let (sender, receiver) = broadcast::channel(1);
            drop(sender);
            receiver
        }
    }
}
impl Drop for RemoteClient {
    fn drop(&mut self) {
        self.forwarder.abort();
        self.events_tx.lock().unwrap().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn round_trips_and_skips_bad_json() {
        let (a, mut b) = tokio::io::duplex(1024);
        b.write_all(b"bad\n").await.unwrap();
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
