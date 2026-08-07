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

/// Current UTC time in the canonical database timestamp format.
pub fn utc_now() -> String {
    utc_from_unix_secs(now_secs())
}

pub(crate) fn utc_from_unix_secs(secs: u64) -> String {
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Convert days since 1970-01-01 to (year, month, day).
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// Global lock for tests that mutate process-wide env vars (`BONE_DIR`,
/// `XDG_CONFIG_HOME`, …). Every such test must take this guard so parallel
/// `cargo test` threads do not clobber each other.
#[cfg(test)]
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
