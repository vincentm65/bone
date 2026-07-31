use super::{
    InstallKind, THROTTLE, cache_file, cargo_source_path_from_metadata,
    cargo_source_root_from_home, check_due_from, detect_install_kind_from, is_newer_version,
    source_update_commands,
};
use std::path::PathBuf;

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
    assert!(is_newer_version("2.5.0", "2.5.0-beta.1"));
    assert!(is_newer_version("2.5.0-beta.2", "2.5.0-beta.1"));
    assert!(!is_newer_version("2.5.0-beta.1", "2.5.0"));
    assert!(!is_newer_version("not-a-version", "2.5.0"));
}

#[test]
fn source_update_commands_work_in_windows_shells() {
    let commands = source_update_commands(&PathBuf::from(r"C:\Users\Vincent Miranda\bone"), true);
    assert_eq!(
        commands,
        concat!(
            "git -C \"C:\\Users\\Vincent Miranda\\bone\" pull --ff-only\n",
            "cargo install --path \"C:\\Users\\Vincent Miranda\\bone\\tui\" --force"
        )
    );
    assert!(!commands.contains("&&"));
    assert!(!commands.contains("cd "));
}

#[test]
fn reads_source_checkout_from_cargo_install_metadata() {
    let metadata = r#"{
        "installs": {
            "other 1.0.0 (registry+https://example.invalid)": {"bins":["other"]},
            "bone 2.4.3 (path+file:///tmp/bone/tui)": {"bins":["bone"]}
        }
    }"#;
    assert_eq!(
        cargo_source_path_from_metadata(metadata),
        Some(PathBuf::from("/tmp/bone/tui"))
    );
}

#[test]
fn resolves_cargo_install_back_to_its_git_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path().join("bone");
    let tui = checkout.join("tui");
    let cargo_home = dir.path().join("cargo-home");
    std::fs::create_dir_all(checkout.join(".git")).unwrap();
    std::fs::create_dir_all(&tui).unwrap();
    std::fs::create_dir_all(&cargo_home).unwrap();
    let source = reqwest::Url::from_directory_path(&tui).unwrap();
    let key = format!("bone 2.4.3 (path+{source})");
    let metadata = serde_json::json!({ "installs": { (key): { "bins": ["bone"] } } });
    std::fs::write(
        cargo_home.join(".crates2.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();

    assert_eq!(cargo_source_root_from_home(&cargo_home), Some(checkout));
}

#[test]
fn detects_native_binary_nested_in_an_npm_platform_package() {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("node_modules/bone-agent-linux-x64");
    let executable = package.join("bin/bone");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"name":"bone-agent-linux-x64"}"#,
    )
    .unwrap();

    assert_eq!(detect_install_kind_from(&executable), InstallKind::Npm);
}

#[test]
fn update_notices_prefer_the_in_app_command_when_supported() {
    assert_eq!(
        InstallKind::Npm.notice("2.5.0"),
        "bone 2.5.0 available — run /update or `bone update`"
    );
    assert!(InstallKind::Npm.can_apply());
    assert_eq!(
        InstallKind::Cargo(Some(PathBuf::from("/tmp/bone"))).can_apply(),
        !cfg!(windows)
    );
    assert!(!InstallKind::Cargo(None).can_apply());
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
