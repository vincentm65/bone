//! Compatibility re-exports for the shared client transport.
pub use bone_client::{MAX_LINE_BYTES, MessageReader, ReadError, write_message};

#[cfg(test)]
#[path = "codec_tests.rs"]
mod codec_tests;
