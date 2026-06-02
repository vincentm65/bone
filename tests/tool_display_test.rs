use bone::tools::types::{ToolCall, ToolDisplayConfig, ToolResult};
use bone::ui::tool_display::{format_shell_label, tool_label};
use serde_json::json;

#[test]
fn shell_label_splits_top_level_shell_chains() {
    assert_eq!(
        format_shell_label("cd repo && cargo test"),
        "shell cd repo &&\n cargo test"
    );
}

#[test]
fn shell_label_keeps_quoted_operators_intact() {
    assert_eq!(
        format_shell_label("printf \"a && b\" && echo done"),
        "shell printf \"a && b\" &&\n echo done"
    );
}

#[test]
fn shell_label_expands_unquoted_heredoc_delimiter() {
    assert_eq!(
        format_shell_label("cat > /tmp/file << EOFfn main() {}EOF"),
        "shell cat > /tmp/file << EOF\n  fn main()\n  {\n  }\n EOF"
    );
}

#[test]
fn shell_label_expands_quoted_heredoc_delimiter() {
    assert_eq!(
        format_shell_label("cat > /tmp/file << 'EOF'let x = 1;EOF"),
        "shell cat > /tmp/file << 'EOF'\n  let x = 1;\n EOF"
    );
}

#[test]
fn shell_label_handles_collapsed_heredoc_followed_by_command() {
    assert_eq!(
        format_shell_label("cat << 'EOF'let x = 1;EOFBONE_TEST_DIR=/tmp cargo test"),
        "shell cat << 'EOF'\n  let x = 1;\n EOF\n BONE_TEST_DIR=/tmp cargo test"
    );
}

#[test]
fn shell_label_reflows_basic_code_payload() {
    assert_eq!(
        format_shell_label("cat << EOF// hello fn main(){let x = 1;}EOF"),
        "shell cat << EOF\n  // hello fn main()\n  {\n    let x = 1;\n  }\n EOF"
    );
}

#[test]
fn dynamic_display_args_render_in_tool_label() {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "task_list".to_string(),
        arguments: json!({
            "action": "done",
            "index": 3,
        }),
    };
    let result = ToolResult {
        call_id: "call-1".to_string(),
        name: "task_list".to_string(),
        content: "Marked task 3 as done".to_string(),
        is_error: false,
        pane_page: None,
        state: None,
    };
    let display = ToolDisplayConfig {
        args: vec![
            "action".to_string(),
            "texts".to_string(),
            "index".to_string(),
            "indices".to_string(),
        ],
        template: None,
        show: None,
        show_result: None,
    };

    assert_eq!(
        tool_label(&call, &result, Some(&display)),
        "task_list action=done index=3"
    );
}

#[test]
fn dynamic_display_template_renders_in_tool_label() {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "web_search".to_string(),
        arguments: json!({
            "query": "rust async",
        }),
    };
    let result = ToolResult {
        call_id: "call-1".to_string(),
        name: "web_search".to_string(),
        content: String::new(),
        is_error: false,
        pane_page: None,
        state: None,
    };
    let display = ToolDisplayConfig {
        args: Vec::new(),
        template: Some("search {query}".to_string()),
        show: None,
        show_result: None,
    };

    assert_eq!(
        tool_label(&call, &result, Some(&display)),
        "web_search search \"rust async\""
    );
}
