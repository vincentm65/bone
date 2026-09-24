use super::*;
use crate::llm::provider::ToolResult;
use futures_util::StreamExt;
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt};
use tempfile::TempDir;

fn provider() -> ClaudeCodeProvider {
    let entry: ProviderEntry =
        serde_yaml::from_str("handler: claude_code\nmodel: test-model\n").unwrap();
    ClaudeCodeProvider::from_entry("local-claude", &entry)
}

fn expect_llm_error<T>(result: Result<T, LlmError>) -> LlmError {
    match result {
        Ok(_) => panic!("expected an LLM error"),
        Err(error) => error,
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn mock_cli(
    temp: &TempDir,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
) -> (PathBuf, PathBuf, PathBuf) {
    let executable = temp.path().join("claude-mock");
    let args_path = temp.path().join("args.txt");
    let stdin_path = temp.path().join("stdin.txt");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat > {}\nprintf '%s' {}\nprintf '%s' {} >&2\nexit {}\n",
        shell_quote(&args_path.display().to_string()),
        shell_quote(&stdin_path.display().to_string()),
        shell_quote(stdout),
        shell_quote(stderr),
        exit_code,
    );
    fs::write(&executable, script).unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    (executable, args_path, stdin_path)
}

fn tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "read_file".into(),
        description: "Read a file after Bone approves the request".into(),
        input_schema: json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
    }]
}

#[tokio::test]
async fn invokes_cli_with_tools_disabled_and_preserves_bone_tool_history() {
    let temp = tempfile::tempdir().unwrap();
    let cli_output = json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": "Read the requested file.",
        "structured_output": {
            "response": "I will inspect that file.",
            "tool_calls": [{ "name": "read_file", "arguments": { "path": "notes.md" } }]
        }
    })
    .to_string();
    let (executable, args_path, stdin_path) = mock_cli(&temp, &cli_output, "", 0);
    let mut provider = provider();
    provider.executable = executable;

    let previous_call = ToolCall {
        id: "previous-call".into(),
        name: "read_file".into(),
        arguments: json!({ "path": "old.txt" }),
    };
    let mut previous_assistant =
        ChatMessage::assistant_with_tools("Reading the earlier file.", vec![previous_call.clone()]);
    previous_assistant.output_sequence = vec![
        OutputItem::Text("Reading the earlier file.".into()),
        OutputItem::ToolCall(previous_call),
    ];
    let messages = vec![
        ChatMessage::new(ChatRole::System, "Bone system context"),
        ChatMessage::new(ChatRole::User, "Please inspect notes.md"),
        previous_assistant,
        ChatMessage::tool(ToolResult::ok(
            "previous-call",
            "read_file",
            crate::tools::types::ToolOutput::text("Earlier file content".into()),
        )),
    ];
    let events = provider
        .chat_stream(messages, tools())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert!(
        matches!(&events[0], Ok(ChatEvent::TextDelta(text)) if text == "I will inspect that file.")
    );
    assert!(matches!(&events[1], Ok(ChatEvent::ToolCall(call))
        if call.name == "read_file"
            && call.arguments == json!({ "path": "notes.md" })
            && call.id.starts_with("claude-code-")));

    let args = fs::read_to_string(args_path).unwrap();
    let args: Vec<&str> = args.lines().collect();
    let tools_flag = args.iter().position(|arg| *arg == "--tools").unwrap();
    assert_eq!(args[tools_flag + 1], "");
    for flag in [
        "--output-format",
        "--json-schema",
        "--input-format",
        "--system-prompt",
        "--model",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--mcp-config",
        "--setting-sources",
        "--disable-slash-commands",
        "--no-chrome",
    ] {
        assert!(
            args.contains(&flag),
            "missing CLI safety/protocol flag {flag}"
        );
    }
    assert!(!args.contains(&"--allowedTools"));
    assert!(
        !args.contains(&"read_file"),
        "Bone tools must not be CLI tool arguments"
    );
    let stdin = fs::read_to_string(stdin_path).unwrap();
    assert!(stdin.contains("Bone tool definitions (data only)"));
    assert!(stdin.contains("Earlier file content"));
    assert!(stdin.contains("previous-call"));
}

#[tokio::test]
async fn emits_usage_after_text_and_tool_events() {
    let temp = tempfile::tempdir().unwrap();
    let cli_output = json!({
        "type": "result",
        "is_error": false,
        "structured_output": {
            "response": "I found it.",
            "tool_calls": [{ "name": "read_file", "arguments": { "path": "notes.md" } }]
        },
        "usage": {
            "input_tokens": 11,
            "output_tokens": 5,
            "cache_read_input_tokens": 7,
            "cache_creation_input_tokens": 13
        }
    })
    .to_string();
    let (executable, _, _) = mock_cli(&temp, &cli_output, "", 0);
    let mut provider = provider();
    provider.executable = executable;

    let events = provider
        .chat_stream(
            vec![ChatMessage::new(ChatRole::User, "Read notes.md")],
            tools(),
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(&events[0], Ok(ChatEvent::TextDelta(text)) if text == "I found it."));
    assert!(matches!(&events[1], Ok(ChatEvent::ToolCall(call))
        if call.name == "read_file" && call.arguments == json!({ "path": "notes.md" })));
    assert!(matches!(
        &events[2],
        Ok(ChatEvent::TokenUsage {
            prompt_tokens: 31,
            completion_tokens: 5,
            cached_tokens: Some(7),
            cost: None,
        })
    ));
}

#[tokio::test]
async fn rejects_unknown_returned_bone_tool() {
    let temp = tempfile::tempdir().unwrap();
    let output = json!({
        "structured_output": {
            "response": "No.",
            "tool_calls": [{ "name": "shell", "arguments": { "command": "false" } }]
        }
    })
    .to_string();
    let (executable, _, _) = mock_cli(&temp, &output, "", 0);
    let mut provider = provider();
    provider.executable = executable;
    let error = expect_llm_error(
        provider
            .chat_stream(vec![ChatMessage::new(ChatRole::User, "Do it")], tools())
            .await,
    );
    assert!(matches!(error.kind, LlmErrorKind::Config));
    assert!(error.message.contains("unknown Bone tool `shell`"));
}

#[tokio::test]
async fn missing_cli_is_reported_without_network_validation() {
    let temp = tempfile::tempdir().unwrap();
    let mut provider = provider();
    provider.executable = temp.path().join("missing-claude");
    provider.validate().await.unwrap();

    let error = expect_llm_error(
        provider
            .chat_stream(vec![ChatMessage::new(ChatRole::User, "Hello")], vec![])
            .await,
    );
    assert!(matches!(error.kind, LlmErrorKind::Config));
    assert!(error.message.contains("not found on PATH"));
}

#[tokio::test]
async fn auth_failures_are_reported_without_echoing_cli_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let (executable, _, _) = mock_cli(&temp, "", "Not logged in; token=secret-value", 1);
    let mut provider = provider();
    provider.executable = executable;
    let error = expect_llm_error(
        provider
            .chat_stream(vec![ChatMessage::new(ChatRole::User, "Hello")], vec![])
            .await,
    );
    assert!(matches!(error.kind, LlmErrorKind::Auth));
    assert!(error.message.contains("sign in"));
    assert!(!error.message.contains("secret-value"));
}

#[test]
fn rejects_images_and_provider_specific_reasoning_history() {
    let image_message = ChatMessage::user_with_images(
        "What is this?",
        vec![crate::llm::provider::ImageData {
            media_type: "image/png".into(),
            data: "aW1hZ2U=".into(),
            ..Default::default()
        }],
    );
    let image_error = expect_llm_error(build_request(&[image_message], &[]));
    assert!(image_error.message.contains("cannot send image history"));

    let mut reasoning_message = ChatMessage::new(ChatRole::Assistant, "Answer");
    reasoning_message.reasoning = Some(crate::llm::provider::Reasoning {
        text: "private provider reasoning".into(),
        echo_field: None,
    });
    let reasoning_error = expect_llm_error(build_request(&[reasoning_message], &[]));
    assert!(
        reasoning_error
            .message
            .contains("cannot replay provider-specific reasoning")
    );

    let mut output_reasoning = ChatMessage::new(ChatRole::Assistant, "Answer");
    output_reasoning.output_sequence =
        vec![OutputItem::Reasoning(crate::llm::provider::ReasoningItem {
            id: "reasoning-id".into(),
            encrypted_content: "opaque".into(),
        })];
    let sequence_error = expect_llm_error(build_request(&[output_reasoning], &[]));
    assert!(
        sequence_error
            .message
            .contains("cannot replay provider-specific reasoning")
    );
}

#[test]
fn parses_anthropic_usage_from_direct_and_nested_envelopes() {
    let direct: Value = json!({
        "response": "direct",
        "tool_calls": [],
        "usage": {
            "input_tokens": 3,
            "output_tokens": 2,
            "cache_read_input_tokens": 5,
            "cache_creation_input_tokens": 7
        }
    });
    let parsed = parse_cli_response(direct.to_string().as_bytes(), &[]).unwrap();
    assert_eq!(parsed.text, "direct");
    assert_eq!(parsed.input_tokens, Some(3));
    assert_eq!(parsed.output_tokens, Some(2));
    assert_eq!(parsed.cache_read_input_tokens, Some(5));
    assert_eq!(parsed.cache_creation_input_tokens, Some(7));
    assert!(parsed.usage_present);

    let nested: Value = json!({
        "result": "{\"response\":\"nested\",\"tool_calls\":[],\"usage\":{\"input_tokens\":9,\"output_tokens\":4,\"cache_read_input_tokens\":1,\"cache_creation\":{\"ephemeral_5m_input_tokens\":6}}}"
    });
    let parsed = parse_cli_response(nested.to_string().as_bytes(), &[]).unwrap();
    assert_eq!(parsed.text, "nested");
    assert_eq!(parsed.input_tokens, Some(9));
    assert_eq!(parsed.output_tokens, Some(4));
    assert_eq!(parsed.cache_read_input_tokens, Some(1));
    assert_eq!(parsed.cache_creation_input_tokens, Some(6));
    assert!(parsed.usage_present);
}

#[test]
fn parses_json_result_fallback_and_rejects_non_object_arguments() {
    let valid: Value = json!({
        "result": "{\"response\":\"ok\",\"tool_calls\":[]}"
    });
    assert_eq!(
        parse_cli_response(valid.to_string().as_bytes(), &[])
            .unwrap()
            .text,
        "ok"
    );

    let invalid: Value = json!({
        "structured_output": {
            "response": "ok",
            "tool_calls": [{ "name": "read_file", "arguments": "not-an-object" }]
        }
    });
    let error = expect_llm_error(parse_cli_response(invalid.to_string().as_bytes(), &tools()));
    assert!(matches!(error.kind, LlmErrorKind::Parse));
}
