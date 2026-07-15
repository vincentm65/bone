use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_temp_config_home(test: impl FnOnce()) {
    let _guard = ENV_LOCK.lock().unwrap();
    let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bone-config-migration-{suffix}"));
    std::fs::create_dir_all(&dir).unwrap();

    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &dir);
    }
    test();
    unsafe {
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
        assert_eq!(UserConfig::from_custom_configs(&configs).input_preset, None);

        configs.set_value("general", "input_preset", "box".to_string());
        assert_eq!(
            UserConfig::from_custom_configs(&configs)
                .input_preset
                .as_deref(),
            Some("box")
        );
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
