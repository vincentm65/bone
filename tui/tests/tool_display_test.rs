use bone::tools::types::{ToolCall, ToolDisplayConfig, ToolResult};
use bone::ui::tool_display::{
    build_tool_row, format_shell_call_label, format_shell_label, shell_row, tool_label,
};
use serde_json::{Value, json};

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        name: name.into(),
        arguments,
    }
}

fn result(name: &str, content: &str) -> ToolResult {
    ToolResult {
        call_id: "call-1".into(),
        name: name.into(),
        content: content.into(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Shell-label formatting
// ---------------------------------------------------------------------------

#[test]
fn shell_label_table_driven() {
    let cases: &[(&str, &str)] = &[
        ("cd repo && cargo test", "shell cd repo && cargo test"),
        (
            "printf \"a && b\" && echo done",
            "shell printf \"a && b\" && echo done",
        ),
        (
            "cat > /tmp/file << EOFfn main() {}EOF",
            "shell cat > /tmp/file << EOF\n  fn main()\n  {\n  }\n EOF",
        ),
        (
            "cat > /tmp/file << 'EOF'let x = 1;EOF",
            "shell cat > /tmp/file << 'EOF'\n  let x = 1;\n EOF",
        ),
        (
            "cat << 'EOF'let x = 1;EOFBONE_TEST_DIR=/tmp cargo test",
            "shell cat << 'EOF'\n  let x = 1;\n EOF\n BONE_TEST_DIR=/tmp cargo test",
        ),
        (
            "cat << EOF// hello fn main(){let x = 1;}EOF",
            "shell cat << EOF\n  // hello fn main()\n  {\n    let x = 1;\n  }\n EOF",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(format_shell_label(input), *expected);
    }
}

#[test]
fn shell_management_labels_include_action_and_process_id() {
    assert_eq!(
        format_shell_call_label(&json!({ "action": "status", "id": "process-20" })),
        "shell status process-20"
    );
    assert_eq!(
        format_shell_call_label(&json!({ "action": "kill", "id": "process-20" })),
        "shell kill process-20"
    );
    assert_eq!(
        format_shell_call_label(&json!({ "action": "list" })),
        "shell list"
    );
}

#[test]
fn shell_tool_rows_retain_content_and_flag_shell() {
    let call = call("shell", json!({ "command": "echo hi" }));
    let result = result("shell", "hi");

    let row = build_tool_row(&call, &result, None);
    let tool = row.tool.unwrap();
    assert_eq!(row.content, "hi");
    assert!(tool.is_shell);
    assert_eq!(tool.label, "shell echo hi");
}

#[test]
fn non_shell_tool_rows_still_hide_content_by_default() {
    let call = call("read_file", json!({ "path": "src/main.rs" }));
    let result = result("read_file", "contents");

    let row = build_tool_row(&call, &result, None);
    let tool = row.tool.unwrap();
    assert_eq!(row.content, "");
    assert!(!tool.is_shell);
}

#[test]
fn shell_row_uses_raw_output_and_shell_label() {
    let row = shell_row("printf hi && echo done", "hi\ndone".to_string(), true);
    let tool = row.tool.unwrap();

    assert_eq!(row.content, "hi\ndone");
    assert!(tool.is_error);
    assert!(tool.is_shell);
    assert_eq!(tool.label, "shell printf hi && echo done");
}

// ---------------------------------------------------------------------------
// Dynamic display config
// ---------------------------------------------------------------------------

#[test]
fn dynamic_display_args_render_in_tool_label() {
    let call = call("task_list", json!({ "action": "done", "index": 3 }));
    let result = result("task_list", "Marked task 3 as done");
    let display = ToolDisplayConfig {
        args: vec![
            "action".to_string(),
            "texts".to_string(),
            "index".to_string(),
            "indices".to_string(),
        ],
        ..Default::default()
    };

    assert_eq!(
        tool_label(&call, &result, Some(&display)),
        "task_list action=done index=3"
    );
}

#[test]
fn dynamic_display_template_renders_in_tool_label() {
    let call = call("web_search", json!({ "query": "rust async" }));
    let result = result("web_search", "");
    let display = ToolDisplayConfig {
        template: Some("search {query}".to_string()),
        ..Default::default()
    };

    assert_eq!(
        tool_label(&call, &result, Some(&display)),
        "web_search search \"rust async\""
    );
}

#[test]
fn dynamic_display_value_labels_render_in_tool_label() {
    let call = call("custom_tool", json!({ "action": "semantic_find" }));
    let result = result("custom_tool", "");
    let display = ToolDisplayConfig {
        template: Some("{action}".to_string()),
        value_labels: std::collections::HashMap::from([(
            "action".to_string(),
            std::collections::HashMap::from([(
                "semantic_find".to_string(),
                "finding accessible controls".to_string(),
            )]),
        )]),
        ..Default::default()
    };

    assert_eq!(
        tool_label(&call, &result, Some(&display)),
        "custom_tool finding accessible controls"
    );
}

#[test]
fn hidden_success_results_still_retain_errors() {
    let call = call("custom_tool", json!({}));
    let mut result = result("custom_tool", "machine JSON");
    let display = ToolDisplayConfig {
        show_result: Some(false),
        ..Default::default()
    };

    assert!(
        build_tool_row(&call, &result, Some(&display))
            .content
            .is_empty()
    );
    result.is_error = true;
    result.content = "visible failure".to_string();
    assert_eq!(
        build_tool_row(&call, &result, Some(&display)).content,
        "visible failure"
    );
}

// ---------------------------------------------------------------------------
// Subagent tool display
// ---------------------------------------------------------------------------

fn subagent_call(arguments: serde_json::Value) -> (ToolCall, ToolResult) {
    (
        call("subagent", arguments),
        result("subagent", "Dispatched 2, rejected 0"),
    )
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
        value_labels: Default::default(),
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

// ---------------------------------------------------------------------------
// read_file summary formatting
// ---------------------------------------------------------------------------

#[test]
fn read_file_summary_excludes_status_footer_lines() {
    // The read_file result appends "\n\n[...]" status footers; those lines
    // are not file content and must not inflate the read count.
    let call = call(
        "read_file",
        json!({ "path": "src/main.rs", "start_line": 501 }),
    );
    let result = result(
        "read_file",
        "line a\nline b\nline c\n\n[showing lines 501-503 of 503; end of file]",
    );

    let row = build_tool_row(&call, &result, None);
    let tool = row.tool.unwrap();
    assert!(
        tool.label.contains("(lines 501-503, 3 read)"),
        "label: {}",
        tool.label
    );
}

#[test]
fn read_file_summary_reports_zero_for_footer_only_result() {
    let call = call(
        "read_file",
        json!({ "path": "src/main.rs", "start_line": 999 }),
    );
    let result = result("read_file", "[no lines in range; file has 10 lines]");

    let row = build_tool_row(&call, &result, None);
    let tool = row.tool.unwrap();
    assert!(tool.label.contains("(0 lines)"), "label: {}", tool.label);
}

#[test]
fn read_file_summary_counts_only_new_numbered_rows() {
    let call = call(
        "read_file",
        json!({ "path": "src/main.rs", "start_line": 20 }),
    );
    let result = result(
        "read_file",
        "File: /repo/src/main.rs\nRange: lines 20-21 of 30.\n   20 | alpha\n   21 | beta",
    );

    let row = build_tool_row(&call, &result, None);
    let tool = row.tool.unwrap();
    assert!(
        tool.label.contains("(lines 20-21, 2 read)"),
        "label: {}",
        tool.label
    );
}
