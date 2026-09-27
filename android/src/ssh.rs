//! In-app SSH (russh), since Android has no `ssh` program.
//!
//! The app owns one ed25519 key, created on first use in its data directory;
//! the user adds its public line to `~/.ssh/authorized_keys` on the computer.
//! Host keys are trusted on first use and pinned in `<data>/known_hosts`; a
//! changed key refuses the connection. The channel runs `<bone> stdio`, whose
//! stream is the same newline-JSON transport as every other client.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::ChannelStream;
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
                write_private(&path, pem.as_bytes())?;
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
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
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

impl client::Handler for Pinned {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        match russh::keys::check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(true),
            Ok(false) => {
                russh::keys::known_hosts::learn_known_hosts_path(
                    &self.host,
                    self.port,
                    key,
                    &self.known_hosts,
                )?;
                Ok(true)
            }
            Err(_) => {
                self.changed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(false)
            }
        }
    }
}

/// Log in and start `<bone> stdio` on the destination. Keep the returned
/// session handle alive while using the stream.
pub(crate) async fn open(
    identity: &Identity,
    destination: &Destination,
    bone: &str,
) -> Result<(ChannelStream<Msg>, Handle<Pinned>), String> {
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
    channel
        .exec(true, format!("{bone} stdio"))
        .await
        .map_err(|error| format!("could not run `{bone} stdio`: {error}"))?;
    Ok((channel.into_stream(), session))
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
}
