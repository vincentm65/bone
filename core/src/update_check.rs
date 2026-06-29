//! Throttled, non-blocking check for a newer release on GitHub.
//!
//! Mirrors the shape of `ext::catalog`: a background thread does one
//! `reqwest::blocking` GET (on a dedicated OS thread to avoid the nested
//! tokio runtime panic documented in `catalog.rs:76`), and the result lands
//! in a cache file so the banner can read it synchronously next launch
//! without ever blocking startup. Channel-agnostic: we only compare version
//! strings and point at the releases page; each install method (cargo, npm,
//! git, pkg) has its own command, so we suggest none.

use std::path::PathBuf;
use std::time::Duration;

const THROTTLE: Duration = Duration::from_secs(24 * 3600);
const TIMEOUT: Duration = Duration::from_secs(8);
const URL: &str = "https://api.github.com/repos/vincentm65/bone/releases/latest";

fn cache_dir() -> PathBuf {
    crate::config::bone_dir()
}

fn check_due() -> bool {
    let last = std::fs::read_to_string(cache_dir().join("update_checked_at"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    crate::util::now_secs().saturating_sub(last) >= THROTTLE.as_secs()
}

fn mark_checked() {
    let _ = std::fs::write(
        cache_dir().join("update_checked_at"),
        crate::util::now_secs().to_string(),
    );
}

/// Fetch the latest release tag once per day, off the main thread. Safe to
/// call at every interactive startup; no-ops if a check ran recently. Never
/// blocks: the banner reads the cached result via `latest_seen()`.
pub fn check_in_background() {
    if !check_due() {
        return;
    }
    std::thread::spawn(|| {
        let tag = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .ok()
            .and_then(|c| c.get(URL).header("User-Agent", "bone").send().ok())
            .and_then(|r| r.json::<serde_json::Value>().ok())
            .and_then(|v| v["tag_name"].as_str().map(str::to_string));
        if let Some(t) = tag {
            let _ = std::fs::write(cache_dir().join("update_latest"), t.trim_start_matches('v'));
        }
        // Mark checked regardless of success so offline launches don't retry
        // every time; the next window will try again.
        mark_checked();
    });
}

/// Cached latest version string, or None if unknown / never fetched. This is
/// a local file read — never blocks on network.
pub fn latest_seen() -> Option<String> {
    let s = std::fs::read_to_string(cache_dir().join("update_latest")).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// True if a newer release was seen (and cached). Convenience for the banner.
pub fn update_available() -> bool {
    latest_seen()
        .map(|v| v != env!("CARGO_PKG_VERSION"))
        .unwrap_or(false)
}
