use super::*;

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "bone-settings-{name}-{}-{}.yaml",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn validates_version_unknown_keys_and_values() {
    let bad_version = "version: 2\n";
    let parsed: BoneSettings = serde_yaml::from_str(bad_version).unwrap();
    assert_eq!(parsed.version, 2);

    assert!(serde_yaml::from_str::<BoneSettings>("version: 1\nunknown: true\n").is_err());
    assert!(validate_approval("prompt").is_err());
    assert!(parse_bool("maybe").is_err());
    assert!(
        validate_keymaps(&KeymapSettings {
            bindings: vec![KeyBinding {
                key: String::new(),
                action: "quit".into(),
            }],
        })
        .is_err()
    );
    assert!(
        validate_keymaps(&KeymapSettings {
            bindings: vec![
                KeyBinding {
                    key: "<C-p>".into(),
                    action: "one".into(),
                },
                KeyBinding {
                    key: "<C-p>".into(),
                    action: "two".into(),
                },
            ],
        })
        .is_err()
    );
}

#[test]
fn atomically_persists_and_loads_values_only_yaml() {
    let path = temp_path("roundtrip");
    let mut settings = Settings::defaults();
    settings.inner.general.approval = "danger".into();
    settings.inner.ui.input.preset = Some("box".into());
    settings.save_path(&path).unwrap();
    settings.inner.general.approval = "safe".into();
    settings.save_path(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let loaded = Settings::load_path(&path).unwrap().unwrap();
    assert_eq!(loaded.inner.general.approval, "safe");
    assert_eq!(loaded.inner.ui.input.preset.as_deref(), Some("box"));
    let raw = fs::read_to_string(&path).unwrap();
    assert!(raw.contains("version: 2"));
    assert!(!raw.contains("subagents:"));
    assert!(!raw.contains("Report file and line references."));
    assert!(!raw.contains("label:"));
    assert!(!raw.contains("null"));
    assert!(!raw.contains("show_reasoning"));
    assert!(!raw.contains("theme:"));
    assert!(
        raw.lines().count() < 10,
        "config should stay sparse:\n{raw}"
    );
    let _ = fs::remove_file(path);
}

#[test]
fn verbose_input_round_trips_to_sparse_output() {
    let path = temp_path("verbose-roundtrip");
    let mut original = Settings::defaults();
    original.inner.general.approval = "danger".into();
    original.inner.ui.input.prefix = Some("λ ".into());
    original.inner.ui.status_show_timer = false;
    let verbose = serde_yaml::to_string(original.resolved()).unwrap();
    assert!(verbose.contains("show_reasoning: false"));
    assert!(verbose.contains("system_prompt: null"));
    assert!(verbose.contains("theme:"));
    fs::write(&path, verbose).unwrap();

    let loaded = Settings::load_path(&path).unwrap().unwrap();
    loaded.save_path(&path).unwrap();
    let reloaded = Settings::load_path(&path).unwrap().unwrap();
    assert_eq!(reloaded.inner.general.approval, "danger");
    assert_eq!(reloaded.inner.ui.input.prefix.as_deref(), Some("λ "));
    assert!(!reloaded.inner.ui.status_show_timer);
    let sparse = fs::read_to_string(&path).unwrap();
    assert!(!sparse.contains("show_reasoning"));
    assert!(!sparse.contains("theme:"));
    assert!(sparse.contains("approval: danger"));
    assert!(sparse.contains("status_show_timer: false"));

    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(lock_path_for(&path));
}

#[test]
fn field_updates_reload_latest_document_before_saving() {
    let path = temp_path("concurrent-updates");
    let initial = Settings::defaults();
    initial.save_path(&path).unwrap();
    let mut first = Settings::load_path(&path).unwrap().unwrap();
    let mut stale = Settings::load_path(&path).unwrap().unwrap();

    first
        .set_path_at("general.show_reasoning", true.into(), &path)
        .unwrap();
    stale
        .set_path_at("ui.status_show_timer", false.into(), &path)
        .unwrap();

    let loaded = Settings::load_path(&path).unwrap().unwrap();
    assert!(loaded.inner.general.show_reasoning);
    assert!(!loaded.inner.ui.status_show_timer);
    assert!(stale.inner.general.show_reasoning);
    let _ = fs::remove_file(path);
}

#[test]
fn rejects_invalid_files_without_overwriting_them() {
    let malformed = temp_path("malformed");
    fs::write(&malformed, "version: 1\ngeneral: [\n").unwrap();
    assert!(matches!(
        Settings::load_path(&malformed),
        Err(SettingsError::Parse(_))
    ));
    assert_eq!(
        fs::read_to_string(&malformed).unwrap(),
        "version: 1\ngeneral: [\n"
    );

    let bad_version = temp_path("bad-version");
    fs::write(&bad_version, "version: 3\n").unwrap();
    assert!(matches!(
        Settings::load_path(&bad_version),
        Err(SettingsError::BadVersion(3))
    ));

    let _ = fs::remove_file(malformed);
    let _ = fs::remove_file(bad_version);
}

// ── Cross-process lock tests ──────────────────────────────────────────

#[test]
fn lock_path_is_sibling_with_dot_lock_suffix() {
    let p = Path::new("/home/user/.bone-rust/config.yaml");
    let lock = lock_path_for(p);
    assert_eq!(
        lock,
        PathBuf::from("/home/user/.bone-rust/config.yaml.lock")
    );

    // Works with just a filename too.
    let bare = Path::new("config.yaml");
    assert_eq!(lock_path_for(bare), PathBuf::from("config.yaml.lock"));
}

#[test]
fn lock_errors_include_operation_path_and_os_error() {
    let path = PathBuf::from("exact/config.yaml.lock");
    for (operation, expected) in [
        (
            "create parent directory for",
            "cannot create parent directory for settings lock exact/config.yaml.lock: denied",
        ),
        (
            "open",
            "cannot open settings lock exact/config.yaml.lock: denied",
        ),
        (
            "acquire",
            "cannot acquire settings lock exact/config.yaml.lock: denied",
        ),
    ] {
        let error = SettingsError::Lock {
            operation,
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        };
        assert_eq!(error.to_string(), expected);
    }
    assert_eq!(
        SettingsError::Io(std::io::Error::other("plain")).to_string(),
        "settings I/O error: plain"
    );
}

#[test]
fn save_creates_lock_file() {
    let path = temp_path("lock-create");
    Settings::defaults().save_path(&path).unwrap();
    assert!(lock_path_for(&path).exists());
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(lock_path_for(&path));
}
