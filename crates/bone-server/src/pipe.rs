//! Windows named-pipe listener, in place of the Unix socket.

use std::io;
use std::path::{Path, PathBuf};

use bone_proto::transport;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::Server;

/// A pipe name such as `\\.\pipe\bone3-USER` that we serve. The name goes
/// away with the last instance.
pub struct Listener {
    path: PathBuf,
    next: std::sync::Mutex<Option<NamedPipeServer>>,
}

impl Listener {
    /// Create the first instance of `path`. A pipe another server already
    /// serves is an error.
    pub async fn bind(path: &Path) -> io::Result<Self> {
        let first = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(path)
            .map_err(|e| {
                if e.raw_os_error() == Some(5) {
                    // ERROR_ACCESS_DENIED: the first instance exists.
                    io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("a server is already listening on {}", path.display()),
                    )
                } else {
                    e
                }
            })?;
        Ok(Listener {
            path: path.to_owned(),
            next: std::sync::Mutex::new(Some(first)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accept connections forever, serving each on its own task. A new
    /// instance waits before each client is handed off, so none is turned
    /// away between connections.
    pub async fn run(&self, server: &Server) -> io::Result<()> {
        let mut pipe = match self.next.lock().unwrap().take() {
            Some(pipe) => pipe,
            None => self.instance()?,
        };
        loop {
            pipe.connect().await?;
            let connected = std::mem::replace(&mut pipe, self.instance()?);
            let (r, w) = tokio::io::split(connected);
            let server = server.clone();
            tokio::spawn(async move { server.serve(transport::framed(r, w)).await });
        }
    }

    fn instance(&self) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .reject_remote_clients(true)
            .create(&self.path)
    }
}
