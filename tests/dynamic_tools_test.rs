use bone::tools::command_policy::CommandSafety;
use bone::tools::dynamic::{DynamicTool, InteractionType};
use bone::tools::registry::ToolRegistry;
use bone::tools::{ApprovalMode, ToolCall, ToolHandler};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("bone-tools-{label}-{suffix}"));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn default_ask_user_parses_select_interaction() {
    let yaml = include_str!("../defaults/tools/ask_user.yaml");
    let tool: DynamicTool = serde_yaml::from_str(yaml).unwrap();

    assert_eq!(tool.name, "ask_user");
    assert!(matches!(tool.interaction, Some(InteractionType::Select)));
    assert!(
        tool.args.iter().any(|arg| {
            arg.name == "allow_custom" && arg.arg_type == "boolean" && !arg.required
        })
    );
}

#[test]
fn script_tool_can_omit_args() {
    let tool: DynamicTool = serde_yaml::from_str(
        "name: status\ndescription: show status\nscript: |\n  git status --short\n",
    )
    .unwrap();

    assert!(tool.args.is_empty());
}

#[test]
fn dynamic_tool_declared_safe_safety_is_allowed_in_safe_mode() {
    let call = ToolCall {
        id: "call-1".into(),
        name: "safe_tool".into(),
        arguments: json!({ "action": "list" }),
    };
    let safety = [("safe_tool".to_string(), CommandSafety::ReadOnly)]
        .into_iter()
        .collect();
    let handler = ToolHandler::with_enabled_and_safety(ToolRegistry::new(), &[], safety);

    assert_eq!(handler.safety_for_call(&call), CommandSafety::ReadOnly);
    assert!(handler.allows_call(ApprovalMode::Safe, &call));
    assert!(handler.allows_call(ApprovalMode::Edits, &call));
}

#[test]
fn task_list_without_declared_safety_defaults_to_danger() {
    let dir = temp_dir("tool-safety");
    fs::write(
        dir.join("my_tool.yaml"),
        "name: my_tool\ndescription: my tool\nscript: echo ok\n",
    )
    .unwrap();
    let mut registry = bone::tools::builtin_tools();
    let mut dynamic_safety = std::collections::HashMap::new();
    for tool in bone::tools::dynamic::load_from_dir(&dir) {
        dynamic_safety.insert(
            tool.name.clone(),
            tool.safety.unwrap_or(CommandSafety::Danger),
        );
        registry = registry.register(tool);
    }
    let handler =
        ToolHandler::with_enabled_and_safety(registry, &["my_tool".into()], dynamic_safety);

    assert_eq!(
        handler.safety_for_call(&ToolCall {
            id: "call-1".into(),
            name: "my_tool".into(),
            arguments: json!({ "action": "list" }),
        }),
        CommandSafety::Danger
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn interaction_tool_with_script_is_rejected_during_load() {
    let dir = temp_dir("hybrid");
    fs::write(
        dir.join("select_branch.yaml"),
        "name: select_branch\ndescription: pick branch\ninteraction: select\nscript: echo main\n",
    )
    .unwrap();

    assert!(bone::tools::dynamic::load_from_dir(&dir).is_empty());
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn json_envelope_output_sets_tool_content_and_pane_page() {
    let tool: DynamicTool = serde_yaml::from_str(
        r#"
name: pane_writer
description: write pane content
output:
  kind: json_envelope
script: |
  printf '%s\n' '{"content":"Added task: fix auth","pane":{"source":"task_list","title":"tasks (0/1)","lines":["○ fix auth"]}}'
"#,
    )
    .unwrap();
    let handler = ToolHandler::new(ToolRegistry::new().register(tool));

    let results = handler
        .execute_all(vec![ToolCall {
            id: "call-1".into(),
            name: "pane_writer".into(),
            arguments: json!({}),
        }])
        .await;

    assert!(!results[0].is_error);
    assert_eq!(results[0].content, "Added task: fix auth");
    let page = results[0].pane_page.as_ref().unwrap();
    assert_eq!(page.source, "task_list");
    assert_eq!(page.title, "tasks (0/1)");
    assert_eq!(page.content[0].to_string(), "○ fix auth");
}

#[tokio::test]
async fn json_envelope_pane_scroll_is_preserved() {
    let tool: DynamicTool = serde_yaml::from_str(
        r#"
name: pane_scroller
description: write pane content with scroll
output:
  kind: json_envelope
script: |
  printf '%s\n' '{"content":"Added tasks","pane":{"source":"task_list","title":"tasks (0/10)","lines":["one","two","three"],"scroll":2}}'
"#,
    )
    .unwrap();
    let handler = ToolHandler::new(ToolRegistry::new().register(tool));

    let results = handler
        .execute_all(vec![ToolCall {
            id: "call-1".into(),
            name: "pane_scroller".into(),
            arguments: json!({}),
        }])
        .await;

    assert!(!results[0].is_error);
    assert_eq!(results[0].pane_page.as_ref().unwrap().scroll, 2);
}

#[tokio::test]
async fn default_task_list_kill_returns_empty_pane_for_removal() {
    let dir = temp_dir("task-list-kill");
    let mut tool: DynamicTool =
        serde_yaml::from_str(include_str!("../defaults/tools/task_list.yaml")).unwrap();
    let script = tool.script.take().unwrap();
    tool.script = Some(format!(
        "export XDG_CONFIG_HOME='{}'\n{}",
        dir.display(),
        script
    ));
    let handler = ToolHandler::new(ToolRegistry::new().register(tool));

    let results = handler
        .execute_all(vec![ToolCall {
            id: "call-1".into(),
            name: "task_list".into(),
            arguments: json!({ "action": "kill" }),
        }])
        .await;

    assert!(!results[0].is_error, "{}", results[0].content);
    assert_eq!(results[0].content, "Task list killed.");
    let page = results[0].pane_page.as_ref().unwrap();
    assert_eq!(page.source, "task_list");
    assert!(page.content.is_empty());

    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn repeated_task_list_calls_execute_in_order() {
    let dir = temp_dir("task-list-order");
    let counter = dir.join("counter");
    let tool: DynamicTool = serde_yaml::from_str(&format!(
        r#"
name: task_list
description: test ordering
script: |
  n=0
  if [ -f {counter} ]; then n=$(cat {counter}); fi
  sleep 0.1
  n=$((n + 1))
  printf '%s' "$n" > {counter}
  printf '%s' "$n"
"#,
        counter = counter.display()
    ))
    .unwrap();
    let handler = ToolHandler::new(ToolRegistry::new().register(tool));

    let results = handler
        .execute_all(vec![
            ToolCall {
                id: "call-1".into(),
                name: "task_list".into(),
                arguments: json!({}),
            },
            ToolCall {
                id: "call-2".into(),
                name: "task_list".into(),
                arguments: json!({}),
            },
        ])
        .await;

    assert_eq!(results[0].content, "1");
    assert_eq!(results[1].content, "2");
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn json_envelope_invalid_json_returns_tool_error() {
    let tool: DynamicTool = serde_yaml::from_str(
        r#"
name: broken_pane_writer
description: write bad pane content
output:
  kind: json_envelope
script: |
  printf '%s\n' 'not json'
"#,
    )
    .unwrap();
    let handler = ToolHandler::new(ToolRegistry::new().register(tool));

    let results = handler
        .execute_all(vec![ToolCall {
            id: "call-1".into(),
            name: "broken_pane_writer".into(),
            arguments: json!({}),
        }])
        .await;

    assert!(results[0].is_error);
    assert!(results[0].content.contains("invalid json_envelope output"));
    assert!(results[0].pane_page.is_none());
}

#[tokio::test]
async fn json_envelope_empty_pane_lines_returns_empty_page_for_removal() {
    let tool: DynamicTool = serde_yaml::from_str(
        r#"
name: pane_remover
description: remove pane content
output:
  kind: json_envelope
script: |
  printf '%s\n' '{"content":"All tasks cleared.","pane":{"source":"task_list","title":"tasks (0)","lines":[]}}'
"#,
    )
    .unwrap();
    let handler = ToolHandler::new(ToolRegistry::new().register(tool));

    let results = handler
        .execute_all(vec![ToolCall {
            id: "call-1".into(),
            name: "pane_remover".into(),
            arguments: json!({}),
        }])
        .await;

    assert!(!results[0].is_error);
    let page = results[0].pane_page.as_ref().unwrap();
    assert_eq!(page.source, "task_list");
    assert!(page.content.is_empty());
}
