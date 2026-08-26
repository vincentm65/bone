use super::parse_tool_output;

#[test]
fn preserves_plain_json_object_as_content() {
    let text = r#"{"cancelled":false,"answers":[{"value":"Blue"}]}"#;
    let output = parse_tool_output(text).unwrap();

    assert_eq!(output.content, text);
    assert!(output.images.is_empty());
    assert!(output.pane_page.is_none());
    assert!(output.state.is_none());
}

#[test]
fn preserves_non_string_content_key_as_plain_text() {
    // An MCP CallToolResult always carries `content` as an array. It must
    // not be misparsed as a tool-output envelope and emptied.
    let text = r#"{"content":[{"type":"text","text":"5"}]}"#;
    let output = parse_tool_output(text).unwrap();

    assert_eq!(output.content, text);
    assert!(output.images.is_empty());
    assert!(output.pane_page.is_none());
    assert!(output.state.is_none());
}

#[test]
fn still_parses_tool_output_envelopes() {
    let output = parse_tool_output(r#"{"content":"done","state":"saved"}"#).unwrap();

    assert_eq!(output.content, "done");
    assert_eq!(output.state.as_deref(), Some("saved"));
    assert!(!output.ephemeral_images);
}

#[test]
fn preserves_valid_ephemeral_png_bytes() {
    use base64::Engine;
    use sha2::{Digest, Sha256};

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    let expected = base64::engine::general_purpose::STANDARD
        .decode(PNG)
        .unwrap();
    let expected_hash = format!("{:x}", Sha256::digest(&expected));
    let envelope = serde_json::json!({
        "content": "seen",
        "images": [{
            "media_type": "image/png",
            "data": PNG,
            "width": 1,
            "height": 1,
            "sha256": expected_hash,
        }],
        "ephemeral_images": true,
    });
    let output = parse_tool_output(&envelope.to_string()).unwrap();
    let actual = base64::engine::general_purpose::STANDARD
        .decode(&output.images[0].data)
        .unwrap();

    assert_eq!(output.images.len(), 1);
    assert_eq!(output.images[0].media_type, "image/png");
    assert_eq!(output.images[0].width, Some(1));
    assert_eq!(output.images[0].height, Some(1));
    assert_eq!(
        output.images[0].sha256.as_deref(),
        Some(expected_hash.as_str())
    );
    assert_eq!(actual, expected);
    assert!(actual.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(output.ephemeral_images);
}
