//! The bone API.
//!
//! This crate is the only contract between the core and its clients. Every
//! frontend (TUI, CLI, scripts, editors) talks to the core through these types,
//! carried as JSON-RPC 2.0 over newline-delimited JSON.
//!
//! - [`message`]: the JSON-RPC envelope ([`Message`]).
//! - [`methods`]: typed requests ([`Method`]) and server events ([`Notification`]).
//! - [`codec`]: NDJSON encoding of messages.
//! - [`transport`]: a bidirectional [`Connection`] and the in-process transport.

pub mod codec;
pub mod message;
pub mod methods;
pub mod transport;
pub mod types;

pub use message::{Message, RequestId, RpcError};
pub use methods::{Method, Notification};
pub use transport::Connection;

/// Version of the protocol spoken by this build. Exchanged in `initialize`;
/// client and server must agree exactly until the protocol is stabilised.
pub const PROTOCOL_VERSION: u32 = 0;
