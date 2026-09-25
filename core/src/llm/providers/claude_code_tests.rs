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
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nIFS= read -r line || line=\nprintf '%s\\n' \"$line\" > {}\nprintf '%s' {}\nprintf '%s' {} >&2\nexit {}\n",
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

fn persistent_mock_cli(temp: &TempDir, outputs: &[String]) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let executable = temp.path().join("claude-persistent-mock");
    let args_path = temp.path().join("persistent-args.txt");
    let input_path = temp.path().join("persistent-input.ndjson");
    let spawn_path = temp.path().join("persistent-spawns.txt");
    let cases = outputs
        .iter()
        .enumerate()
        .map(|(index, output)| {
            format!(
                "        {}) printf '%s\\n' {};;\n",
                index + 1,
                shell_quote(output)
            )
        })
        .collect::<String>();
    let fallback = shell_quote(outputs.last().map(String::as_str).unwrap_or_default());
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nprintf 'spawn\\n' >> {}\nturn=0\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> {}\n  turn=$((turn + 1))\n  case \"$turn\" in\n{}        *) printf '%s\\n' {};;\n  esac\ndone\n",
        shell_quote(&args_path.display().to_string()),
        shell_quote(&spawn_path.display().to_string()),
        shell_quote(&input_path.display().to_string()),
        cases,
        fallback,
    );
    fs::write(&executable, script).unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    (executable, args_path, input_path, spawn_path)
}

fn failing_persistent_mock_cli(
    temp: &TempDir,
    output: &str,
) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let executable = temp.path().join("claude-failing-persistent-mock");
    let args_path = temp.path().join("failing-args.txt");
    let input_path = temp.path().join("failing-input.ndjson");
    let spawn_path = temp.path().join("failing-spawns.txt");
    let output = shell_quote(output);
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nprintf 'spawn\\n' >> {}\nspawn_number=$(wc -l < {})\nif [ \"$spawn_number\" -eq 1 ]; then\n  IFS= read -r line || exit 1\n  printf '%s\\n' \"$line\" >> {}\n  printf '%s\\n' {}\n  IFS= read -r line || exit 1\n  printf '%s\\n' \"$line\" >> {}\n  exit 1\nfi\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> {}\n  printf '%s\\n' {}\ndone\n",
        shell_quote(&args_path.display().to_string()),
        shell_quote(&spawn_path.display().to_string()),
        shell_quote(&spawn_path.display().to_string()),
        shell_quote(&input_path.display().to_string()),
        output,
        shell_quote(&input_path.display().to_string()),
        shell_quote(&input_path.display().to_string()),
        output,
    );
    fs::write(&executable, script).unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    (executable, args_path, input_path, spawn_path)
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
        "--verbose",
        "--json-schema",
        "--input-format",
        "--system-prompt",
        "--model",
        "--system-prompt-snapshot",
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
    let output_format = args
        .iter()
        .position(|arg| *arg == "--output-format")
        .unwrap();
    assert_eq!(args[output_format + 1], "stream-json");
    let input_format = args
        .iter()
        .position(|arg| *arg == "--input-format")
        .unwrap();
    assert_eq!(args[input_format + 1], "stream-json");
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
async fn persistent_cli_reuses_process_sends_history_delta_and_maps_cache_usage() {
    let temp = tempfile::tempdir().unwrap();
    let first_output = format!(
        "{}\n{}",
        json!({ "type": "system", "subtype": "init" }),
        json!({
            "type": "result",
            "is_error": false,
            "structured_output": { "response": "first answer", "tool_calls": [] },
            "usage": {
                "input_tokens": 10,
                "output_tokens": 2,
                "cache_read_input_tokens": 3,
                "cache_creation_input_tokens": 4
            }
        })
    );
    let second_output = format!(
        "{}\n{}",
        json!({ "type": "assistant", "message": { "content": [] } }),
        json!({
            "type": "result",
            "is_error": false,
            "structured_output": { "response": "second answer", "tool_calls": [] },
            "usage": {
                "input_tokens": 5,
                "output_tokens": 3,
                "cache_read_input_tokens": 8,
                "cache_creation_input_tokens": 1
            }
        })
    );
    let (executable, args_path, input_path, spawn_path) =
        persistent_mock_cli(&temp, &[first_output, second_output]);
    let mut provider = provider();
    provider.executable = executable;
    let context = ProviderRequestContext {
        cache_scope: Some("fake-cache-scope".into()),
        ..Default::default()
    };
    let system = ChatMessage::new(ChatRole::System, "stable system context");
    let first_user = ChatMessage::new(ChatRole::User, "first question");
    let first_messages = vec![system.clone(), first_user.clone()];
    let first_events = provider
        .chat_stream_with_context(first_messages.clone(), vec![], context.clone())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(
        first_events.last(),
        Some(Ok(ChatEvent::TokenUsage {
            prompt_tokens: 17,
            completion_tokens: 2,
            cached_tokens: Some(3),
            cost: None,
        }))
    ));

    let second_messages = vec![
        system,
        first_user,
        ChatMessage::new(ChatRole::Assistant, "first answer"),
        ChatMessage::new(ChatRole::User, "second question"),
    ];
    let second_events = provider
        .chat_stream_with_context(second_messages, vec![], context)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(
        second_events.last(),
        Some(Ok(ChatEvent::TokenUsage {
            prompt_tokens: 14,
            completion_tokens: 3,
            cached_tokens: Some(8),
            cost: None,
        }))
    ));

    let inputs = fs::read_to_string(input_path).unwrap();
    let input_lines = inputs
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap());
    let input_lines = input_lines.collect::<Vec<_>>();
    assert_eq!(input_lines.len(), 2);
    let first_prompt = input_lines[0]["message"]["content"].as_str().unwrap();
    let second_prompt = input_lines[1]["message"]["content"].as_str().unwrap();
    assert!(first_prompt.contains("Full conversation history"));
    assert!(first_prompt.contains("first question"));
    assert!(second_prompt.contains("Conversation history delta"));
    assert!(second_prompt.contains("second question"));
    assert!(second_prompt.contains("first answer"));
    assert!(!second_prompt.contains("first question"));
    assert_eq!(fs::read_to_string(spawn_path).unwrap().lines().count(), 1);

    let args = fs::read_to_string(args_path).unwrap();
    assert!(args.contains("stream-json"));
    assert!(args.contains("--no-session-persistence"));
}

#[tokio::test]
async fn history_replacement_discards_session_and_sends_full_history() {
    let temp = tempfile::tempdir().unwrap();
    let output = json!({
        "type": "result",
        "is_error": false,
        "structured_output": { "response": "answer", "tool_calls": [] }
    })
    .to_string();
    let (executable, _, input_path, spawn_path) =
        persistent_mock_cli(&temp, &[output.clone(), output]);
    let mut provider = provider();
    provider.executable = executable;

    provider
        .chat_stream(
            vec![ChatMessage::new(ChatRole::User, "original question")],
            vec![],
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    provider
        .chat_stream(
            vec![ChatMessage::new(ChatRole::User, "replacement question")],
            vec![],
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    let input_lines = fs::read_to_string(input_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(input_lines.len(), 2);
    let first_prompt = input_lines[0]["message"]["content"].as_str().unwrap();
    let replacement_prompt = input_lines[1]["message"]["content"].as_str().unwrap();
    assert!(first_prompt.contains("Full conversation history"));
    assert!(first_prompt.contains("original question"));
    assert!(replacement_prompt.contains("Full conversation history"));
    assert!(replacement_prompt.contains("replacement question"));
    assert!(!replacement_prompt.contains("Conversation history delta"));
    assert!(!replacement_prompt.contains("original question"));
    assert_eq!(fs::read_to_string(spawn_path).unwrap().lines().count(), 2);
}

#[tokio::test]
async fn process_failure_discards_session_before_the_next_request() {
    let temp = tempfile::tempdir().unwrap();
    let output = json!({
        "type": "result",
        "is_error": false,
        "structured_output": { "response": "answer", "tool_calls": [] }
    })
    .to_string();
    let (executable, _, input_path, spawn_path) = failing_persistent_mock_cli(&temp, &output);
    let mut provider = provider();
    provider.executable = executable;

    provider
        .chat_stream(
            vec![ChatMessage::new(ChatRole::User, "first question")],
            vec![],
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    let error = expect_llm_error(
        provider
            .chat_stream(
                vec![
                    ChatMessage::new(ChatRole::User, "first question"),
                    ChatMessage::new(ChatRole::Assistant, "first answer"),
                    ChatMessage::new(ChatRole::User, "failed question"),
                ],
                vec![],
            )
            .await,
    );
    assert!(matches!(error.kind, LlmErrorKind::Server(500)));

    provider
        .chat_stream(
            vec![
                ChatMessage::new(ChatRole::User, "first question"),
                ChatMessage::new(ChatRole::Assistant, "first answer"),
                ChatMessage::new(ChatRole::User, "failed question"),
                ChatMessage::new(ChatRole::User, "retry question"),
            ],
            vec![],
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    let input_lines = fs::read_to_string(input_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(input_lines.len(), 3);
    let retry_prompt = input_lines[2]["message"]["content"].as_str().unwrap();
    assert!(retry_prompt.contains("Full conversation history"));
    assert!(retry_prompt.contains("first question"));
    assert!(retry_prompt.contains("first answer"));
    assert!(retry_prompt.contains("retry question"));
    assert!(!retry_prompt.contains("Conversation history delta"));
    assert_eq!(fs::read_to_string(spawn_path).unwrap().lines().count(), 2);
}

#[tokio::test]
async fn converts_cumulative_usage_to_per_request_deltas() {
    let temp = tempfile::tempdir().unwrap();
    let first_output = json!({
        "type": "result",
        "is_error": false,
        "structured_output": { "response": "first answer", "tool_calls": [] },
        "usage": {
            "cumulative": true,
            "total_input_tokens": 10,
            "total_output_tokens": 2,
            "total_cache_read_input_tokens": 3,
            "total_cache_creation_input_tokens": 4
        }
    })
    .to_string();
    let second_output = json!({
        "type": "result",
        "is_error": false,
        "structured_output": { "response": "second answer", "tool_calls": [] },
        "usage": {
            "cumulative": true,
            "total_input_tokens": 15,
            "total_output_tokens": 5,
            "total_cache_read_input_tokens": 8,
            "total_cache_creation_input_tokens": 6
        }
    })
    .to_string();
    let (executable, _, _, spawn_path) = persistent_mock_cli(&temp, &[first_output, second_output]);
    let mut provider = provider();
    provider.executable = executable;
    let context = ProviderRequestContext {
        cache_scope: Some("cumulative-cache-scope".into()),
        ..Default::default()
    };

    let first_messages = vec![ChatMessage::new(ChatRole::User, "first question")];
    let first_events = provider
        .chat_stream_with_context(first_messages.clone(), vec![], context.clone())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(
        first_events.last(),
        Some(Ok(ChatEvent::TokenUsage {
            prompt_tokens: 17,
            completion_tokens: 2,
            cached_tokens: Some(3),
            cost: None,
        }))
    ));

    let second_events = provider
        .chat_stream_with_context(
            vec![
                first_messages[0].clone(),
                ChatMessage::new(ChatRole::Assistant, "first answer"),
                ChatMessage::new(ChatRole::User, "second question"),
            ],
            vec![],
            context,
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(
        second_events.last(),
        Some(Ok(ChatEvent::TokenUsage {
            prompt_tokens: 12,
            completion_tokens: 3,
            cached_tokens: Some(5),
            cost: None,
        }))
    ));
    assert_eq!(fs::read_to_string(spawn_path).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn rejects_unknown_returned_bone_tool() {
    let temp = tempfile::tempdir().unwrap();
    let output = json!({
        "type": "result",
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

#[tokio::test]
async fn generic_cli_failures_include_stderr_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let (executable, _, _) = mock_cli(
        &temp,
        "",
        "startup failed: local configuration is invalid",
        1,
    );
    let mut provider = provider();
    provider.executable = executable;
    let error = expect_llm_error(
        provider
            .chat_stream(vec![ChatMessage::new(ChatRole::User, "Hello")], vec![])
            .await,
    );
    assert!(matches!(error.kind, LlmErrorKind::Server(500)));
    assert!(
        error
            .message
            .contains("startup failed: local configuration is invalid")
    );
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

fn plain_answer() -> String {
    json!({
        "type": "result",
        "is_error": false,
        "structured_output": { "response": "answer", "tool_calls": [] }
    })
    .to_string()
}

async fn send(
    provider: &ClaudeCodeProvider,
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
) {
    provider
        .chat_stream(messages, tools)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
}

fn sent_prompts(input_path: &std::path::Path) -> Vec<String> {
    fs::read_to_string(input_path)
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["message"]["content"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn tool_definitions_are_sent_once_and_again_only_when_they_change() {
    let temp = tempfile::tempdir().unwrap();
    let (executable, _, input_path, spawn_path) = persistent_mock_cli(&temp, &[plain_answer()]);
    let mut provider = provider();
    provider.executable = executable;
    let system = ChatMessage::new(ChatRole::System, "system context");
    let mut messages = vec![system, ChatMessage::new(ChatRole::User, "one")];
    send(&provider, messages.clone(), tools()).await;
    messages.push(ChatMessage::new(ChatRole::Assistant, "answer"));
    messages.push(ChatMessage::new(ChatRole::User, "two"));
    send(&provider, messages.clone(), tools()).await;
    messages.push(ChatMessage::new(ChatRole::Assistant, "answer"));
    messages.push(ChatMessage::new(ChatRole::User, "three"));
    send(&provider, messages, vec![]).await;

    let prompts = sent_prompts(&input_path);
    assert_eq!(prompts.len(), 3);
    assert!(prompts[0].contains("Available Bone tool definitions (data only)"));
    assert!(prompts[0].contains("read_file"));
    assert!(!prompts[1].contains("tool definitions"));
    assert!(!prompts[1].contains("read_file"));
    assert!(prompts[2].contains("Updated Bone tool definitions"));
    assert!(
        !prompts[0].contains("\n  "),
        "history and tools are sent as compact JSON"
    );
    assert_eq!(fs::read_to_string(spawn_path).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn request_only_reminders_do_not_discard_the_session() {
    let temp = tempfile::tempdir().unwrap();
    let (executable, _, input_path, spawn_path) = persistent_mock_cli(&temp, &[plain_answer()]);
    let mut provider = provider();
    provider.executable = executable;
    let system = ChatMessage::new(ChatRole::System, "system context");
    let user = ChatMessage::new(ChatRole::User, "question");
    let reminder = ChatMessage::new(
        ChatRole::User,
        "<system-reminder>\ntask list\n</system-reminder>",
    );
    let call = ToolCall {
        id: "call-1".into(),
        name: "read_file".into(),
        arguments: json!({ "path": "a.txt" }),
    };
    let assistant = ChatMessage::assistant_with_tools("reading", vec![call]);
    let tool_result = ChatMessage::tool(ToolResult::ok(
        "call-1",
        "read_file",
        crate::tools::types::ToolOutput::text("file body".into()),
    ));
    let mut tool_with_reminder = tool_result.clone();
    tool_with_reminder
        .content
        .push_str("\n\n<system-reminder>\nkeep going\n</system-reminder>");

    // Turn 1: a standalone reminder, then a reminder inside the tool result.
    send(
        &provider,
        vec![system.clone(), user.clone(), reminder.clone()],
        tools(),
    )
    .await;
    send(
        &provider,
        vec![
            system.clone(),
            user.clone(),
            reminder,
            assistant.clone(),
            tool_with_reminder,
        ],
        tools(),
    )
    .await;
    // Turn 2 is rebuilt from the transcript, which never held the reminders.
    send(
        &provider,
        vec![
            system,
            user,
            assistant,
            tool_result,
            ChatMessage::new(ChatRole::Assistant, "done"),
            ChatMessage::new(ChatRole::User, "next question"),
        ],
        tools(),
    )
    .await;

    let prompts = sent_prompts(&input_path);
    assert_eq!(prompts.len(), 3);
    assert!(prompts[2].contains("Conversation history delta"));
    assert!(prompts[2].contains("next question"));
    assert!(!prompts[2].contains("file body"));
    assert!(!prompts[2].contains("retracted"));
    assert_eq!(fs::read_to_string(spawn_path).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn system_context_changes_are_sent_as_updates_without_a_new_session() {
    let temp = tempfile::tempdir().unwrap();
    let (executable, _, input_path, spawn_path) = persistent_mock_cli(&temp, &[plain_answer()]);
    let mut provider = provider();
    provider.executable = executable;
    let user = ChatMessage::new(ChatRole::User, "question");
    send(
        &provider,
        vec![
            ChatMessage::new(ChatRole::System, "memory v1"),
            user.clone(),
        ],
        vec![],
    )
    .await;
    send(
        &provider,
        vec![
            ChatMessage::new(ChatRole::System, "memory v2"),
            user,
            ChatMessage::new(ChatRole::Assistant, "answer"),
            ChatMessage::new(ChatRole::User, "follow-up"),
        ],
        vec![],
    )
    .await;

    let prompts = sent_prompts(&input_path);
    assert!(!prompts[0].contains("Updated Bone conversation system context"));
    assert!(prompts[1].contains("Updated Bone conversation system context"));
    assert!(prompts[1].contains("memory v2"));
    assert!(!prompts[1].contains("\"question\""));
    assert_eq!(fs::read_to_string(spawn_path).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn side_requests_are_retracted_instead_of_discarding_the_session() {
    let temp = tempfile::tempdir().unwrap();
    let (executable, _, input_path, spawn_path) = persistent_mock_cli(&temp, &[plain_answer()]);
    let mut provider = provider();
    provider.executable = executable;
    let system = ChatMessage::new(ChatRole::System, "system context");
    let user = ChatMessage::new(ChatRole::User, "question");
    let answer = ChatMessage::new(ChatRole::Assistant, "answer");
    send(&provider, vec![system.clone(), user.clone()], vec![]).await;
    // A recap-style private request extends the history with its own prompt.
    send(
        &provider,
        vec![
            system.clone(),
            user.clone(),
            answer.clone(),
            ChatMessage::new(ChatRole::User, "summarize the conversation"),
        ],
        vec![],
    )
    .await;
    send(
        &provider,
        vec![
            system,
            user,
            answer,
            ChatMessage::new(ChatRole::User, "real follow-up"),
        ],
        vec![],
    )
    .await;

    let prompts = sent_prompts(&input_path);
    assert_eq!(prompts.len(), 3);
    assert!(prompts[2].contains("retracted the last 1 conversation message(s)"));
    assert!(prompts[2].contains("real follow-up"));
    assert!(!prompts[2].contains("\"answer\""));
    assert_eq!(fs::read_to_string(spawn_path).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn multi_call_turns_report_the_final_call_as_context() {
    let temp = tempfile::tempdir().unwrap();
    // The CLI made two API calls: usage sums both, `iterations` holds the last.
    let cli_output = json!({
        "type": "result",
        "is_error": false,
        "structured_output": { "response": "done", "tool_calls": [] },
        "usage": {
            "input_tokens": 6,
            "output_tokens": 40,
            "cache_read_input_tokens": 44166,
            "cache_creation_input_tokens": 43594,
            "iterations": [{
                "input_tokens": 4,
                "output_tokens": 30,
                "cache_read_input_tokens": 43608,
                "cache_creation_input_tokens": 544
            }]
        }
    })
    .to_string();
    let (executable, _, _) = mock_cli(&temp, &cli_output, "", 0);
    let mut provider = provider();
    provider.executable = executable;

    let events = provider
        .chat_stream(vec![ChatMessage::new(ChatRole::User, "hi")], vec![])
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let usage = events
        .iter()
        .filter_map(|event| match event {
            Ok(ChatEvent::TokenUsage {
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                ..
            }) => Some((*prompt_tokens, *completion_tokens, *cached_tokens)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        usage,
        vec![(43610, 0, Some(558)), (44156, 40, Some(43608))],
        "earlier calls first, then the final call whose prompt is the context"
    );
}
