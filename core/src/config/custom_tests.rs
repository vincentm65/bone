use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    ConfigField, ConfigFieldType, CustomConfigPage, CustomConfigs, config_dir, seed_builtin_pages,
};
use crate::config::{bone_dir, load_yaml};

fn with_temp_config_home(test: impl FnOnce()) {
    let _guard = crate::util::test_env_lock();
    let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
    let old_bone = std::env::var_os("BONE_DIR");
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bone-config-migration-{suffix}"));
    std::fs::create_dir_all(&dir).unwrap();

    unsafe {
        std::env::remove_var("BONE_DIR");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
    }
    test();
    unsafe {
        match old_bone {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
        match old_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}

#[test]
fn old_values_file_general_status_toggles_migrate_to_status_page() {
    with_temp_config_home(|| {
        seed_builtin_pages(None, false);
        let values_path = bone_dir().join("config-values.yaml");
        std::fs::write(
            &values_path,
            "general:\n  status_show_timer: \"false\"\n  approval_mode: danger\n",
        )
        .unwrap();

        let configs = CustomConfigs::load();

        assert_eq!(configs.get_value("status", "status_show_timer"), "false");
        assert_eq!(configs.get_value("general", "approval_mode"), "danger");
        assert!(!values_path.exists());
    });
}

#[test]
fn backfill_adds_new_seed_field_to_existing_general_page() {
    with_temp_config_home(|| {
        seed_builtin_pages(None, false);
        // Simulate an older general.yaml predating newer built-in fields.
        let general_path = config_dir().join("general.yaml");
        let mut general = load_yaml::<CustomConfigPage>(&general_path).unwrap();
        general
            .fields
            .retain(|f| !matches!(f.key.as_str(), "show_thinking" | "input_preset"));
        // Keep a user-set value on a surviving field to prove it's preserved.
        if let Some(f) = general.fields.iter_mut().find(|f| f.key == "approval_mode") {
            f.value = Some(serde_yaml::Value::String("danger".to_string()));
        }
        std::fs::write(&general_path, serde_yaml::to_string(&general).unwrap()).unwrap();

        let mut configs = CustomConfigs::load();

        // New fields are now reachable, existing values remain untouched.
        assert_eq!(configs.get_value("general", "show_thinking"), "false");
        assert_eq!(configs.get_value("general", "input_preset"), "custom");
        assert_eq!(configs.get_value("general", "approval_mode"), "danger");

        configs.set_value("general", "input_preset", "box".to_string());
        assert_eq!(configs.get_value("general", "input_preset"), "box");
    });
}

#[test]
fn general_page_status_toggles_migrate_to_status_page() {
    with_temp_config_home(|| {
        seed_builtin_pages(None, false);
        let general_path = config_dir().join("general.yaml");
        let mut general = load_yaml::<CustomConfigPage>(&general_path).unwrap();
        general.fields.push(ConfigField {
            key: "status_show_spinner".to_string(),
            label: Some("Spinner".to_string()),
            field_type: ConfigFieldType::Bool,
            options: Vec::new(),
            default: Some(serde_yaml::Value::Bool(true)),
            value: Some(serde_yaml::Value::Bool(false)),
        });
        std::fs::write(&general_path, serde_yaml::to_string(&general).unwrap()).unwrap();

        let configs = CustomConfigs::load();

        assert_eq!(configs.get_value("status", "status_show_spinner"), "false");
    });
}

#[test]
fn compaction_migration_preserves_destinations_and_cleans_legacy_page() {
    with_temp_config_home(|| {
        seed_builtin_pages(None, false);
        let general_path = config_dir().join("general.yaml");
        let mut general = load_yaml::<CustomConfigPage>(&general_path).unwrap();
        general.fields.extend([
            ConfigField {
                key: "compact_trigger_mode".into(),
                label: None,
                field_type: ConfigFieldType::Enum,
                options: vec!["absolute".into(), "percentage".into()],
                default: Some(serde_yaml::Value::String("absolute".into())),
                value: Some(serde_yaml::Value::String("percentage".into())),
            },
            ConfigField {
                key: "compact_trigger_percentage".into(),
                label: None,
                field_type: ConfigFieldType::String,
                options: Vec::new(),
                default: Some(serde_yaml::Value::String("80".into())),
                value: Some(serde_yaml::Value::String("90".into())),
            },
            ConfigField {
                key: "compact_context_window_tokens".into(),
                label: None,
                field_type: ConfigFieldType::String,
                options: Vec::new(),
                default: Some(serde_yaml::Value::String("100000".into())),
                value: Some(serde_yaml::Value::String("131072".into())),
            },
        ]);
        std::fs::write(&general_path, serde_yaml::to_string(&general).unwrap()).unwrap();

        let mut settings = crate::config::settings::Settings::defaults();
        settings
            .inner
            .extensions
            .entry("compact".into())
            .or_default()
            .insert(
                "auto".into(),
                crate::config::settings::ExtensionValue::String("invalid".into()),
            );
        settings.save().unwrap();

        let configs = CustomConfigs::load();
        let settings = configs.settings.unwrap();
        assert_eq!(
            settings.extension_value("compact.auto"),
            Some(&crate::config::settings::ExtensionValue::String(
                "invalid".into()
            ))
        );
        assert_eq!(
            settings.extension_value("compact.trigger_percentage"),
            Some(&crate::config::settings::ExtensionValue::Number(90.0))
        );
        assert_eq!(
            settings.extension_value("compact.context_window_tokens"),
            Some(&crate::config::settings::ExtensionValue::Number(131_072.0))
        );
        let cleaned = load_yaml::<CustomConfigPage>(&general_path).unwrap();
        assert!(
            cleaned
                .fields
                .iter()
                .all(|field| !field.key.contains("compact"))
        );
    });
}

#[test]
fn compaction_migration_renames_fallback_extension_key() {
    with_temp_config_home(|| {
        seed_builtin_pages(None, false);
        let mut settings = crate::config::settings::Settings::defaults();
        settings
            .inner
            .extensions
            .entry("compact".into())
            .or_default()
            .insert(
                "fallback_context_window_tokens".into(),
                crate::config::settings::ExtensionValue::Number(100_000.0),
            );
        settings.save().unwrap();

        let configs = CustomConfigs::load();
        let settings = configs.settings.unwrap();
        assert_eq!(
            settings.extension_value("compact.context_window_tokens"),
            Some(&crate::config::settings::ExtensionValue::Number(100_000.0))
        );
        assert_eq!(
            settings.extension_value("compact.fallback_context_window_tokens"),
            None
        );
    });
}

#[test]
fn set_value_tests_use_an_isolated_config_home() {
    with_temp_config_home(|| {
        std::fs::create_dir_all(config_dir()).unwrap();
        let mut configs = CustomConfigs::default();
        configs.pages.push((
            "test".to_string(),
            CustomConfigPage {
                title: "Test".to_string(),
                fields: vec![
                    ConfigField {
                        key: "mode".to_string(),
                        label: None,
                        field_type: ConfigFieldType::Enum,
                        options: vec!["safe".into(), "edit".into(), "danger".into()],
                        default: Some(serde_yaml::Value::String("safe".into())),
                        value: None,
                    },
                    ConfigField {
                        key: "max".to_string(),
                        label: None,
                        field_type: ConfigFieldType::Number,
                        options: Vec::new(),
                        default: None,
                        value: None,
                    },
                ],
            },
        ));

        assert_eq!(configs.get_value("test", "mode"), "safe");
        configs.set_value("test", "mode", "danger".to_string());
        assert_eq!(configs.get_value("test", "mode"), "danger");

        configs.set_value("test", "max", "200".to_string());
        let field = configs.find_field("test", "max").unwrap();
        assert!(matches!(field.value, Some(serde_yaml::Value::Number(_))));
        assert_eq!(configs.get_value("test", "max"), "200");
        assert!(config_dir().join("test.yaml").exists());
    });
}
