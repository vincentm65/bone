use bone::chat::message::Message;
use bone::config::UserConfig;
use bone::llm::ChatRole;
use bone::ui::render::wrap::{visual_line_count, wrap_text, wrap_text_with_prefix};

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
    let msg = Message::tool_row("bash: ls -la".to_string(), false);
    assert_eq!(msg.role, ChatRole::Tool);
    assert!(msg.content.is_empty());
    assert!(msg.tool.is_some());
}

#[test]
fn tool_message_error_flag_is_preserved() {
    let msg = Message::tool_row("bash: failed command".to_string(), true);
    assert!(msg.tool.unwrap().is_error);

    let ok_msg = Message::tool_row("bash: ok command".to_string(), false);
    assert!(!ok_msg.tool.unwrap().is_error);
}

// ── Text Wrapping ────────────────────────────────────────────────────────────

#[test]
fn wrap_plain_line_splits_at_spaces() {
    // Current algorithm prefers the last whitespace within width, so " foo bar"
    // (8 chars) fits together at width 10 after " world" breaks off.
    assert_eq!(
        wrap_text("hello world foo bar", 10),
        vec!["hello", " world", " foo bar"]
    );
}

#[test]
fn wrap_plain_line_keeps_leading_indent_on_continuations() {
    assert_eq!(
        wrap_text("  hello world foo bar", 10),
        vec!["  hello", "  world", "  foo bar"]
    );
}

#[test]
fn wrap_plain_line_hard_breaks_long_words() {
    assert_eq!(wrap_text("abcdefghij", 5), vec!["abcde", "fghij"]);
}

#[test]
fn wrap_plain_line_empty_string_returns_single_empty_line() {
    assert_eq!(wrap_text("", 10), vec![""]);
}

#[test]
fn wrap_with_prefix_applies_prefixes_correctly() {
    // With first_prefix "> " (width 2) and rest_prefix "  " (width 2), width 10
    // gives first_width=8, rest_width=8. "one two three" at width 8 breaks at
    // the last space: "one two" then "three".
    assert_eq!(
        wrap_text_with_prefix("one two three", "> ", "  ", 10),
        vec!["> one two", "  three"]
    );
}

#[test]
fn wrap_preserves_unicode_display_width() {
    // Each CJK character is width 2
    assert_eq!(wrap_text("你好世界", 4), vec!["你好", "世界"]);
}

#[test]
fn wrap_respects_minimum_width_one() {
    assert_eq!(wrap_text("abc", 1), vec!["a", "b", "c"]);
}

// ── Visual Line Counting ─────────────────────────────────────────────────────

#[test]
fn empty_text_counts_as_one_line() {
    assert_eq!(visual_line_count("", 80), 1);
}

#[test]
fn short_text_fits_in_one_line() {
    assert_eq!(visual_line_count("hello", 80), 1);
}

#[test]
fn text_exceeding_width_wraps_multiple_lines() {
    assert_eq!(visual_line_count("hello world foo bar", 10), 2);
}

#[test]
fn hard_newlines_add_line_breaks() {
    assert_eq!(visual_line_count("line1\nline2", 80), 2);
}

#[test]
fn wide_characters_count_correctly() {
    // "你好世界" = 4 chars × 2 width = 8 total
    // At width 4 => 2 visual lines
    assert_eq!(visual_line_count("你好世界", 4), 2);
}

#[test]
fn mixed_wide_and_narrow_characters() {
    // "a你好b世界" = 1+2+2+1+2+2 = 10 display width at width 4 => ceil(10/4)=3 lines
    assert_eq!(visual_line_count("a你好b世界", 4), 3);
}

// ── UserConfig ───────────────────────────────────────────────────────────────

#[test]
fn default_provider_is_local() {
    let cfg = UserConfig::default();
    assert_eq!(cfg.provider, "local");
}

#[test]
fn user_config_serializes_and_deserializes() {
    let cfg = UserConfig {
        provider: "openai".into(),
    };
    let yaml = serde_yaml::to_string(&cfg).unwrap();
    let deserialized: UserConfig = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(deserialized.provider, "openai");
}

#[test]
fn config_missing_provider_field_defaults_to_local() {
    let yaml = "";
    let cfg: UserConfig = serde_yaml::from_str(yaml).unwrap_or_default();
    assert_eq!(cfg.provider, "local");
}
// ── Tool Handler (concurrency ordering) ─────────────────────────────────────

#[tokio::test]
async fn execute_all_returns_results_in_request_order_after_concurrent_execution() {
    use async_trait::async_trait;
    use bone::tools::registry::ToolRegistry;
    use bone::tools::types::Tool;
    use bone::tools::{ToolCall, ToolDefinition, ToolHandler};
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
                name: "record",
                description: "record execution order",
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
        .execute_all(vec![
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
        ])
        .await;

    assert_eq!(*order.lock().unwrap(), vec!["second", "first"]);
    assert_eq!(results[0].call_id, "slow");
    assert_eq!(results[1].call_id, "fast");
}
