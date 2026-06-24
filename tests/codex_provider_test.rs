use bone::llm::provider::{ChatMessage, ChatRole, ImageData};
use bone::llm::providers::codex::{build_codex_messages, build_instructions, codex_tools};
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
