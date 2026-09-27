//! SSH transport for thin remote frontends.
//!
//! A client runs `ssh -T <host> -- bone stdio` and uses the child's stdout and
//! stdin as the newline-JSON runtime connection, exactly as it would a TCP
//! socket. On the remote, `bone stdio` ([`stdio_bridge`]) copies bytes between
//! its stdio and the loopback daemon. The daemon never listens beyond loopback
//! and gains no auth of its own: SSH owns authentication, encryption, and host
//! verification, using the user's own `~/.ssh` configuration.

use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// Overrides the `ssh` program (tests, or a wrapper such as `tsh`).
pub const SSH_PROGRAM_ENV: &str = "BONE_SSH";
/// Overrides the remote `bone` binary, for hosts where it is not on the
/// non-interactive `PATH`.
pub const REMOTE_BIN_ENV: &str = "BONE_SSH_REMOTE_BIN";
/// Bytes of ssh stderr kept to explain a failure.
const STDERR_TAIL_BYTES: usize = 4096;

/// A running `ssh … bone stdio` child. Dropping it kills the child.
pub struct SshSession {
    child: Child,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_task: tokio::task::JoinHandle<()>,
}

/// Start `ssh -T <host> -- bone stdio` and return its (read, write) halves for
/// [`crate::SocketConn`]. Batch mode keeps ssh from blocking on a password or
/// host-key prompt no one can see; keys or an agent are required.
pub fn ssh_connect(host: &str) -> std::io::Result<(ChildStdout, ChildStdin, SshSession)> {
    let program = std::env::var(SSH_PROGRAM_ENV).unwrap_or_else(|_| "ssh".into());
    let remote_bin = std::env::var(REMOTE_BIN_ENV).unwrap_or_else(|_| "bone".into());
    spawn_ssh(&program, host, &remote_bin)
}

fn spawn_ssh(
    program: &str,
    host: &str,
    remote_bin: &str,
) -> std::io::Result<(ChildStdout, ChildStdin, SshSession)> {
    let mut command = Command::new(program);
    command
        .args(ssh_args(host, remote_bin)?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let (Some(stdout), Some(stdin), Some(mut stderr)) =
        (child.stdout.take(), child.stdin.take(), child.stderr.take())
    else {
        return Err(std::io::Error::other("ssh started without piped stdio"));
    };
    let tail = Arc::new(Mutex::new(Vec::new()));
    let sink = tail.clone();
    let stderr_task = tokio::spawn(async move {
        let mut chunk = [0u8; 1024];
        while let Ok(read) = stderr.read(&mut chunk).await {
            if read == 0 {
                break;
            }
            let mut tail = sink.lock().unwrap_or_else(|error| error.into_inner());
            tail.extend_from_slice(&chunk[..read]);
            let excess = tail.len().saturating_sub(STDERR_TAIL_BYTES);
            tail.drain(..excess);
        }
    });
    Ok((
        stdout,
        stdin,
        SshSession {
            child,
            stderr: tail,
            stderr_task,
        },
    ))
}

/// Arguments after the program name. The host is validated so a value such as
/// `-oProxyCommand=…` can never be read as an ssh option.
pub fn ssh_args(host: &str, remote_bin: &str) -> std::io::Result<Vec<String>> {
    let host = host.trim();
    if host.is_empty() || host.starts_with('-') || host.chars().any(char::is_whitespace) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid ssh host {host:?}"),
        ));
    }
    Ok([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        host,
        "--",
        remote_bin,
        "stdio",
    ]
    .map(String::from)
    .to_vec())
}

impl SshSession {
    /// Why the session ended: ssh's exit status plus the tail of its stderr
    /// (e.g. `Permission denied (publickey)`). Waits briefly for the child.
    pub async fn failure(mut self) -> String {
        let status =
            tokio::time::timeout(std::time::Duration::from_secs(2), self.child.wait()).await;
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), &mut self.stderr_task)
            .await;
        let stderr = {
            let tail = self
                .stderr
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            String::from_utf8_lossy(&tail).trim().to_string()
        };
        let last = stderr.lines().last().unwrap_or_default().to_string();
        match status {
            Ok(Ok(status)) if !last.is_empty() => format!("ssh {status}: {last}"),
            Ok(Ok(status)) => format!("ssh {status}"),
            _ if !last.is_empty() => format!("ssh: {last}"),
            _ => "ssh session ended".into(),
        }
    }
}

/// Copy bytes between a client's stdio and the daemon connection until either
/// side closes: `input` → `daemon`, `daemon` → `output`.
pub async fn stdio_bridge<D, I, O>(daemon: D, mut input: I, mut output: O) -> std::io::Result<()>
where
    D: AsyncRead + AsyncWrite,
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let (mut from_daemon, mut to_daemon) = tokio::io::split(daemon);
    tokio::select! {
        result = tokio::io::copy(&mut input, &mut to_daemon) => result.map(drop),
        result = tokio::io::copy(&mut from_daemon, &mut output) => result.map(drop),
    }
}

#[cfg(test)]
#[path = "ssh_tests.rs"]
mod ssh_tests;
