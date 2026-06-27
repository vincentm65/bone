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
        images: Vec::new(),
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
        eager: None,
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
        images: Vec::new(),
        is_error: false,
        pane_page: None,
        state: None,
    };
    let display = ToolDisplayConfig {
        args: Vec::new(),
        template: Some("search {query}".to_string()),
        show: None,
        show_result: None,
        eager: None,
    };

    assert_eq!(
        tool_label(&call, &result, Some(&display)),
        "web_search search \"rust async\""
    );
}

fn subagent_call(arguments: serde_json::Value) -> (ToolCall, ToolResult) {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "subagent".to_string(),
        arguments,
    };
    let result = ToolResult {
        call_id: "call-1".to_string(),
        name: "subagent".to_string(),
        content: "Dispatched 2, rejected 0".to_string(),
        images: Vec::new(),
        is_error: false,
        pane_page: None,
        state: None,
    };
    (call, result)
}

/// The display config the `subagent` tool declares (mirrors subagent.lua):
/// an array template for the dispatch label plus `args` for the fallback.
fn subagent_display() -> ToolDisplayConfig {
    ToolDisplayConfig {
        // Mirrors subagent.lua: the array template drives the dispatch label;
        // absent args are filtered out of the fallback, so a non-dispatch call
        // still renders as `action=status`.
        args: vec![
            "action".to_string(),
            "tasks".to_string(),
            "wait".to_string(),
            "ids".to_string(),
        ],
        template: Some("dispatch: {tasks[].title|task}".to_string()),
        show: Some(true),
        show_result: Some(false),
        eager: Some(true),
    }
}

#[test]
fn subagent_dispatch_label_uses_task_titles() {
    let (call, result) = subagent_call(json!({
        "action": "dispatch",
        "tasks": [
            { "agent": "reviewer", "title": "Review unstaged changes", "task": "Review unstaged changes in /home/foo for bugs..." },
            { "agent": "tester", "title": "Run the test suite", "task": "Run cargo test and report failures..." },
        ],
        "wait": false,
    }));

    assert_eq!(
        tool_label(&call, &result, Some(&subagent_display())),
        "subagent dispatch: \"Review unstaged changes\", \"Run the test suite\""
    );
}

#[test]
fn subagent_dispatch_label_falls_back_to_task_when_no_title() {
    let (call, result) = subagent_call(json!({
        "action": "dispatch",
        "tasks": [
            { "agent": "reviewer", "task": "Review the diff" },
        ],
    }));

    assert_eq!(
        tool_label(&call, &result, Some(&subagent_display())),
        "subagent dispatch: \"Review the diff\""
    );
}

#[test]
fn subagent_non_dispatch_action_uses_generic_display() {
    // No `tasks` → the array template resolves to nothing and the row falls
    // back to the `args` label.
    let (call, result) = subagent_call(json!({
        "action": "status",
    }));

    assert_eq!(
        tool_label(&call, &result, Some(&subagent_display())),
        "subagent action=status"
    );
}
