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

/// Maximum automatic reconnect rounds after a mid-session drop before the
/// coordinator gives up and defers to the Server dialog.
pub const MAX_RECONNECT_ROUNDS: u32 = 5;

/// Delay between automatic reconnect rounds after a mid-session drop.
pub const RECONNECT_RETRY_DELAY_MS: u64 = 2000;

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

/// Native remote access must use a secure tunnel terminating on loopback.
/// Resolve `localhost` ourselves: never trust DNS to enforce this boundary.
///
/// Returns every loopback candidate for `address` in preference order. A
/// `localhost` host expands to both IPv4 and IPv6 loopback so a daemon bound to
/// either family is reachable; an explicit IP yields a single candidate.
pub fn local_endpoints(address: &str) -> Result<Vec<std::net::SocketAddr>, String> {
    let candidates = split_host_port(address).map(|(host, port)| {
        if host.eq_ignore_ascii_case("localhost") {
            vec![
                std::net::SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port),
                std::net::SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), port),
            ]
        } else {
            host.parse::<IpAddr>()
                .ok()
                .filter(|ip| ip.is_loopback())
                .map(|ip| vec![std::net::SocketAddr::new(ip, port)])
                .unwrap_or_default()
        }
    });
    match candidates {
        Some(endpoints) if !endpoints.is_empty() => Ok(endpoints),
        _ => Err(REMOTE_DISABLED.into()),
    }
}

/// The preferred loopback endpoint for `address`. See [`local_endpoints`] for
/// the full candidate list (a `localhost` host may have more than one).
pub fn local_endpoint(address: &str) -> Result<std::net::SocketAddr, String> {
    local_endpoints(address).map(|mut endpoints| endpoints.remove(0))
}

const REMOTE_DISABLED: &str = "Direct remote connections are disabled: Bone TCP has no encryption or authentication. Use an SSH tunnel and connect to 127.0.0.1:<forwarded-port>.";

/// Classify a connect-failure reason string produced by the socket worker.
/// `true` when the peer actively refused the connection (port closed) — the
/// only case where auto-starting a local daemon makes sense.
pub fn is_refused(reason: &str) -> bool {
    reason.contains("refused")
}

/// Classify a `Disconnected` reason: `true` when an already-established session
/// was dropped mid-way (socket loss, writer close, runtime failure) rather than
/// never connected in the first place. Drives the bounded mid-session
/// auto-reconnect; plain "Disconnected"/cancelled/init-failure reasons do not.
pub fn is_mid_session_drop(reason: &str) -> bool {
    !reason.starts_with("Connect failed")
        && reason != "Disconnected"
        && reason != "Connection cancelled"
}

fn executable_name() -> &'static str {
    if cfg!(windows) { "bone.exe" } else { "bone" }
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
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(executable_name());
        if candidate.is_file() {
            return Some(candidate);
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
/// `log_path`. Returns the spawned `Child` on success; the caller keeps it so
/// the app can detect the death of a daemon it started itself (`try_wait`).
pub fn spawn_daemon(
    bin: &Path,
    address: &str,
    log_path: &Path,
) -> std::io::Result<std::process::Child> {
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
    command.spawn()
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
        assert_eq!(
            split_host_port("localhost:80"),
            Some(("localhost".into(), 80))
        );
        assert_eq!(split_host_port("[::1]:9000"), Some(("::1".into(), 9000)));
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
    fn local_endpoints_expand_localhost_to_both_families() {
        let endpoints = local_endpoints("localhost:7878").unwrap();
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0], "127.0.0.1:7878".parse().unwrap());
        assert_eq!(endpoints[1], "[::1]:7878".parse().unwrap());
        // An explicit loopback IP yields exactly one candidate.
        assert_eq!(
            local_endpoints("[::1]:9000").unwrap(),
            vec!["[::1]:9000".parse().unwrap()]
        );
        // Non-loopback and malformed targets are still rejected.
        assert!(local_endpoints("10.0.0.5:7878").is_err());
        assert!(local_endpoints("127.0.0.1").is_err());
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
        assert!(is_refused(
            "Connect failed: Connection refused (os error 111)"
        ));
        assert!(is_refused("Connect failed: Connection refused"));
        assert!(!is_refused(
            "Connect failed: connection timed out after 10 seconds"
        ));
        assert!(!is_refused("Disconnected"));
    }

    #[test]
    fn mid_session_drop_classification() {
        // Established sockets lost mid-way: reconnect is warranted.
        assert!(is_mid_session_drop(
            "Connection lost. Delivery may be uncertain; reconnect manually. Prompts are never resent automatically."
        ));
        assert!(is_mid_session_drop(
            "Connection writer closed; delivery may be uncertain."
        ));
        assert!(is_mid_session_drop("Runtime initialization failed: boom"));
        // Never-connected / deliberate reasons: no auto-reconnect.
        assert!(!is_mid_session_drop(
            "Connect failed: Connection refused (os error 111)"
        ));
        assert!(!is_mid_session_drop("Connect failed: connection timed out"));
        assert!(!is_mid_session_drop("Disconnected"));
        assert!(!is_mid_session_drop("Connection cancelled"));
    }
}
