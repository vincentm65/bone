use bone::llm::provider::{ChatMessage, ChatRole, ImageData};
use bone::llm::providers::codex::{
    CodexRequest, build_codex_messages, build_instructions, codex_tools,
};
use bone::tools::ToolDefinition;

#[test]
fn test_build_instructions_empty() {
    let messages = vec![ChatMessage::new(ChatRole::User, "Hello")];
    let instructions = build_instructions(&messages);
    assert_eq!(instructions, "You are a helpful assistant.");
}

#[test]
fn test_build_instructions_with_system() {
    let messages = vec![
        ChatMessage::new(ChatRole::System, "You are a coding assistant."),
        ChatMessage::new(ChatRole::User, "Hello"),
    ];
    let instructions = build_instructions(&messages);
    assert_eq!(instructions, "You are a coding assistant.");
}

#[test]
fn test_build_codex_messages_user() {
    let messages = vec![ChatMessage::new(ChatRole::User, "Hello world")];
    let items = build_codex_messages(messages);
    assert_eq!(items.len(), 1);
}

#[test]
fn test_build_codex_messages_user_images() {
    let messages = vec![ChatMessage::user_with_images(
        "look",
        vec![ImageData {
            media_type: "image/png".to_string(),
            data: "abc".to_string(),
        }],
    )];
    let items = build_codex_messages(messages);
    let json = serde_json::to_value(&items[0]).unwrap();
    assert_eq!(json["role"], "user");
    assert_eq!(json["content"][0]["type"], "input_text");
    assert_eq!(json["content"][0]["text"], "look");
    assert_eq!(json["content"][1]["type"], "input_image");
    assert_eq!(json["content"][1]["image_url"], "data:image/png;base64,abc");
}

#[test]
fn test_build_codex_messages_system_skipped() {
    let messages = vec![
        ChatMessage::new(ChatRole::System, "System prompt"),
        ChatMessage::new(ChatRole::User, "Hello"),
    ];
    let items = build_codex_messages(messages);
    assert_eq!(items.len(), 1);
}

#[test]
fn test_codex_tools_conversion() {
    let tools = vec![ToolDefinition {
        name: "test_tool".to_string(),
        description: "A test tool".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "text": {"type": "string"}
            }
        }),
    }];
    let codex_tools = codex_tools(tools);
    assert_eq!(codex_tools.len(), 1);
    assert_eq!(codex_tools[0].name, "test_tool");
    assert_eq!(codex_tools[0].description, "A test tool");
}

#[test]
fn test_codex_tools_sorted_by_name() {
    let tools = vec![
        ToolDefinition {
            name: "zeta".to_string(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        ToolDefinition {
            name: "alpha".to_string(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        },
    ];
    let names: Vec<_> = codex_tools(tools)
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, vec!["alpha", "zeta"]);
}
#[test]
fn test_build_codex_messages_reasoning_items() {
    use bone::llm::provider::ReasoningItem;

    let mut msg = ChatMessage::new(ChatRole::Assistant, "answer");
    msg.reasoning_items = vec![
        ReasoningItem {
            id: "rs_abc".to_string(),
            encrypted_content: "enc123".to_string(),
        },
    ];

    let items = build_codex_messages(vec![msg]);
    let json = serde_json::to_value(&items).unwrap();

    // Reasoning item should come before the assistant text.
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["type"], "reasoning");
    assert_eq!(arr[0]["id"], "rs_abc");
    assert!(arr[0]["summary"].is_array());
    assert_eq!(arr[0]["summary"].as_array().unwrap().len(), 0);
    assert_eq!(arr[0]["encrypted_content"], "enc123");
    assert_eq!(arr[1]["role"], "assistant");
}

#[test]
fn test_build_codex_messages_preserves_interleaved_order() {
    use bone::llm::provider::{OutputItem, ReasoningItem};
    use bone::tools::ToolCall;

    // Backend emitted: reasoning A, call A, reasoning B, call B. The replayed
    // input must keep that exact order, not group reasoning before calls.
    let mut msg = ChatMessage::new(ChatRole::Assistant, "");
    msg.output_sequence = vec![
        OutputItem::Reasoning(ReasoningItem {
            id: "rs_a".to_string(),
            encrypted_content: "encA".to_string(),
        }),
        OutputItem::ToolCall(ToolCall {
            id: "call_a".to_string(),
            name: "tool_a".to_string(),
            arguments: serde_json::json!({"x": 1}),
        }),
        OutputItem::Reasoning(ReasoningItem {
            id: "rs_b".to_string(),
            encrypted_content: "encB".to_string(),
        }),
        OutputItem::ToolCall(ToolCall {
            id: "call_b".to_string(),
            name: "tool_b".to_string(),
            arguments: serde_json::json!({"y": 2}),
        }),
    ];

    let items = build_codex_messages(vec![msg]);
    let json = serde_json::to_value(&items).unwrap();
    let arr = json.as_array().unwrap();

    assert_eq!(arr.len(), 4);
    assert_eq!(arr[0]["type"], "reasoning");
    assert_eq!(arr[0]["id"], "rs_a");
    assert_eq!(arr[1]["type"], "function_call");
    assert_eq!(arr[1]["call_id"], "call_a");
    assert_eq!(arr[2]["type"], "reasoning");
    assert_eq!(arr[2]["id"], "rs_b");
    assert_eq!(arr[3]["type"], "function_call");
    assert_eq!(arr[3]["call_id"], "call_b");
}

#[test]
fn test_codex_request_serializes_prompt_cache_key() {
    let request = CodexRequest {
        model: "gpt-5-codex".to_string(),
        instructions: "be helpful".to_string(),
        input: build_codex_messages(vec![ChatMessage::new(ChatRole::User, "hi")]),
        stream: true,
        store: false,
        temperature: None,
        top_p: None,
        tools: None,
        tool_choice: None,
        prompt_cache_key: Some("bone-codex-thread-42".to_string()),
        include: None,
    };

    let json = serde_json::to_value(request).unwrap();
    assert_eq!(json["prompt_cache_key"], "bone-codex-thread-42");
    assert_eq!(json["input"].as_array().unwrap().len(), 1);
}

#[test]
fn test_codex_request_omits_optional_fields_when_unset() {
    // A first-turn request with no conversation id and no tools must not emit
    // `prompt_cache_key` or `tool_choice` — only stable, Codex-shaped fields.
    let request = CodexRequest {
        model: "gpt-5-codex".to_string(),
        instructions: "be helpful".to_string(),
        input: build_codex_messages(vec![ChatMessage::new(ChatRole::User, "hi")]),
        stream: true,
        store: false,
        temperature: None,
        top_p: None,
        tools: None,
        tool_choice: None,
        prompt_cache_key: None,
        include: None,
    };

    let json = serde_json::to_value(request).unwrap();
    let obj = json.as_object().unwrap();
    assert!(!obj.contains_key("prompt_cache_key"));
    assert!(!obj.contains_key("tool_choice"));
    assert!(!obj.contains_key("tools"));
    assert_eq!(json["store"], false);
}
