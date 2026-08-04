use super::parse_tool_arguments;
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
