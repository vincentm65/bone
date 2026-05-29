use super::*;
use crate::config::{ProviderEntry, ProvidersConfig};
use serde_json::json;
use std::collections::HashMap;

#[test]
fn subagent_calls_use_immediate_live_path() {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "subagent".to_string(),
        arguments: json!({
            "approval": "read_only",
            "task": "review the architecture",
        }),
    };

    assert!(call_is_immediate(&call));
}

#[test]
fn non_subagent_calls_use_normal_tool_path() {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "Cargo.toml" }),
    };

    assert!(!call_is_immediate(&call));
}

#[test]
fn subagent_status_page_uses_compact_table_rows() {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "subagent".to_string(),
        arguments: json!({
            "approval": "read_only",
            "task": "Architecture/Design review for the workspace",
        }),
    };
    let page = subagent_status_page(&[ActiveSubagent {
        call,
        model: "current".to_string(),
        resolved_model: false,
        sent: 10_000,
        received: 1_300,
        status: SubagentStatus::Running,
    }]);

    assert_eq!(
        page.content[0].to_string(),
        "MODE MODEL        TOKENS TITLE"
    );
    let row = page.content[1].to_string();
    assert!(row.starts_with("ro   current      11.3k  Architecture/Design revie..."));
}

#[test]
fn subagent_configured_model_uses_requested_provider_model() {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "subagent".to_string(),
        arguments: json!({
            "approval": "read_only",
            "task": "review",
            "provider": "glm_plan",
        }),
    };
    let mut providers = HashMap::new();
    providers.insert(
        "glm_plan".to_string(),
        ProviderEntry {
            label: "GLM Plan".to_string(),
            base_url: String::new(),
            model: "GLM-5.1".to_string(),
            api_key: String::new(),
            endpoint: "/chat/completions".to_string(),
            handler: "openai".to_string(),
        },
    );
    let config = ProvidersConfig {
        last_provider: "local".to_string(),
        providers,
    };

    assert_eq!(
        subagent_configured_model(&call, &config, "current"),
        ("GLM-5.1".to_string(), true)
    );
}

#[test]
fn parse_subagent_event_parses_valid_json() {
    let event = parse_subagent_event(r#"{"type":"status","message":"thinking"}"#);
    assert!(event.is_some());
    assert_eq!(event.unwrap()["type"].as_str(), Some("status"));
}

#[test]
fn parse_subagent_event_returns_none_for_invalid_json() {
    assert!(parse_subagent_event("not json").is_none());
}

#[test]
fn token_usage_from_event_extracts_sent_received() {
    let event: serde_json::Value =
        serde_json::from_str(r#"{"type":"token_usage","sent":5000,"received":1200}"#).unwrap();
    assert_eq!(token_usage_from_event(&event), Some((5000, 1200)));
}

#[test]
fn token_usage_from_event_ignores_wrong_type() {
    let event: serde_json::Value =
        serde_json::from_str(r#"{"type":"status","message":"running"}"#).unwrap();
    assert_eq!(token_usage_from_event(&event), None);
}

#[test]
fn clip_text_short_value_unchanged() {
    assert_eq!(clip_text("hello", 10), "hello");
}

#[test]
fn clip_text_truncates_long_value() {
    assert_eq!(clip_text("abcdefghij", 7), "abcd...");
}

#[test]
fn clip_text_collapses_whitespace() {
    assert_eq!(clip_text("a  b  c", 10), "a b c");
}

#[test]
fn subagent_token_text_formats_thousands() {
    let call = ToolCall {
        id: "c1".to_string(),
        name: "subagent".to_string(),
        arguments: json!({}),
    };
    let agent = ActiveSubagent {
        call,
        model: "m".to_string(),
        resolved_model: false,
        sent: 5000,
        received: 1500,
        status: SubagentStatus::Running,
    };
    assert_eq!(subagent_token_text(&agent), "6.5k");
}

#[test]
fn subagent_token_text_formats_small_counts() {
    let call = ToolCall {
        id: "c1".to_string(),
        name: "subagent".to_string(),
        arguments: json!({}),
    };
    let agent = ActiveSubagent {
        call,
        model: "m".to_string(),
        resolved_model: false,
        sent: 50,
        received: 30,
        status: SubagentStatus::Running,
    };
    assert_eq!(subagent_token_text(&agent), "80");
}

#[test]
fn tool_error_creates_error_result() {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "shell".to_string(),
        arguments: json!({}),
    };
    let result = tool_error(&call, "something went wrong");
    assert_eq!(result.call_id, "call-1");
    assert_eq!(result.name, "shell");
    assert_eq!(result.content, "something went wrong");
    assert!(result.is_error);
    assert!(result.pane_page.is_none());
}

#[test]
fn pane_toggle_hint_hidden_when_no_pages() {
    assert_eq!(pane_toggle_hint(true, false), None);
    assert_eq!(pane_toggle_hint(false, false), None);
}

#[test]
fn pane_toggle_hint_shows_when_pages_exist() {
    assert_eq!(pane_toggle_hint(true, true), Some("Ctrl+T hide panel"));
    assert_eq!(pane_toggle_hint(false, true), Some("Ctrl+T show panel"));
}

#[test]
fn call_row_shown_during_prepare_only_for_edit_file() {
    let edit_call = ToolCall {
        id: "c1".to_string(),
        name: "edit_file".to_string(),
        arguments: json!({}),
    };
    let shell_call = ToolCall {
        id: "c2".to_string(),
        name: "shell".to_string(),
        arguments: json!({}),
    };
    assert!(call_row_shown_during_prepare(&edit_call));
    assert!(!call_row_shown_during_prepare(&shell_call));
}

#[test]
fn short_subagent_mode_maps_approval_modes() {
    let ro = ToolCall { id: "c".to_string(), name: "subagent".to_string(), arguments: json!({"approval":"read_only"}) };
    let edit = ToolCall { id: "c".to_string(), name: "subagent".to_string(), arguments: json!({"approval":"edit"}) };
    let danger = ToolCall { id: "c".to_string(), name: "subagent".to_string(), arguments: json!({"approval":"danger"}) };
    let unknown = ToolCall { id: "c".to_string(), name: "subagent".to_string(), arguments: json!({"approval":"custom_mode"}) };
    assert_eq!(short_subagent_mode(&ro), "ro");
    assert_eq!(short_subagent_mode(&edit), "edit");
    assert_eq!(short_subagent_mode(&danger), "danger");
    assert_eq!(short_subagent_mode(&unknown), "cus...");
}

#[test]
fn subagent_configured_model_falls_back_to_fallback() {
    let call = ToolCall {
        id: "c".to_string(),
        name: "subagent".to_string(),
        arguments: json!({"approval": "read_only", "task": "review"}),
    };
    let config = ProvidersConfig {
        last_provider: "local".to_string(),
        providers: HashMap::new(),
    };
    assert_eq!(
        subagent_configured_model(&call, &config, "fallback-model"),
        ("fallback-model".to_string(), false)
    );
}

#[test]
fn subagent_configured_model_uses_explicit_model_over_provider() {
    let call = ToolCall {
        id: "c".to_string(),
        name: "subagent".to_string(),
        arguments: json!({"approval": "read_only", "task": "review", "model": "explicit-model", "provider": "glm_plan"}),
    };
    let mut providers = HashMap::new();
    providers.insert("glm_plan".to_string(), ProviderEntry {
        label: "GLM".to_string(), base_url: String::new(), model: "GLM-5.1".to_string(),
        api_key: String::new(), endpoint: "/chat/completions".to_string(), handler: "openai".to_string(),
    });
    let config = ProvidersConfig { last_provider: "local".to_string(), providers };
    assert_eq!(
        subagent_configured_model(&call, &config, "current"),
        ("explicit-model".to_string(), true)
    );
}

#[test]
fn subagent_task_preview_clips_long_first_line() {
    let call = ToolCall {
        id: "c".to_string(),
        name: "subagent".to_string(),
        arguments: json!({"task": "This is a very long task description that should be clipped to fit within the width"}),
    };
    let preview = subagent_task_preview(&call, 28);
    assert!(preview.len() <= 31); // 28 chars + "..." padding
}

#[test]
fn assistant_message_with_tool_calls() {
    let call = ToolCall {
        id: "c1".to_string(),
        name: "shell".to_string(),
        arguments: json!({}),
    };
    let msg = assistant_message("I will run a command".to_string(), vec![call], "reasoning".to_string());
    assert_eq!(msg.role, ChatRole::Assistant);
    assert_eq!(msg.content, "I will run a command");
    assert_eq!(msg.reasoning_content, Some("reasoning".to_string()));
    assert_eq!(msg.tool_calls.len(), 1);
}

#[test]
fn assistant_message_without_tool_calls() {
    let msg = assistant_message("Hello".to_string(), vec![], String::new());
    assert_eq!(msg.role, ChatRole::Assistant);
    assert!(msg.tool_calls.is_empty());
    assert!(msg.reasoning_content.is_none());
}

#[test]
fn subagent_status_page_empty() {
    let page = subagent_status_page(&[]);
    assert_eq!(page.content.len(), 1); // header only
    assert_eq!(page.title, "subagents (0)");
}

#[test]
fn subagent_status_page_multiple_agents() {
    let call1 = ToolCall { id: "c1".to_string(), name: "subagent".to_string(), arguments: json!({"approval":"read_only","task":"task one"}) };
    let call2 = ToolCall { id: "c2".to_string(), name: "subagent".to_string(), arguments: json!({"approval":"danger","task":"task two"}) };
    let page = subagent_status_page(&[
        ActiveSubagent { call: call1, model: "model-a".to_string(), resolved_model: true, sent: 100, received: 200, status: SubagentStatus::Thinking },
        ActiveSubagent { call: call2, model: "model-b".to_string(), resolved_model: true, sent: 0, received: 0, status: SubagentStatus::Starting },
    ]);
    assert_eq!(page.content.len(), 3); // header + 2 rows
    assert!(page.content[1].to_string().starts_with("ro   "));
    assert!(page.content[2].to_string().starts_with("danger"));
}
