//! Throttled, non-blocking check for newer app releases.

use std::path::{Path, PathBuf};
use std::time::Duration;

const THROTTLE: Duration = Duration::from_secs(24 * 3600);
const TIMEOUT: Duration = Duration::from_secs(8);
const GITHUB_URL: &str = "https://api.github.com/repos/vincentm65/bone/releases/latest";
const NPM_URL: &str = "https://registry.npmjs.org/bone-agent/latest";

#[derive(Clone, Debug, PartialEq, Eq)]
enum InstallKind {
    Npm,
    Git(PathBuf),
    Unknown,
}

impl InstallKind {
    fn key(&self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Git(_) | Self::Unknown => "github",
        }
    }

    fn url(&self) -> &'static str {
        match self {
            Self::Npm => NPM_URL,
            Self::Git(_) | Self::Unknown => GITHUB_URL,
        }
    }

    fn latest_from_json(&self, v: &serde_json::Value) -> Option<String> {
        match self {
            Self::Npm => v["version"].as_str().map(str::to_string),
            Self::Git(_) | Self::Unknown => v["tag_name"]
                .as_str()
                .map(|s| s.trim_start_matches('v').to_string()),
        }
    }

    fn notice(&self, latest: &str) -> String {
        match self {
            Self::Unknown => {
                format!("bone {latest} available — https://github.com/vincentm65/bone/releases")
            }
            _ => format!(
                "bone {latest} available — update with: {}",
                self.update_hint()
            ),
        }
    }

    fn update_hint(&self) -> String {
        match self {
            Self::Npm => "npm install -g bone-agent@latest".to_string(),
            Self::Git(root) => format!(
                "cd {} && git pull && cargo install --path tui --force",
                shell_quote(root)
            ),
            Self::Unknown => "https://github.com/vincentm65/bone/releases".to_string(),
        }
    }
}

fn cache_dir() -> PathBuf {
    crate::config::bone_dir()
}

fn cache_file(kind: &InstallKind, suffix: &str) -> PathBuf {
    cache_dir().join(format!("update_{}_{}", kind.key(), suffix))
}

fn check_due(kind: &InstallKind) -> bool {
    let last = std::fs::read_to_string(cache_file(kind, "checked_at"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    crate::util::now_secs().saturating_sub(last) >= THROTTLE.as_secs()
}

fn mark_checked(kind: &InstallKind) {
    let _ = std::fs::write(
        cache_file(kind, "checked_at"),
        crate::util::now_secs().to_string(),
    );
}

fn detect_install_kind() -> InstallKind {
    let exe = std::env::current_exe().ok();
    let exe = exe
        .as_ref()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .or(exe);
    exe.as_deref()
        .map(detect_install_kind_from)
        .unwrap_or(InstallKind::Unknown)
}

fn detect_install_kind_from(exe: &Path) -> InstallKind {
    for dir in exe.ancestors() {
        let package_json = dir.join("package.json");
        if std::fs::read_to_string(&package_json)
            .ok()
            .is_some_and(|s| s.contains("\"name\"") && s.contains("\"bone-agent"))
        {
            return InstallKind::Npm;
        }
        if dir.join(".git").exists() {
            return InstallKind::Git(dir.to_path_buf());
        }
    }
    InstallKind::Unknown
}

/// Fetch the latest release once per day, off the main thread. Safe to call at
/// every interactive startup; the banner reads the cached result next launch.
pub fn check_in_background() {
    let kind = detect_install_kind();
    if !check_due(&kind) {
        return;
    }
    std::thread::spawn(move || {
        let latest = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .ok()
            .and_then(|c| c.get(kind.url()).header("User-Agent", "bone").send().ok())
            .and_then(|r| r.json::<serde_json::Value>().ok())
            .and_then(|v| kind.latest_from_json(&v));
        if let Some(version) = latest {
            let _ = std::fs::write(cache_file(&kind, "latest"), version.trim());
        }
        mark_checked(&kind);
    });
}

fn latest_seen(kind: &InstallKind) -> Option<String> {
    let s = std::fs::read_to_string(cache_file(kind, "latest")).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// User-facing update notice for the startup banner, if this install is behind.
pub fn notice() -> Option<String> {
    let kind = detect_install_kind();
    let latest = latest_seen(&kind)?;
    is_newer_version(&latest, env!("CARGO_PKG_VERSION")).then(|| kind.notice(&latest))
}

fn is_newer_version(latest: &str, current: &str) -> bool {
    version_key(latest) > version_key(current)
}

fn version_key(s: &str) -> [u64; 3] {
    let mut out = [0, 0, 0];
    for (i, part) in s
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .take(3)
        .enumerate()
    {
        out[i] = part.parse().unwrap_or(0);
    }
    out
}

fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::{is_newer_version, shell_quote};
    use std::path::Path;

    #[test]
    fn compares_versions_as_versions() {
        assert!(is_newer_version("2.10.0", "2.9.9"));
        assert!(is_newer_version("v3.0.0", "2.9.9"));
        assert!(!is_newer_version("2.2.4", "2.2.4"));
        assert!(!is_newer_version("2.2.0", "2.2"));
        assert!(!is_newer_version("2.0.9", "2.2.4"));
    }

    #[test]
    fn quotes_update_paths_for_shell() {
        assert_eq!(shell_quote(Path::new("/tmp/bone")), "/tmp/bone");
        assert_eq!(shell_quote(Path::new("/tmp/my bone")), "'/tmp/my bone'");
    }
}
