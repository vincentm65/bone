//! In-app SSH (russh), since Android has no `ssh` program.
//!
//! The app owns one ed25519 key, created on first use in its data directory;
//! the user adds its public line to `~/.ssh/authorized_keys` on the computer.
//! Host keys are trusted on first use and pinned in `<data>/known_hosts`; a
//! changed key refuses the connection. The channel runs `<bone> stdio`, whose
//! stream is the same newline-JSON transport as every other client.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::ChannelMsg;
use russh::client::{self, Handle, Msg};
use russh::keys::ssh_key::LineEnding;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};

const KEY_FILE: &str = "id_ed25519";
const KNOWN_HOSTS_FILE: &str = "known_hosts";

/// The app's SSH key and trust store.
pub struct Identity {
    key: Arc<PrivateKey>,
    dir: PathBuf,
}

impl Identity {
    /// Load `<dir>/id_ed25519`, creating it on first use.
    pub fn load_or_create(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(KEY_FILE);
        let key = match std::fs::read_to_string(&path) {
            Ok(pem) => PrivateKey::from_openssh(pem).map_err(std::io::Error::other)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut seed = [0u8; 32];
                std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut seed)?;
                let mut key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
                key.set_comment("bone-android");
                let pem = key
                    .to_openssh(LineEnding::LF)
                    .map_err(std::io::Error::other)?;
                write_atomic(&path, pem.as_bytes())?;
                key
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            key: Arc::new(key),
            dir: dir.to_path_buf(),
        })
    }

    /// The line to add to `~/.ssh/authorized_keys` on the computer.
    pub fn public_line(&self) -> String {
        self.key.public_key().to_openssh().unwrap_or_default()
    }

    /// Whether this destination already has a host key in this app's pin
    /// store. A key is considered pinned even when the next connection would
    /// reject it as changed; that distinction is useful on the connect screen.
    pub fn host_key_pinned(&self, destination: &Destination) -> bool {
        russh::keys::known_hosts::known_host_keys_path(
            &destination.host,
            destination.port,
            self.dir.join(KNOWN_HOSTS_FILE),
        )
        .is_ok_and(|keys| !keys.is_empty())
    }

    /// Remove only this destination's entries from the app's pin store.
    /// Other hosts remain trusted, and the next successful connection will
    /// deliberately pin this host again.
    pub fn forget_host_key(&self, destination: &Destination) -> std::io::Result<bool> {
        let path = self.dir.join(KNOWN_HOSTS_FILE);
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let matches = russh::keys::known_hosts::known_host_keys_path(
            &destination.host,
            destination.port,
            &path,
        )
        .map_err(std::io::Error::other)?;
        if matches.is_empty() {
            return Ok(false);
        }
        // russh numbers entries without counting `#` comment lines, so map its
        // numbers back onto the file's physical lines.
        let lines: Vec<usize> = matches.into_iter().map(|(line, _)| line).collect();
        let mut entry = 0;
        let kept: String = contents
            .split_inclusive('\n')
            .filter(|line| {
                if line.starts_with('#') {
                    return true;
                }
                entry += 1;
                !lines.contains(&entry)
            })
            .collect();
        write_atomic(&path, kept.as_bytes())?;
        Ok(true)
    }
}

/// Replace `path` atomically: write an owner-only sibling temp file, flush it
/// to disk, then rename it over `path`, so a crash or full disk never leaves a
/// torn or empty file behind.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temp = path.with_file_name(name);
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// `user@host[:port]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    pub user: String,
    pub host: String,
    pub port: u16,
}

impl Destination {
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        let (user, rest) = value
            .split_once('@')
            .filter(|(user, rest)| !user.is_empty() && !rest.is_empty())
            .ok_or("use user@host or user@host:port")?;
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (
                host,
                port.parse().map_err(|_| format!("invalid port {port:?}"))?,
            ),
            _ => (rest, 22),
        };
        if host.is_empty() || host.chars().any(char::is_whitespace) {
            return Err(format!("invalid host {host:?}"));
        }
        Ok(Self {
            user: user.to_string(),
            host: host.to_string(),
            port,
        })
    }
}

/// Trust-on-first-use host key check against the app's `known_hosts`.
pub(crate) struct Pinned {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    changed: Arc<std::sync::atomic::AtomicBool>,
}

/// What the pin store says about a key a server offered.
#[derive(Debug, PartialEq, Eq)]
enum PinDecision {
    /// The host has a pinned key and this is it.
    Trusted,
    /// Nothing is pinned for the host yet; trust and remember this key.
    Learn,
    /// The host has pinned keys and this is none of them, whatever its type.
    Changed,
}

fn pin_decision(
    host: &str,
    port: u16,
    key: &russh::keys::PublicKey,
    known_hosts: &Path,
) -> Result<PinDecision, russh::keys::Error> {
    let pinned = russh::keys::known_hosts::known_host_keys_path(host, port, known_hosts)?;
    Ok(if pinned.is_empty() {
        PinDecision::Learn
    } else if pinned
        .iter()
        .any(|(_, pinned)| pinned.key_data() == key.key_data())
    {
        PinDecision::Trusted
    } else {
        PinDecision::Changed
    })
}

impl client::Handler for Pinned {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        match pin_decision(&self.host, self.port, key, &self.known_hosts)? {
            PinDecision::Trusted => Ok(true),
            PinDecision::Learn => {
                russh::keys::known_hosts::learn_known_hosts_path(
                    &self.host,
                    self.port,
                    key,
                    &self.known_hosts,
                )?;
                Ok(true)
            }
            PinDecision::Changed => {
                self.changed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(false)
            }
        }
    }
}

/// Quote a remote program path for the login shell, keeping a leading `~/`
/// outside the quotes so the remote shell still expands it.
fn shell_path(path: &str) -> String {
    let (prefix, rest) = match path.strip_prefix("~/") {
        Some(rest) => ("~/", rest),
        None if path == "~" => return path.to_string(),
        None => ("", path),
    };
    if rest.is_empty() {
        return prefix.to_string();
    }
    format!("{prefix}'{}'", rest.replace('\'', "'\\''"))
}

/// How much of the remote's stderr is kept, and how much of it is shown.
const STDERR_KEPT: usize = 4096;
const STDERR_SHOWN: usize = 300;

/// What the remote `bone stdio` wrote to stderr and how it ended, so a closed
/// stream can say why (e.g. `bone: command not found`, exit status 127).
#[derive(Clone, Default)]
pub(crate) struct RemoteExit(Arc<std::sync::Mutex<ExitState>>);

#[derive(Default)]
struct ExitState {
    stderr: Vec<u8>,
    status: Option<u32>,
    signal: Option<String>,
}

impl RemoteExit {
    fn record(&self, message: &ChannelMsg) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match message {
            ChannelMsg::ExtendedData { data, ext: 1 } => {
                state.stderr.extend_from_slice(data);
                let excess = state.stderr.len().saturating_sub(STDERR_KEPT);
                state.stderr.drain(..excess);
            }
            ChannelMsg::ExitStatus { exit_status } => state.status = Some(*exit_status),
            ChannelMsg::ExitSignal {
                signal_name,
                error_message,
                ..
            } => {
                state.signal = Some(if error_message.is_empty() {
                    format!("{signal_name:?}")
                } else {
                    format!("{signal_name:?} ({error_message})")
                });
            }
            _ => {}
        }
    }

    /// A short explanation, or `None` after a clean exit with nothing on stderr.
    pub(crate) fn describe(&self) -> Option<String> {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stderr = String::from_utf8_lossy(&state.stderr);
        let lines: Vec<&str> = stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let mut said = lines[lines.len().saturating_sub(3)..].join(" / ");
        let chars = said.chars().count();
        if chars > STDERR_SHOWN {
            said = format!(
                "…{}",
                said.chars().skip(chars - STDERR_SHOWN).collect::<String>()
            );
        }
        let ended = match (&state.signal, state.status) {
            (Some(signal), _) => Some(format!("remote bone was killed by signal {signal}")),
            (None, Some(status)) if status != 0 => {
                Some(format!("remote bone exited with status {status}"))
            }
            _ => None,
        };
        match (ended, said.is_empty()) {
            (Some(ended), true) => Some(ended),
            (Some(ended), false) => Some(format!("{ended}: {said}")),
            (None, false) => Some(format!("remote stderr: {said}")),
            (None, true) => None,
        }
    }
}

/// Bridge the channel to an in-memory stream. Unlike [`russh::ChannelStream`],
/// this records stderr and the exit status, and ends the stream only when the
/// channel closes, so both are known once the reader sees EOF.
fn pump(channel: russh::Channel<Msg>) -> (tokio::io::DuplexStream, RemoteExit) {
    use tokio::io::AsyncWriteExt;
    let (mut output, input) = channel.split();
    let (local, remote) = tokio::io::duplex(64 * 1024);
    let (mut from_app, mut to_app) = tokio::io::split(remote);
    let exit = RemoteExit::default();
    let recorder = exit.clone();
    tokio::spawn(async move {
        while let Some(message) = output.wait().await {
            match message {
                ChannelMsg::Data { data } => {
                    if to_app.write_all(&data).await.is_err() {
                        return;
                    }
                }
                ChannelMsg::Close => break,
                message => recorder.record(&message),
            }
        }
        let _ = to_app.shutdown().await;
    });
    tokio::spawn(async move {
        let mut writer = std::pin::pin!(input.make_writer());
        let _ = tokio::io::copy(&mut from_app, &mut writer).await;
        let _ = writer.shutdown().await;
        drop(input);
    });
    (local, exit)
}

/// How long connecting, logging in and starting bone may take in total.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Log in and start `<bone> stdio` on the destination. Keep the returned
/// session handle alive while using the stream; [`RemoteExit`] explains why
/// the stream ended.
pub(crate) async fn open(
    identity: &Identity,
    destination: &Destination,
    bone: &str,
) -> Result<(tokio::io::DuplexStream, Handle<Pinned>, RemoteExit), String> {
    tokio::time::timeout(CONNECT_TIMEOUT, open_inner(identity, destination, bone))
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "timed out connecting to {}:{} after {}s",
                destination.host,
                destination.port,
                CONNECT_TIMEOUT.as_secs()
            ))
        })
}

async fn open_inner(
    identity: &Identity,
    destination: &Destination,
    bone: &str,
) -> Result<(tokio::io::DuplexStream, Handle<Pinned>, RemoteExit), String> {
    let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handler = Pinned {
        host: destination.host.clone(),
        port: destination.port,
        known_hosts: identity.dir.join(KNOWN_HOSTS_FILE),
        changed: changed.clone(),
    };
    let config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(15)),
        ..Default::default()
    });
    let address = (destination.host.as_str(), destination.port);
    let mut session = client::connect(config, address, handler)
        .await
        .map_err(|error| {
            if changed.load(std::sync::atomic::Ordering::Relaxed) {
                format!(
                    "the host key for {} changed; if expected, clear the app's known hosts",
                    destination.host
                )
            } else {
                format!(
                    "could not reach {}:{}: {error}",
                    destination.host, destination.port
                )
            }
        })?;
    let auth = session
        .authenticate_publickey(
            destination.user.clone(),
            PrivateKeyWithHashAlg::new(identity.key.clone(), None),
        )
        .await
        .map_err(|error| format!("login failed: {error}"))?;
    if !auth.success() {
        return Err(format!(
            "{}@{} rejected this app's key; add it to ~/.ssh/authorized_keys there",
            destination.user, destination.host
        ));
    }
    let channel = session
        .channel_open_session()
        .await
        .map_err(|error| format!("could not open a channel: {error}"))?;
    let command = format!("{} stdio", shell_path(bone));
    channel
        .exec(true, command.clone())
        .await
        .map_err(|error| format!("could not run `{command}`: {error}"))?;
    let (stream, exit) = pump(channel);
    Ok((stream, session, exit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_need_a_user_and_default_to_port_22() {
        assert_eq!(
            Destination::parse(" me@devbox ").unwrap(),
            Destination {
                user: "me".into(),
                host: "devbox".into(),
                port: 22
            }
        );
        assert_eq!(Destination::parse("me@10.0.0.2:2222").unwrap().port, 2222);
        for bad in ["devbox", "@devbox", "me@", "me@host:x", "me@a b"] {
            assert!(Destination::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn identity_is_created_once_and_reloaded() {
        let dir = std::env::temp_dir().join(format!("bone-android-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = Identity::load_or_create(&dir).unwrap();
        let line = first.public_line();
        assert!(
            line.starts_with("ssh-ed25519 ") && line.ends_with(" bone-android"),
            "{line}"
        );
        assert_eq!(Identity::load_or_create(&dir).unwrap().public_line(), line);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_key_pins_and_forgets_only_the_selected_destination() {
        let dir = std::env::temp_dir().join(format!(
            "bone-android-host-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let identity = Identity::load_or_create(&dir).unwrap();
        let known_hosts = dir.join(KNOWN_HOSTS_FILE);
        let first = PrivateKey::from(Ed25519Keypair::from_seed(&[1; 32]));
        let second = PrivateKey::from(Ed25519Keypair::from_seed(&[2; 32]));
        russh::keys::known_hosts::learn_known_hosts_path(
            "devbox",
            22,
            first.public_key(),
            &known_hosts,
        )
        .unwrap();
        russh::keys::known_hosts::learn_known_hosts_path(
            "otherbox",
            22,
            second.public_key(),
            &known_hosts,
        )
        .unwrap();

        let selected = Destination::parse("me@devbox").unwrap();
        let other = Destination::parse("me@otherbox").unwrap();
        assert!(identity.host_key_pinned(&selected));
        assert!(identity.host_key_pinned(&other));
        assert!(identity.forget_host_key(&selected).unwrap());
        assert!(!identity.host_key_pinned(&selected));
        assert!(identity.host_key_pinned(&other));
        assert!(!identity.forget_host_key(&selected).unwrap());

        // Comment lines must not shift which entries are removed.
        let text = std::fs::read_to_string(&known_hosts).unwrap();
        std::fs::write(
            &known_hosts,
            format!("# pinned by bone\n# another note\n{text}"),
        )
        .unwrap();
        russh::keys::known_hosts::learn_known_hosts_path(
            "devbox",
            22,
            first.public_key(),
            &known_hosts,
        )
        .unwrap();
        assert!(identity.forget_host_key(&selected).unwrap());
        assert!(identity.host_key_pinned(&other));
        assert!(!identity.host_key_pinned(&selected));
        let contents = std::fs::read_to_string(known_hosts).unwrap();
        assert!(!contents.contains("devbox"));
        assert!(contents.contains("otherbox"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pinned_host_rejects_any_other_key() {
        let dir = std::env::temp_dir().join(format!(
            "bone-android-pin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let known_hosts = dir.join(KNOWN_HOSTS_FILE);
        let first = PrivateKey::from(Ed25519Keypair::from_seed(&[1; 32]));
        let second = PrivateKey::from(Ed25519Keypair::from_seed(&[2; 32]));
        let decide = |host: &str, key: &PrivateKey| {
            pin_decision(host, 22, key.public_key(), &known_hosts).unwrap()
        };

        assert_eq!(decide("devbox", &first), PinDecision::Learn);
        russh::keys::known_hosts::learn_known_hosts_path(
            "devbox",
            22,
            first.public_key(),
            &known_hosts,
        )
        .unwrap();
        assert_eq!(decide("devbox", &first), PinDecision::Trusted);
        assert_eq!(decide("devbox", &second), PinDecision::Changed);
        assert_eq!(decide("otherbox", &second), PinDecision::Learn);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remote_exit_explains_stderr_and_status() {
        let exit = RemoteExit::default();
        assert_eq!(exit.describe(), None);
        exit.record(&ChannelMsg::ExtendedData {
            data: b"\nbash: bone: command not found\n".to_vec().into(),
            ext: 1,
        });
        exit.record(&ChannelMsg::ExtendedData {
            data: b"not stderr".to_vec().into(),
            ext: 2,
        });
        exit.record(&ChannelMsg::ExitStatus { exit_status: 127 });
        assert_eq!(
            exit.describe().as_deref(),
            Some("remote bone exited with status 127: bash: bone: command not found")
        );

        let clean = RemoteExit::default();
        clean.record(&ChannelMsg::ExitStatus { exit_status: 0 });
        assert_eq!(clean.describe(), None);

        let noisy = RemoteExit::default();
        noisy.record(&ChannelMsg::ExtendedData {
            data: vec![b'x'; 10_000].into(),
            ext: 1,
        });
        let text = noisy.describe().unwrap();
        assert!(text.starts_with("remote stderr: …"), "{text}");
        assert_eq!(
            text.chars().count(),
            "remote stderr: …".chars().count() + STDERR_SHOWN
        );
    }

    #[test]
    fn bone_paths_are_quoted_but_keep_tilde_expansion() {
        assert_eq!(shell_path("bone"), "'bone'");
        assert_eq!(shell_path("~/bin/my bone"), "~/'bin/my bone'");
        assert_eq!(shell_path("/opt/it's/bone"), "'/opt/it'\\''s/bone'");
        assert_eq!(shell_path("bone; rm -rf ~"), "'bone; rm -rf ~'");
        assert_eq!(shell_path("~"), "~");
    }

    #[test]
    fn atomic_writes_replace_contents_privately_without_leftovers() {
        let dir = std::env::temp_dir().join(format!("bone-android-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert!(!dir.join("settings.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
