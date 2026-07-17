use super::{InstallKind, THROTTLE, cache_file, check_due_from, is_newer_version, shell_quote};
use serde_json::json;
use std::path::{Path, PathBuf};

fn without_config_dir<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = crate::util::test_env_lock();
    let old_bone = std::env::var_os("BONE_DIR");
    let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
    let old_home = std::env::var_os("HOME");
    let old_userprofile = std::env::var_os("USERPROFILE");
    unsafe {
        std::env::remove_var("BONE_DIR");
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("HOME");
        std::env::remove_var("USERPROFILE");
    }
    let result = f();
    unsafe {
        for (key, value) in [
            ("BONE_DIR", old_bone),
            ("XDG_CONFIG_HOME", old_xdg),
            ("HOME", old_home),
            ("USERPROFILE", old_userprofile),
        ] {
            if let Some(value) = value {
                std::env::set_var(key, value);
            }
        }
    }
    result
}

#[test]
fn missing_config_dir_disables_update_cache() {
    let cached = without_config_dir(|| cache_file(&InstallKind::Unknown, "latest"));
    assert_eq!(cached, None);
}

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

#[test]
fn reads_latest_from_github_tags_or_release_json() {
    let kind = InstallKind::Git(PathBuf::from("/tmp/bone"));
    assert_eq!(
        kind.latest_from_json(&json!([{ "name": "v2.2.7" }, { "name": "v2.2.8" }]))
            .as_deref(),
        Some("2.2.8")
    );
    assert_eq!(
        kind.latest_from_json(&json!({ "tag_name": "v2.2.9" }))
            .as_deref(),
        Some("2.2.9")
    );
}

#[test]
fn reads_latest_from_npm_json() {
    assert_eq!(
        InstallKind::Npm
            .latest_from_json(&json!({ "version": "2.2.8" }))
            .as_deref(),
        Some("2.2.8")
    );
}

#[test]
fn rechecks_when_cache_does_not_show_stale_binary() {
    let now = THROTTLE.as_secs() + 10_000;
    let recent = now - 10;
    assert!(check_due_from(None, recent, "2.2.7", now));
    assert!(check_due_from(Some("2.2.7"), recent, "2.2.7", now));
    assert!(check_due_from(Some("2.2.6"), recent, "2.2.7", now));
    assert!(!check_due_from(Some("2.2.8"), recent, "2.2.7", now));
    assert!(check_due_from(
        Some("2.2.8"),
        now - THROTTLE.as_secs(),
        "2.2.7",
        now
    ));
}
