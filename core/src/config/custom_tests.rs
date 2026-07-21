use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    ConfigField, ConfigFieldType, CustomConfigPage, CustomConfigs, config_dir, seed_builtin_pages,
};
use crate::config::{ProviderEntry, load_yaml};

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

#[cfg(unix)]
#[test]
fn provider_page_update_replaces_file_and_preserves_permissions() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    with_temp_config_home(|| {
        seed_builtin_pages(None, false);
        let path = config_dir().join("providers.yaml");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let original_inode = std::fs::metadata(&path).unwrap().ino();
        let mut configs = CustomConfigs::load();

        configs
            .upsert_provider_entry(
                "test-provider",
                &ProviderEntry {
                    label: "Test Provider".into(),
                    base_url: "http://localhost:1234".into(),
                    model: "test-model".into(),
                    api_key: Default::default(),
                    endpoint: "/chat/completions".into(),
                    handler: "openai".into(),
                    context_window_tokens: Some(4096),
                    reasoning_effort: String::new(),
                },
            )
            .unwrap();

        let metadata = std::fs::metadata(&path).unwrap();
        assert_ne!(metadata.ino(), original_inode);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
        let persisted = load_yaml::<CustomConfigPage>(&path).unwrap();
        assert!(persisted.fields.iter().any(|field| {
            field.key == "test-provider"
                && ProviderEntry::from_nested(field.value.as_ref().unwrap())
                    .is_some_and(|entry| entry.model == "test-model")
        }));
    });
}
