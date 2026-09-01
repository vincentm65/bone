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
fn legacy_flat_theme_colors_deserialize_into_canonical_fields() {
    let theme: ThemeSettings = serde_yaml::from_str(
        "shell_program: '#112233'\nsyntax_function: '#445566'\nsyntax_type: '#778899'\n",
    )
    .unwrap();

    assert_eq!(theme.shell.program.as_deref(), Some("#112233"));
    assert_eq!(theme.syntax.function_name.as_deref(), Some("#445566"));
    assert_eq!(theme.syntax.r#type.as_deref(), Some("#778899"));

    let serialized = serde_yaml::to_string(&theme).unwrap();
    assert!(serialized.contains("shell:\n  program: '#112233'"));
    assert!(serialized.contains("function_name: '#445566'"));
    assert!(!serialized.contains("shell_program:"));
    assert!(!serialized.contains("syntax_function:"));
    assert!(!serialized.contains("syntax_type:"));
}

#[test]
fn legacy_flat_theme_colors_override_nested_values() {
    let theme: ThemeSettings = serde_yaml::from_str(
        "shell:\n  program: nested\nsyntax:\n  comment: nested\nshell_program: legacy\nsyntax_comment: legacy\n",
    )
    .unwrap();

    assert_eq!(theme.shell.program.as_deref(), Some("legacy"));
    assert_eq!(theme.syntax.comment.as_deref(), Some("legacy"));
}

#[test]
fn theme_compatibility_deserializer_still_rejects_unknown_fields() {
    assert!(serde_yaml::from_str::<ThemeSettings>("shell_program_typo: red\n").is_err());
    assert!(serde_yaml::from_str::<ThemeSettings>("shell:\n  program_typo: red\n").is_err());
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
    assert!(raw.contains("system_prompt:"));
    assert!(raw.contains(shipped_system_prompt().lines().next().unwrap()));
    assert!(!raw.contains("subagents:"));
    assert!(!raw.contains("label:"));
    assert!(!raw.contains("null"));
    assert!(!raw.contains("show_reasoning"));
    assert!(!raw.contains("theme:"));
    let _ = fs::remove_file(path);
}

#[test]
fn unchanged_load_does_not_create_write_lock() {
    let path = temp_path("lock-free-load");
    let yaml = serde_yaml::to_string(Settings::defaults().resolved()).unwrap();
    fs::write(&path, yaml).unwrap();
    let lock_path = lock_path_for(&path);

    let loaded = Settings::load_path(&path).unwrap().unwrap();

    assert_eq!(
        loaded.inner.general.system_prompt.as_deref(),
        Some(shipped_system_prompt())
    );
    assert!(!lock_path.exists());
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
    assert!(verbose.contains("system_prompt:"));
    assert!(verbose.contains(shipped_system_prompt().lines().next().unwrap()));
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
fn missing_and_null_prompts_hydrate_and_persist_while_empty_is_preserved() {
    for (name, yaml) in [
        ("missing-prompt", "version: 2\n"),
        (
            "null-prompt",
            "version: 2\ngeneral:\n  system_prompt: null\n",
        ),
    ] {
        let path = temp_path(name);
        fs::write(&path, yaml).unwrap();

        let loaded = Settings::load_path(&path).unwrap().unwrap();
        assert_eq!(
            loaded.inner.general.system_prompt.as_deref(),
            Some(shipped_system_prompt())
        );
        let persisted = fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("system_prompt:"));
        assert!(persisted.contains(shipped_system_prompt().lines().next().unwrap()));

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path_for(&path));
    }

    let path = temp_path("empty-prompt");
    fs::write(&path, "version: 2\ngeneral:\n  system_prompt: ''\n").unwrap();
    let loaded = Settings::load_path(&path).unwrap().unwrap();
    assert_eq!(loaded.inner.general.system_prompt.as_deref(), Some(""));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "version: 2\ngeneral:\n  system_prompt: ''\n"
    );
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
