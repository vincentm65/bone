//! Catalog client.
//!
//! Optional extensions live in a separate `bone-catalog` repo served as raw
//! content (not embedded in the binary). This module fetches the catalog index
//! and downloads individual entries on demand. Every entry installs as a plugin
//! *package*: its primary file is written to
//! `~/.bone-rust/lua/plugins/<package>/init.lua`, and any extra files land inside
//! the same `plugins/<package>/` tree — once on disk the normal loader runs them
//! like any user package. `kind` (`"tool"`, `"command"`, or `"plugin"`) only
//! decides the catalog fetch path and the legacy flat location an entry may
//! still occupy on disk. Updates are detected by comparing each
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
    /// `"themes/nord.lua"` or a plugin's scoped `"plugins/core/lib/x.lua"`.
    pub path: String,
    #[serde(default)]
    pub sha256: String,
}

/// One catalog entry, as listed in `catalog.json`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CatalogEntry {
    /// File name (`"weather.lua"`) for a tool/command, or the package directory
    /// name (`"core"`) for a plugin.
    pub name: String,
    /// `"tool"`, `"command"`, or `"plugin"`. All install as plugin packages;
    /// the kind only decides the catalog fetch path and legacy locations.
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
        if !matches!(self.kind.as_str(), "tool" | "command" | "plugin") {
            return Err(format!("invalid catalog kind '{}'", self.kind));
        }
        if self.is_plugin() {
            // A plugin is a directory `plugins/<name>/` whose entry point is
            // `init.lua`; the name is the package directory, not a file.
            if !super::is_safe_leaf_name(&self.name) || self.name.ends_with(".lua") {
                return Err(format!(
                    "invalid catalog plugin name '{}': expected a package directory name",
                    self.name
                ));
            }
        } else if !super::is_safe_leaf_name(&self.name) || !self.name.ends_with(".lua") {
            return Err(format!(
                "invalid catalog name '{}': expected one .lua file name",
                self.name
            ));
        }
        let primary_rel = self.install_rel();
        let plugin_prefix = format!("plugins/{}/", self.package_name());
        let mut paths = std::collections::HashSet::new();
        for file in &self.files {
            let path = Path::new(&file.path);
            let is_safe_relative = !file.path.contains('\\')
                && file.path.split('/').count() >= 2
                && file
                    .path
                    .split('/')
                    .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
                && path
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)));
            // A plugin's bundled files must live inside its own package
            // directory; other kinds may publish files anywhere under `lua/`.
            let scoped = !self.is_plugin() || file.path.starts_with(&plugin_prefix);
            if !is_safe_relative
                || !scoped
                || file.path == primary_rel
                || !paths.insert(file.path.as_str())
            {
                return Err(format!("invalid bundled catalog path '{}'", file.path));
            }
        }
        Ok(())
    }

    fn is_plugin(&self) -> bool {
        self.kind == "plugin"
    }

    /// Directory segment under `lua/` and the catalog, e.g. `"tools"`.
    fn dir_segment(&self) -> &'static str {
        match self.kind.as_str() {
            "command" => "commands",
            "plugin" => "plugins",
            _ => "tools",
        }
    }

    /// Package directory name: a plugin's own directory, or a tool/command's
    /// file stem (`weather.lua` → `weather`).
    fn package_name(&self) -> &str {
        self.name.strip_suffix(".lua").unwrap_or(&self.name)
    }

    /// Primary file path relative to the catalog root. Tools and commands are
    /// single files (`tools/weather.lua`, `commands/memory.lua`); a plugin's
    /// entry point is its package entry (`plugins/<name>/init.lua`).
    fn catalog_rel(&self) -> String {
        if self.is_plugin() {
            format!("plugins/{}/init.lua", self.package_name())
        } else {
            format!("{}/{}", self.dir_segment(), self.name)
        }
    }

    /// Primary file path relative to `~/.bone-rust/lua/`. Every item installs as
    /// a plugin package, so tools/commands land at `plugins/<stem>/init.lua` and
    /// a plugin keeps its own directory (`plugins/<name>/init.lua`).
    fn install_rel(&self) -> String {
        format!("plugins/{}/init.lua", self.package_name())
    }

    /// Absolute path of the item's primary file beneath `~/.bone-rust/lua/`.
    fn primary_path(&self) -> PathBuf {
        crate::config::bone_dir()
            .join("lua")
            .join(self.install_rel())
    }

    /// Absolute path of the item's plugin package directory.
    fn package_dir(&self) -> PathBuf {
        crate::config::bone_dir()
            .join("lua/plugins")
            .join(self.package_name())
    }

    /// Legacy flat-file locations of the item's primary file, from when tools
    /// and commands lived as bare files (`lua/tools/<name>.lua`,
    /// `lua/commands/<name>.lua`). A plugin maps to both, since it may have been
    /// published earlier as either kind.
    fn legacy_primary_paths(&self) -> Vec<PathBuf> {
        let lua = crate::config::bone_dir().join("lua");
        if self.is_plugin() {
            vec![
                lua.join("tools").join(format!("{}.lua", self.name)),
                lua.join("commands").join(format!("{}.lua", self.name)),
            ]
        } else {
            vec![lua.join(self.dir_segment()).join(&self.name)]
        }
    }

    /// Legacy flat-file location of a plugin's bundled file: the scoped path
    /// with its `plugins/<name>/` prefix removed (e.g.
    /// `plugins/skill/lib/skill.lua` → `lua/lib/skill.lua`). `None` when the
    /// file is not part of the package.
    fn legacy_bundled_path(&self, file: &CatalogFile) -> Option<PathBuf> {
        let prefix = format!("plugins/{}/", self.package_name());
        file.path
            .strip_prefix(&prefix)
            .map(|scoped| crate::config::bone_dir().join("lua").join(scoped))
    }

    /// Whether this Bone build satisfies the entry's `min_bone_version`.
    ///
    /// A bare full version (`"2.4.5"`) is a minimum ("at least 2.4.5"), not a
    /// caret range, so it never blocks a newer build. Unparseable requirements
    /// (or a non-parseable local version) pass through: version metadata must
    /// never block an install.
    pub fn bone_version_ok(&self) -> Result<(), String> {
        let Some(required) = self.min_bone_version.as_deref() else {
            return Ok(());
        };
        let Ok(current) = semver::Version::parse(crate::build_info::VERSION) else {
            return Ok(());
        };
        let requirement = match semver::Version::parse(required) {
            Ok(version) => {
                // semver parses a bare version as a caret requirement
                // (`^2.4` → `<3.0.0`), which would wrongly reject future
                // builds; normalize to `>=`.
                semver::VersionReq::parse(&format!(">={version}")).ok()
            }
            Err(_) => semver::VersionReq::parse(required).ok(),
        };
        let Some(requirement) = requirement else {
            return Ok(());
        };
        if requirement.matches(&current) {
            Ok(())
        } else {
            Err(format!(
                "requires Bone {required} (current {})",
                crate::build_info::VERSION
            ))
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

/// Fetch `rel` (e.g. `"catalog.json"`, `"tools/weather.lua"`) from the catalog
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
pub(crate) fn cached_index() -> Vec<CatalogEntry> {
    std::fs::read(cache_dir().join("catalog.json"))
        .ok()
        .and_then(|b| parse_index(&b))
        .unwrap_or_default()
}

// ---- install state & update detection -----------------------------------

fn bundled_path(file: &CatalogFile) -> PathBuf {
    crate::config::bone_dir().join("lua").join(&file.path)
}

/// True if the item's primary file is present in the current layout, or (for
/// plugins) in the legacy flat layout awaiting migration.
fn primary_present(entry: &CatalogEntry) -> bool {
    entry.primary_path().exists()
        || entry
            .legacy_primary_paths()
            .iter()
            .any(|path| path.exists())
}

/// True if a bundled file is present in the current layout, or (for plugins)
/// in its legacy flat location.
fn bundled_present(entry: &CatalogEntry, file: &CatalogFile) -> bool {
    bundled_path(file).exists()
        || entry
            .legacy_bundled_path(file)
            .is_some_and(|path| path.exists())
}

/// True if the item's primary file and all bundled files are present on disk,
/// in either the current or (for plugins) the legacy flat layout.
pub fn is_installed(entry: &CatalogEntry) -> bool {
    entry.validate().is_ok()
        && primary_present(entry)
        && entry.files.iter().all(|file| bundled_present(entry, file))
}

/// True if any file managed by the item is present on disk (current or
/// legacy layout).
pub fn has_installed_files(entry: &CatalogEntry) -> bool {
    entry.validate().is_ok()
        && (primary_present(entry) || entry.files.iter().any(|file| bundled_present(entry, file)))
}

fn bundled_sha256(entry: &CatalogEntry) -> Option<String> {
    // The bundled default for an install rel `plugins/<name>/init.lua` is keyed
    // by its package-relative path (`<name>/init.lua`).
    let key = entry.install_rel();
    let key = key.strip_prefix("plugins/")?;
    super::DEFAULT_LUA_PLUGINS
        .iter()
        .find(|(name, _)| *name == key)
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

/// True if any installed file differs from the catalog's current content, or
/// (for plugins) if any file still sits in the legacy flat layout awaiting
/// migration by [`install`].
pub fn needs_update(entry: &CatalogEntry) -> bool {
    if entry.validate().is_err() {
        return false;
    }
    let bundled = bundled_sha256(entry);
    file_needs_update(&entry.primary_path(), &entry.sha256, bundled.as_deref())
        || entry
            .files
            .iter()
            .any(|file| file_needs_update(&bundled_path(file), &file.sha256, None))
        || entry
            .legacy_primary_paths()
            .iter()
            .any(|path| path.exists())
        || entry.files.iter().any(|file| {
            entry
                .legacy_bundled_path(file)
                .is_some_and(|path| path.exists())
        })
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

/// Download and install a catalog entry and any files bundled with it under
/// `~/.bone-rust/lua/plugins/<package>/`.
/// Verifies declared sha256 values before writing anything.
///
/// Plugin entries previously installed under the legacy flat layout
/// (`lua/tools/<name>.lua`, `lua/lib/…`, …) are migrated in place: a legacy
/// file's bytes are moved into the package directory — preserving any user
/// edits, so that file is not re-downloaded — and the leftover legacy path is
/// deleted. Files already present in the package are updated from the catalog
/// as usual.
pub fn install(entry: &CatalogEntry) -> Result<(), String> {
    entry.validate()?;

    // (destination, bytes) moved from a legacy flat file — no download.
    let mut writes: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    // (destination, catalog rel path, expected sha) to download and verify.
    let mut downloads: Vec<(PathBuf, String, &str)> = Vec::new();

    let primary_dest = entry.primary_path();
    if !primary_dest.exists()
        && let Some(legacy) = entry
            .legacy_primary_paths()
            .into_iter()
            .find(|path| path.exists())
    {
        let bytes = std::fs::read(&legacy)
            .map_err(|e| format!("could not read legacy file {}: {e}", legacy.display()))?;
        writes.push((primary_dest, bytes));
    } else {
        downloads.push((primary_dest, entry.catalog_rel(), entry.sha256.as_str()));
    }

    for file in &entry.files {
        let dest = bundled_path(file);
        if !dest.exists()
            && let Some(bytes) = entry
                .legacy_bundled_path(file)
                .filter(|legacy| legacy.exists())
                .and_then(|legacy| std::fs::read(&legacy).ok())
        {
            writes.push((dest, bytes));
        } else {
            downloads.push((dest, file.path.clone(), file.sha256.as_str()));
        }
    }

    for (dest, rel, expected) in downloads {
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
        writes.push((dest, bytes));
    }

    for (path, bytes) in writes {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        let permissions = std::fs::metadata(&path).ok().map(|meta| meta.permissions());
        crate::tools::write_atomic::write_atomic_sync(&path, &bytes, permissions)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }

    // Sweep the legacy flat files the migration consumed (or that shadow a
    // freshly written package file) so old and new layouts never coexist.
    for legacy in entry.legacy_primary_paths().into_iter().chain(
        entry
            .files
            .iter()
            .filter_map(|file| entry.legacy_bundled_path(file)),
    ) {
        if legacy.exists()
            && let Err(e) = std::fs::remove_file(&legacy)
        {
            super::ctx::runtime_warn(format!(
                "bone: warning: could not remove legacy file {}: {e}",
                legacy.display()
            ));
        }
    }
    Ok(())
}

/// Remove an installed catalog item and every file bundled with it — in the
/// current layout and, for plugins, any leftover legacy flat files. For a
/// plugin, the now-empty package directories left behind are pruned.
pub fn remove(entry: &CatalogEntry) -> Result<(), String> {
    entry.validate()?;
    let mut paths = vec![entry.primary_path()];
    paths.extend(entry.files.iter().map(bundled_path));
    paths.extend(entry.legacy_primary_paths());
    paths.extend(
        entry
            .files
            .iter()
            .filter_map(|file| entry.legacy_bundled_path(file)),
    );
    for path in paths {
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
        }
    }
    prune_empty_dirs(&entry.package_dir());
    Ok(())
}

/// Remove `root` and any subdirectories that are empty, deepest first. Files
/// that remain (e.g. user-authored additions) keep their directories in place.
fn prune_empty_dirs(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    let mut dirs = Vec::new();
    while let Some(dir) = stack.pop() {
        dirs.push(dir.clone());
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
    }
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.components().count()));
    for dir in dirs {
        let _ = std::fs::remove_dir(&dir);
    }
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
