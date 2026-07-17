//! Tiny shared helpers with no dependencies on other crate modules.

/// Convert anything displayable into a `String`.
///
/// Exists as a named function so it can be passed directly to [`map_err`]
/// without an inline closure, replacing the dozens of `.map_err(crate::util::errstr)`
/// sites across the codebase:
///
/// ```ignore
/// fs::read_to_string(&p).await.map_err(crate::util::errstr)?
/// ```
///
/// Every error type used here implements [`Display`] (and thus [`ToString`]),
/// so this is behaviourally identical to the closure it replaces.
///
/// [`map_err`]: Result::map_err
/// [`Display`]: std::fmt::Display
pub fn errstr<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Current Unix timestamp in seconds (best-effort; 0 on clock skew).
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Global lock for tests that mutate process-wide env vars (`BONE_DIR`,
/// `XDG_CONFIG_HOME`, …). Every such test must take this guard so parallel
/// `cargo test` threads do not clobber each other.
#[cfg(test)]
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
