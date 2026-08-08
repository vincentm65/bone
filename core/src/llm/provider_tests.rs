use super::{new_cache_scope, parse_tool_arguments};
use crate::tools::TRUNCATED_ARGS_KEY;
use serde_json::json;

#[test]
fn tool_argument_contract() {
    assert_eq!(parse_tool_arguments(""), json!({}));
    assert_eq!(parse_tool_arguments(" \n\t"), json!({}));
    assert_eq!(
        parse_tool_arguments(r#"{"path":"x"}"#),
        json!({"path": "x"})
    );
    assert_eq!(
        parse_tool_arguments(r#"{"path":"x"#),
        json!({TRUNCATED_ARGS_KEY: r#"{"path":"x"#})
    );
}

#[test]
fn conversation_cache_scope_is_deterministic() {
    assert_eq!(new_cache_scope(Some(42)), "conversation-42");
    assert_eq!(new_cache_scope(Some(42)), "conversation-42");
}

#[test]
fn non_conversation_cache_scopes_are_distinct() {
    let first = new_cache_scope(None);
    let second = new_cache_scope(None);

    assert!(first.starts_with(&format!("run-{}-", std::process::id())));
    assert_ne!(first, second);
}
