//! Local-daemon discovery and lifecycle for the desktop frontend.
//!
//! The frontend talks to a `bone serve` daemon over TCP. For the common local
//! case (a loopback address) the app can start its own daemon when none is
//! listening, so users never have to launch one by hand. Remote addresses are
//! never auto-started: they are always a deliberate "connect to a server"
//! action in the Server dialog.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Default `bone serve` bind address, matching the daemon default.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:7878";

/// Maximum automatic reconnect rounds after spawning the daemon before we
/// surface an error instead of retrying forever.
pub const MAX_DAEMON_RETRIES: u32 = 10;

/// Delay between automatic reconnect rounds while the daemon boots.
pub const DAEMON_RETRY_DELAY_MS: u64 = 600;

/// Split `host:port` for a connect/spawn address. Accepts IPv4, bracketed
/// IPv6, and hostnames. Returns `None` when the address carries no port.
pub fn split_host_port(address: &str) -> Option<(String, u16)> {
    let address = address.trim();
    if let Some(rest) = address.strip_prefix('[') {
        // [v6]:port
        let end = rest.find(']')?;
        let host = &rest[..end];
        let port = rest[end + 1..].strip_prefix(':')?.parse().ok()?;
        return Some((host.to_owned(), port));
    }
    let (host, port) = address.rsplit_once(':')?;
    if host.contains(':') {
        // Bare IPv6 without a port: unsupported for a socket target.
        return None;
    }
    Some((host.to_owned(), port.parse().ok()?))
}

/// True when the address points at the local machine (auto-start is safe).
pub fn is_loopback(address: &str) -> bool {
    split_host_port(address)
        .map(|(host, _)| host_is_loopback(&host))
        .unwrap_or(false)
}

fn host_is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Append `:7878` when the address has no port so connect/spawn always have a
/// concrete target. Bare IPv6 is returned unchanged (callers reject it).
pub fn ensure_port(address: &str) -> String {
    let trimmed = address.trim();
    if split_host_port(trimmed).is_some() || trimmed.contains(':') {
        trimmed.to_owned()
    } else {
        format!("{trimmed}:7878")
    }
}

/// Classify a connect-failure reason string produced by the socket worker.
/// `true` when the peer actively refused the connection (port closed) — the
/// only case where auto-starting a local daemon makes sense.
pub fn is_refused(reason: &str) -> bool {
    reason.contains("refused")
}

fn executable_name() -> &'static str {
    if cfg!(windows) {
        "bone.exe"
    } else {
        "bone"
    }
}

/// Locate the `bone` daemon binary: an explicit `BONE_DESKTOP_DAEMON` override,
/// then a sibling of the running app (dev/release layout), then `bone` on PATH.
pub fn resolve_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("BONE_DESKTOP_DAEMON") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(executable_name());
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(executable_name());
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Log file for an app-spawned daemon, kept next to the app's own state file
/// when one is configured.
pub fn daemon_log_path(state_dir: Option<&Path>) -> PathBuf {
    match state_dir {
        Some(dir) => dir.join("logs").join("daemon.log"),
        None => PathBuf::from("bone-desktop-daemon.log"),
    }
}

/// Launch `bin serve --listen <address>` detached from the app. The daemon
/// deliberately outlives the app (the frontend is a client, not a supervisor),
/// so a shared daemon can keep serving the TUI/web clients. Logs append to
/// `log_path`. Returns the spawned pid on success.
pub fn spawn_daemon(bin: &Path, address: &str, log_path: &Path) -> std::io::Result<u32> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let err = log.try_clone()?;
    let mut command = Command::new(bin);
    command
        .arg("serve")
        .arg("--listen")
        .arg(address)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group so a parent/terminal exit does not take it down.
        command.process_group(0);
    }
    Ok(command.spawn()?.id())
}

/// Lifecycle of the daemon target the app is talking to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// No daemon confirmed yet; decide on the first failed connect.
    Probe,
    /// A local daemon was spawned; wait for its socket to accept connects.
    Starting { attempts: u32 },
    /// A tab connected: the daemon is reachable.
    Ready,
    /// Automatic startup is not possible or gave up; surface a message and
    /// leave further action to the Server dialog.
    Stopped(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_handles_ipv4_hostname_and_ipv6() {
        assert_eq!(
            split_host_port("127.0.0.1:17878"),
            Some(("127.0.0.1".into(), 17878))
        );
        assert_eq!(split_host_port("localhost:80"), Some(("localhost".into(), 80)));
        assert_eq!(
            split_host_port("[::1]:9000"),
            Some(("::1".into(), 9000))
        );
        assert_eq!(split_host_port("127.0.0.1"), None);
        assert_eq!(split_host_port(""), None);
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("127.0.0.1:7878"));
        assert!(is_loopback("localhost:1234"));
        assert!(is_loopback("[::1]:1"));
        assert!(!is_loopback("10.0.0.5:8080"));
        assert!(!is_loopback("example.com:443"));
        assert!(!is_loopback("127.0.0.1")); // no port: not a socket target
    }

    #[test]
    fn ensure_port_appends_default_when_missing() {
        assert_eq!(ensure_port("localhost"), "localhost:7878");
        assert_eq!(ensure_port(" 127.0.0.1 "), "127.0.0.1:7878");
        assert_eq!(ensure_port("127.0.0.1:9000"), "127.0.0.1:9000");
        assert_eq!(ensure_port("[::1]:1"), "[::1]:1");
    }

    #[test]
    fn refused_classification() {
        assert!(is_refused("Connect failed: Connection refused (os error 111)"));
        assert!(is_refused("Connect failed: Connection refused"));
        assert!(!is_refused("Connect failed: connection timed out after 10 seconds"));
        assert!(!is_refused("Disconnected"));
    }
}
