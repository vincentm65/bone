//! Unix socket listener.

use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

use bone_proto::transport;
use tokio::net::{UnixListener, UnixStream};

use crate::Server;

/// A bound socket. Removes the socket file when dropped.
pub struct Listener {
    listener: UnixListener,
    path: PathBuf,
    removed: std::sync::atomic::AtomicBool,
}

impl Listener {
    /// Bind `path`, creating its directory (mode 0700) if needed. A stale
    /// socket left by a dead server is replaced; a live one is an error.
    pub async fn bind(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_socket() => {
                if UnixStream::connect(path).await.is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("a server is already listening on {}", path.display()),
                    ));
                }
                std::fs::remove_file(path)?;
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", path.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(path)?;
        // Full trust over the API, so only this user may connect.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Listener {
            listener,
            path: path.to_owned(),
            removed: false.into(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove the socket file now, while those already connected are
    /// still served: a new launch then starts its own server, and this
    /// one's end cannot take a successor's socket with it.
    pub fn unlink(&self) {
        if !self.removed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Accept connections forever, serving each on its own task.
    pub async fn run(&self, server: &Server) -> io::Result<()> {
        self.run_counted(server, tokio::sync::watch::channel(0).0)
            .await
    }

    /// Like [`run`](Self::run), keeping `clients` at the number of
    /// connections currently being served.
    pub async fn run_counted(
        &self,
        server: &Server,
        clients: tokio::sync::watch::Sender<usize>,
    ) -> io::Result<()> {
        let mut failures = 0;
        loop {
            let (stream, _) = match self.listener.accept().await {
                Ok(accepted) => accepted,
                Err(e) => {
                    crate::accept_failed(e, &mut failures).await;
                    continue;
                }
            };
            let (r, w) = stream.into_split();
            let server = server.clone();
            clients.send_modify(|n| *n += 1);
            let clients = clients.clone();
            tokio::spawn(async move {
                server.serve(transport::framed(r, w)).await;
                clients.send_modify(|n| *n -= 1);
            });
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.unlink();
    }
}
