//! Catalog client.
//!
//! Optional tools and commands live in a separate `bone-catalog` repo served as
//! raw content (not embedded in the binary). This module fetches the catalog
//! index and downloads individual items on demand. Installed items are written
//! into `~/.bone-rust/lua/{tools,commands}/` — once on disk the normal loader
//! runs them like any user file. Updates are detected by comparing the on-disk
//! file's sha256 against the catalog's, and surfaced to the user (`/catalog`
//! tag + startup hint); they're applied only when the user asks.
//!
//! All operations are offline-safe: a network failure falls back to whatever is
//! cached/installed and never errors out the app.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Default catalog location (raw GitHub content). Override with `BONE_CATALOG_URL`
/// — an `http(s)://` base or a local filesystem path (used by tests / dev).
const DEFAULT_URL: &str =
    "https://raw.githubusercontent.com/vincentm65/bone-catalog/refs/heads/main";

/// How often the background refresh actually hits the network.
const REFRESH_THROTTLE: Duration = Duration::from_secs(6 * 60 * 60);

/// One catalog entry, as listed in `catalog.json`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CatalogEntry {
    /// File name, e.g. `"browser.lua"`.
    pub name: String,
    /// `"tool"` or `"command"`.
    pub kind: String,
    #[serde(default)]
    pub description: String,
    /// Hex sha256 of the file bytes. Drives both integrity verification and
    /// update detection; empty disables both.
    #[serde(default)]
    pub sha256: String,
}

impl CatalogEntry {
    fn is_command(&self) -> bool {
        self.kind == "command"
    }

    /// Directory segment under `lua/` and the catalog, e.g. `"tools"`.
    fn dir_segment(&self) -> &'static str {
        if self.is_command() {
            "commands"
        } else {
            "tools"
        }
    }
}

/// The configured base URL or path.
pub fn base_url() -> String {
    std::env::var("BONE_CATALOG_URL").unwrap_or_else(|_| DEFAULT_URL.to_string())
}

fn is_remote(base: &str) -> bool {
    base.starts_with("http://") || base.starts_with("https://")
}

/// Fetch `rel` (e.g. `"catalog.json"`, `"tools/browser.lua"`) from the catalog
/// base. Returns the raw bytes, or `None` on any failure.
fn fetch(base: &str, rel: &str) -> Option<Vec<u8>> {
    if is_remote(base) {
        let url = format!("{}/{}", base.trim_end_matches('/'), rel);
        // `reqwest::blocking` builds its own current-thread runtime; doing that
        // inside bone's async runtime (the TUI / onboarding both run under
        // `#[tokio::main]`) panics when that nested runtime drops. Run the GET on
        // a dedicated OS thread so it never nests in an async context.
        std::thread::spawn(move || fetch_remote(&url))
            .join()
            .ok()
            .flatten()
    } else {
        std::fs::read(Path::new(base).join(rel)).ok()
    }
}

/// Blocking HTTP GET. Must run on a thread with no ambient tokio runtime.
fn fetch_remote(url: &str) -> Option<Vec<u8>> {
    // Short connect timeout so an offline first-launch onboarding (which fetches
    // the index synchronously) doesn't hang.
    let resp = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(15))
        .build()
        .ok()?
        .get(url)
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.bytes().ok().map(|b| b.to_vec())
}

fn cache_dir() -> PathBuf {
    crate::config::bone_dir().join("cache/catalog")
}

fn lua_dir(entry: &CatalogEntry) -> PathBuf {
    crate::config::bone_dir()
        .join("lua")
        .join(entry.dir_segment())
}

fn parse_index(bytes: &[u8]) -> Option<Vec<CatalogEntry>> {
    serde_json::from_slice(bytes).ok()
}

/// Fetch the catalog index. On success the result is cached; on network
/// failure the cached copy is returned; if neither is available, an empty list.
pub fn fetch_index() -> Vec<CatalogEntry> {
    let cache = cache_dir().join("catalog.json");
    if let Some(bytes) = fetch(&base_url(), "catalog.json")
        && let Some(entries) = parse_index(&bytes)
    {
        let _ = std::fs::create_dir_all(cache_dir());
        let _ = std::fs::write(&cache, &bytes);
        return entries;
    }
    std::fs::read(&cache)
        .ok()
        .and_then(|b| parse_index(&b))
        .unwrap_or_default()
}

/// Blocking index refresh used before building a picker (onboarding / `/catalog`).
pub fn sync_quiet() -> Vec<CatalogEntry> {
    fetch_index()
}

/// Read the cached index only (no network). Returns an empty list if nothing is
/// cached yet.
fn cached_index() -> Vec<CatalogEntry> {
    std::fs::read(cache_dir().join("catalog.json"))
        .ok()
        .and_then(|b| parse_index(&b))
        .unwrap_or_default()
}

// ---- install state & update detection -----------------------------------

/// True if the item's file is present on disk.
pub fn is_installed(entry: &CatalogEntry) -> bool {
    lua_dir(entry).join(&entry.name).exists()
}

/// True if the on-disk copy differs from the catalog's current content.
///
/// Detection is purely content-based: the catalog publishes the sha256 of each
/// file, and we hash whatever is installed. An empty `sha256` (no hash
/// published) disables detection and returns `false`, so the feature stays dark
/// until the catalog ships hashes — never a false positive.
pub fn needs_update(entry: &CatalogEntry) -> bool {
    if entry.sha256.is_empty() {
        return false;
    }
    let path = lua_dir(entry).join(&entry.name);
    match std::fs::read(&path) {
        Ok(bytes) => !sha256_hex(&bytes).eq_ignore_ascii_case(&entry.sha256),
        Err(_) => false,
    }
}

/// Number of installed items with a newer version available, read from the
/// cached index only (no network) so callers like the startup banner never
/// block.
pub fn updates_available() -> usize {
    cached_index()
        .iter()
        .filter(|e| is_installed(e) && needs_update(e))
        .count()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Download and install a catalog item into `~/.bone-rust/lua/{tools,commands}/`.
/// Verifies the sha256 when the entry declares one. Returns an error string on
/// failure (caller decides whether to surface it).
pub fn install(entry: &CatalogEntry) -> Result<(), String> {
    let rel = format!("{}/{}", entry.dir_segment(), entry.name);
    let bytes = fetch(&base_url(), &rel)
        .ok_or_else(|| format!("could not download {} from catalog", entry.name))?;

    if !entry.sha256.is_empty() {
        let got = sha256_hex(&bytes);
        if !got.eq_ignore_ascii_case(&entry.sha256) {
            return Err(format!(
                "checksum mismatch for {} (expected {}, got {got})",
                entry.name, entry.sha256
            ));
        }
    }

    let dir = lua_dir(entry);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(&entry.name);
    std::fs::write(&path, &bytes)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(())
}

/// Remove an installed catalog item (delete the file, forget its version).
pub fn remove(entry: &CatalogEntry) -> Result<(), String> {
    let path = lua_dir(entry).join(&entry.name);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
    }
    Ok(())
}

// ---- background refresh --------------------------------------------------

fn last_refresh_path() -> PathBuf {
    cache_dir().join("last_refresh")
}

fn refresh_due() -> bool {
    let last = std::fs::read_to_string(last_refresh_path())
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    crate::util::now_secs().saturating_sub(last) >= REFRESH_THROTTLE.as_secs()
}

fn mark_refreshed() {
    let _ = std::fs::create_dir_all(cache_dir());
    let _ = std::fs::write(last_refresh_path(), crate::util::now_secs().to_string());
}

/// Refresh the cached index so update detection and the startup hint reflect
/// the latest catalog. Installs nothing — updates are applied only when the
/// user does so in `/catalog`. Blocking; intended for a background thread.
pub fn refresh_now() {
    let _ = fetch_index();
    mark_refreshed();
}

/// Spawn a throttled, non-blocking background refresh. Safe to call at every
/// interactive startup; it no-ops if a refresh ran within the throttle window.
pub fn refresh_in_background() {
    if !refresh_due() {
        return;
    }
    std::thread::spawn(refresh_now);
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
