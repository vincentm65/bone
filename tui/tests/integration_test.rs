use bone::chat::Message;
use bone::llm::ChatRole;
use bone::ui::commands::is_protected_builtin;

// ── Message Construction ─────────────────────────────────────────────────────

#[test]
fn user_message_has_correct_role() {
    let msg = Message::user("hello");
    assert_eq!(msg.role, ChatRole::User);
    assert_eq!(msg.content, "hello");
    assert!(msg.tool.is_none());
}

#[test]
fn assistant_message_has_correct_role() {
    let msg = Message::assistant("response");
    assert_eq!(msg.role, ChatRole::Assistant);
    assert_eq!(msg.content, "response");
    assert!(msg.tool.is_none());
}

#[test]
fn system_message_has_correct_role() {
    let msg = Message::system("you are helpful");
    assert_eq!(msg.role, ChatRole::System);
    assert_eq!(msg.content, "you are helpful");
    assert!(msg.tool.is_none());
}

#[test]
fn tool_message_is_marked_as_tool_role() {
    let msg = Message::tool_row("shell: ls -la".to_string(), false);
    assert_eq!(msg.role, ChatRole::Tool);
    assert!(msg.content.is_empty());
    assert!(msg.tool.is_some());
}

#[test]
fn tool_message_error_flag_is_preserved() {
    let msg = Message::tool_row("shell: failed command".to_string(), true);
    assert!(msg.tool.unwrap().is_error);

    let ok_msg = Message::tool_row("shell: ok command".to_string(), false);
    assert!(!ok_msg.tool.unwrap().is_error);
}

#[test]
fn documented_builtins_are_protected_from_lua_overrides() {
    for cmd in [
        "catalog", "clear", "config", "edit", "e", "exit", "help", "model", "new", "provider",
        "quit", "setup", "stats", "tools",
    ] {
        assert!(is_protected_builtin(cmd), "/{cmd} should be protected");
    }

    // Removed builtins and Lua-provided commands are overridable.
    for cmd in ["compact", "context", "usage", "memory", "review"] {
        assert!(!is_protected_builtin(cmd), "/{cmd} should not be protected");
    }
}

// ── Tool Handler (concurrency ordering) ─────────────────────────────────────

#[tokio::test]
async fn execute_all_returns_results_in_request_order_after_concurrent_execution() {
    use async_trait::async_trait;
    use bone::tools::registry::{ToolHandler, ToolRegistry};
    use bone::tools::types::{Tool, ToolCall, ToolDefinition};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use tokio::time::{Duration, sleep};

    struct RecordingTool {
        order: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Tool for RecordingTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "record".to_string(),
                description: "record execution order".to_string(),
                input_schema: json!({ "type": "object" }),
            }
        }

        async fn execute(&self, arguments: Value) -> Result<String, String> {
            let value = arguments["value"].as_str().unwrap().to_string();
            let delay_ms = arguments["delay_ms"].as_u64().unwrap_or(0);
            sleep(Duration::from_millis(delay_ms)).await;
            self.order.lock().unwrap().push(value.clone());
            Ok(value)
        }
    }

    let order = Arc::new(Mutex::new(Vec::new()));
    let handler = ToolHandler::new(ToolRegistry::new().register(RecordingTool {
        order: order.clone(),
    }));

    let results = handler
        .execute_all(
            vec![
                ToolCall {
                    id: "slow".into(),
                    name: "record".into(),
                    arguments: json!({ "value": "first", "delay_ms": 20 }),
                },
                ToolCall {
                    id: "fast".into(),
                    name: "record".into(),
                    arguments: json!({ "value": "second", "delay_ms": 0 }),
                },
            ],
            0,
        )
        .await;

    assert_eq!(*order.lock().unwrap(), vec!["second", "first"]);
    assert_eq!(results[0].call_id, "slow");
    assert_eq!(results[1].call_id, "fast");
}

#[tokio::test]
async fn disabled_tools_are_not_advertised_or_executed() {
    use bone::tools::builtin_tools;
    use bone::tools::registry::ToolHandler;
    use bone::tools::types::ToolCall;
    use serde_json::json;
    use std::collections::HashMap;

    let enabled = vec!["read_file".to_string()];
    let handler = ToolHandler::with_enabled_safety_and_display(
        builtin_tools(),
        &enabled,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );

    let definitions = handler.definitions();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].name, "read_file");

    let results = handler
        .execute_all(
            vec![ToolCall {
                id: "disabled".into(),
                name: "shell".into(),
                arguments: json!({ "command": "pwd" }),
            }],
            0,
        )
        .await;
    assert!(results[0].is_error);
    assert!(results[0].content.contains("disabled"));
}
// ── Custom Config ────────────────────────────────────────────────────────────

#[test]
fn custom_configs_get_value_returns_default_when_no_value_set() {
    use bone::config::custom::{ConfigField, ConfigFieldType, CustomConfigPage, CustomConfigs};

    let mut configs = CustomConfigs::default();
    configs.pages.push((
        "test".to_string(),
        CustomConfigPage {
            title: "Test".to_string(),
            fields: vec![ConfigField {
                key: "port".to_string(),
                label: None,
                field_type: ConfigFieldType::Number,
                options: Vec::new(),
                default: Some(serde_yaml::Value::Number(8080.into())),
                value: None,
            }],
        },
    ));

    assert_eq!(configs.get_value("test", "port"), "8080");
}

#[test]
fn custom_configs_set_value_overrides_default() {
    use bone::config::custom::{ConfigField, ConfigFieldType, CustomConfigPage, CustomConfigs};

    let mut configs = CustomConfigs::default();
    configs.pages.push((
        "test".to_string(),
        CustomConfigPage {
            title: "Test".to_string(),
            fields: vec![ConfigField {
                key: "mode".to_string(),
                label: None,
                field_type: ConfigFieldType::Enum,
                options: vec!["safe".into(), "edit".into(), "danger".into()],
                default: Some(serde_yaml::Value::String("safe".into())),
                value: None,
            }],
        },
    ));

    assert_eq!(configs.get_value("test", "mode"), "safe");
    configs.set_value("test", "mode", "danger".to_string());
    assert_eq!(configs.get_value("test", "mode"), "danger");
}

#[test]
fn custom_configs_number_field_stores_yaml_number() {
    use bone::config::custom::{ConfigField, ConfigFieldType, CustomConfigPage, CustomConfigs};

    let mut configs = CustomConfigs::default();
    configs.pages.push((
        "test".to_string(),
        CustomConfigPage {
            title: "Test".to_string(),
            fields: vec![ConfigField {
                key: "max".to_string(),
                label: None,
                field_type: ConfigFieldType::Number,
                options: Vec::new(),
                default: None,
                value: None,
            }],
        },
    ));

    configs.set_value("test", "max", "200".to_string());
    let field = configs.find_field("test", "max").unwrap();
    // Should be stored as a YAML number, not a string
    assert!(matches!(field.value, Some(serde_yaml::Value::Number(_))));
    assert_eq!(configs.get_value("test", "max"), "200");
}

#[test]
fn user_config_from_custom_configs_applies_general_settings() {
    use bone::config::UserConfig;
    use bone::config::custom::{ConfigField, ConfigFieldType, CustomConfigPage, CustomConfigs};
    use bone::tools::ApprovalMode;

    let mut configs = CustomConfigs::default();
    configs.pages.push((
        "general".to_string(),
        CustomConfigPage {
            title: "General".to_string(),
            fields: vec![ConfigField {
                key: "approval_mode".to_string(),
                label: None,
                field_type: ConfigFieldType::Enum,
                options: vec!["safe".into(), "edit".into(), "danger".into()],
                default: Some(serde_yaml::Value::String("safe".into())),
                value: Some(serde_yaml::Value::String("danger".into())),
            }],
        },
    ));

    let cfg = UserConfig::from_custom_configs(&configs);
    assert_eq!(cfg.approval_mode, ApprovalMode::Danger);
}

#[test]
fn enabled_tool_names_only_includes_true_and_unset() {
    use bone::config::custom::{ConfigField, ConfigFieldType, CustomConfigPage, CustomConfigs};

    let mut configs = CustomConfigs::default();
    configs.pages.push((
        "tools".to_string(),
        CustomConfigPage {
            title: "Tools".to_string(),
            fields: vec![
                ConfigField {
                    key: "read_file".to_string(),
                    label: None,
                    field_type: ConfigFieldType::Bool,
                    options: Vec::new(),
                    default: Some(serde_yaml::Value::Bool(true)),
                    value: None, // default = true → enabled
                },
                ConfigField {
                    key: "shell".to_string(),
                    label: None,
                    field_type: ConfigFieldType::Bool,
                    options: Vec::new(),
                    default: Some(serde_yaml::Value::Bool(true)),
                    value: Some(serde_yaml::Value::Bool(false)), // explicitly disabled
                },
                ConfigField {
                    key: "edit_file".to_string(),
                    label: None,
                    field_type: ConfigFieldType::Bool,
                    options: Vec::new(),
                    default: Some(serde_yaml::Value::Bool(true)),
                    value: Some(serde_yaml::Value::Bool(true)), // explicitly enabled
                },
            ],
        },
    ));

    let enabled = configs.enabled_tool_names();
    assert!(enabled.contains(&"read_file".to_string()));
    assert!(!enabled.contains(&"shell".to_string()));
    assert!(enabled.contains(&"edit_file".to_string()));
}
