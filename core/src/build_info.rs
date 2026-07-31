//! Compile-time identity for diagnosing installed binaries.

/// Package version shared by every workspace crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Source revision supplied by release CI or discovered by `build.rs`.
pub const GIT_SHA: &str = env!("BONE_GIT_SHA");
/// Rust target triple used to compile this binary.
pub const TARGET: &str = env!("BONE_BUILD_TARGET");
/// Cargo build profile (`debug` or `release`).
pub const PROFILE: &str = env!("BONE_BUILD_PROFILE");
/// Distribution channel (`stable`, `next`, or `dev`).
pub const CHANNEL: &str = env!("BONE_BUILD_CHANNEL");

#[must_use]
pub fn summary() -> String {
    format!("bone {VERSION}")
}

#[must_use]
pub fn verbose() -> String {
    format!(
        "{}\ncommit: {GIT_SHA}\ntarget: {TARGET}\nprofile: {PROFILE}\nchannel: {CHANNEL}",
        summary()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_identity_contains_every_build_dimension() {
        let value = verbose();
        assert!(value.starts_with(&format!("bone {VERSION}\n")));
        for label in ["commit:", "target:", "profile:", "channel:"] {
            assert!(value.lines().any(|line| line.starts_with(label)), "{value}");
        }
        assert!(!GIT_SHA.is_empty());
        assert!(!TARGET.is_empty());
    }
}
