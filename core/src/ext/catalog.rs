//! Catalog client.
//!
//! Optional tools and commands live in a separate `bone-catalog` repo served as
//! raw content (not embedded in the binary). This module fetches the catalog
//! index and downloads individual items on demand. Installed items and their
//! bundled files are written beneath `~/.bone-rust/lua/` — once on disk the
//! normal loader runs them like any user file. Updates are detected by comparing
//! file's sha256 against the catalog's, and surfaced to the user (`/catalog`
//! tag + startup hint); they're applied only when the user asks. Index entries
//! may also publish optional version, authorship, links, compatibility,
//! dependency, permission, and long-description metadata for catalog clients.
//!
//! All operations are offline-safe: a network failure falls back to whatever is
//! cached/installed and never errors out the app.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};

/// Default catalog location (raw GitHub content). Override with `BONE_CATALOG_URL`
/// — an `http(s)://` base or a local filesystem path (used by tests / dev).
const DEFAULT_URL: &str = "https://raw.githubusercontent.com/vincentm65/bone-catalog/main";

/// How often the background refresh actually hits the network.
const REFRESH_THROTTLE: Duration = Duration::from_secs(6 * 60 * 60);

/// One additional file installed and removed with its parent catalog item.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CatalogFile {
    /// Path relative to both the catalog root and `~/.bone-rust/lua/`, e.g.
    /// `"themes/nord.lua"`.
    pub path: String,
    #[serde(default)]
    pub sha256: String,
}

/// One catalog entry, as listed in `catalog.json`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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
    /// Published extension version. Numbers are accepted for compatibility with
    /// older catalog indexes and normalized to strings.
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub version: Option<String>,
    /// ISO 8601 publication or update date.
    #[serde(default, alias = "updated_date")]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// Source repository URL.
    #[serde(default, alias = "repository_url", alias = "repo_url")]
    pub repository: Option<String>,
    /// Documentation URL.
    #[serde(default, alias = "docs_url")]
    pub documentation: Option<String>,
    /// Minimum compatible Bone version or version requirement.
    #[serde(default, alias = "minimum_bone_version")]
    pub min_bone_version: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Additional files installed and removed with this visible catalog item.
    #[serde(default)]
    pub files: Vec<CatalogFile>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub long_description: Option<String>,
}

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| match value {
        serde_json::Value::String(value) => Some(value),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }))
}

impl CatalogEntry {
    fn validate(&self) -> Result<(), String> {
        if !super::is_safe_leaf_name(&self.name) || !self.name.ends_with(".lua") {
            return Err(format!(
                "invalid catalog name '{}': expected one .lua file name",
                self.name
            ));
        }
        if !matches!(self.kind.as_str(), "tool" | "command") {
            return Err(format!("invalid catalog kind '{}'", self.kind));
        }
        let primary_path = format!("{}/{}", self.dir_segment(), self.name);
        let mut paths = std::collections::HashSet::new();
        for file in &self.files {
            let Some((dir, name)) = file.path.split_once('/') else {
                return Err(format!(
                    "invalid bundled catalog path '{}': expected <kind>/<name>.lua",
                    file.path
                ));
            };
            if !matches!(dir, "tools" | "commands" | "themes")
                || !super::is_safe_leaf_name(name)
                || !name.ends_with(".lua")
                || file.path == primary_path
                || !paths.insert(file.path.as_str())
            {
                return Err(format!("invalid bundled catalog path '{}'", file.path));
            }
        }
        Ok(())
    }

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
    let entries: Vec<CatalogEntry> = serde_json::from_slice(bytes).ok()?;
    Some(
        entries
            .into_iter()
            .filter(|entry| match entry.validate() {
                Ok(()) => true,
                Err(error) => {
                    super::ctx::runtime_warn_once(format!("bone: warning: {error}; skipping"));
                    false
                }
            })
            .collect(),
    )
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

fn bundled_path(file: &CatalogFile) -> PathBuf {
    crate::config::bone_dir().join("lua").join(&file.path)
}

/// True if the item's primary file and all bundled files are present on disk.
pub fn is_installed(entry: &CatalogEntry) -> bool {
    entry.validate().is_ok()
        && lua_dir(entry).join(&entry.name).exists()
        && entry.files.iter().all(|file| bundled_path(file).exists())
}

/// Installed catalog commands that are not bundled defaults.
pub fn installed_command_names() -> std::collections::HashSet<String> {
    let bundled: std::collections::HashSet<&str> = super::DEFAULT_LUA_COMMANDS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    cached_index()
        .into_iter()
        .filter(|entry| entry.is_command() && !bundled.contains(entry.name.as_str()))
        .filter(is_installed)
        .map(|entry| entry.name.trim_end_matches(".lua").to_string())
        .collect()
}

fn bundled_sha256(entry: &CatalogEntry) -> Option<String> {
    let bundled = if entry.is_command() {
        super::DEFAULT_LUA_COMMANDS
    } else {
        super::DEFAULT_LUA_TOOLS
    };
    bundled
        .iter()
        .find(|(name, _)| *name == entry.name)
        .map(|(_, content)| sha256_hex(content.as_bytes()))
}

fn file_needs_update(path: &Path, expected: &str, bundled: Option<&str>) -> bool {
    if expected.is_empty() {
        return false;
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            let installed = sha256_hex(&bytes);
            !installed.eq_ignore_ascii_case(expected)
                && bundled.is_none_or(|hash| !installed.eq_ignore_ascii_case(hash))
        }
        Err(_) => false,
    }
}

/// True if any installed file differs from the catalog's current content.
pub fn needs_update(entry: &CatalogEntry) -> bool {
    if entry.validate().is_err() {
        return false;
    }
    let bundled = bundled_sha256(entry);
    file_needs_update(
        &lua_dir(entry).join(&entry.name),
        &entry.sha256,
        bundled.as_deref(),
    ) || entry
        .files
        .iter()
        .any(|file| file_needs_update(&bundled_path(file), &file.sha256, None))
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

/// Download and install a catalog item and any files bundled with it beneath
/// `~/.bone-rust/lua/`.
/// Verifies declared sha256 values before writing anything.
pub fn install(entry: &CatalogEntry) -> Result<(), String> {
    entry.validate()?;
    let primary_rel = format!("{}/{}", entry.dir_segment(), entry.name);
    let mut downloads = Vec::with_capacity(entry.files.len() + 1);
    downloads.push((
        primary_rel.clone(),
        lua_dir(entry).join(&entry.name),
        entry.sha256.as_str(),
    ));
    downloads.extend(
        entry
            .files
            .iter()
            .map(|file| (file.path.clone(), bundled_path(file), file.sha256.as_str())),
    );

    let downloads = downloads
        .into_iter()
        .map(|(rel, path, expected)| {
            let bytes = fetch(&base_url(), &rel)
                .ok_or_else(|| format!("could not download {rel} from catalog"))?;
            if !expected.is_empty() {
                let got = sha256_hex(&bytes);
                if !got.eq_ignore_ascii_case(expected) {
                    return Err(format!(
                        "checksum mismatch for {rel} (expected {expected}, got {got})"
                    ));
                }
            }
            Ok((path, bytes))
        })
        .collect::<Result<Vec<_>, String>>()?;

    for (path, bytes) in downloads {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        let permissions = std::fs::metadata(&path).ok().map(|meta| meta.permissions());
        crate::tools::write_atomic::write_atomic_sync(&path, &bytes, permissions)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Remove an installed catalog item and every file bundled with it.
pub fn remove(entry: &CatalogEntry) -> Result<(), String> {
    entry.validate()?;
    let mut paths = vec![lua_dir(entry).join(&entry.name)];
    paths.extend(entry.files.iter().map(bundled_path));
    for path in paths {
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
        }
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
