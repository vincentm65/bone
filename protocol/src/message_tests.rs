use super::{ChatMessage, ChatRole, ImageData};

#[test]
fn image_metadata_round_trips() {
    let message = ChatMessage::user_with_images(
        "look",
        vec![ImageData {
            media_type: "image/png".into(),
            data: "base64".into(),
            width: Some(1920),
            height: Some(1080),
            sha256: Some("abc123".into()),
        }],
    );

    let json = serde_json::to_string(&message).unwrap();
    let decoded: ChatMessage = serde_json::from_str(&json).unwrap();

    assert_eq!(decoded, message);
}

#[test]
fn legacy_image_data_without_metadata_still_deserializes() {
    let image: ImageData =
        serde_json::from_str(r#"{"media_type":"image/png","data":"base64"}"#).unwrap();

    assert_eq!(image.width, None);
    assert_eq!(image.height, None);
    assert_eq!(image.sha256, None);
    assert_eq!(
        serde_json::to_value(image).unwrap(),
        serde_json::json!({"media_type": "image/png", "data": "base64"})
    );
}

#[test]
fn synthetic_flag_is_omitted_when_false() {
    let message = ChatMessage::user_with_images("look", Vec::new());

    let value = serde_json::to_value(&message).unwrap();

    assert!(
        value.get("synthetic").is_none(),
        "synthetic should not be serialized when false: {value}"
    );
}

#[test]
fn synthetic_flag_round_trips_when_set() {
    let mut message = ChatMessage::user_with_images("Image output from read_file:", Vec::new());
    message.synthetic = true;

    let json = serde_json::to_string(&message).unwrap();
    let decoded: ChatMessage = serde_json::from_str(&json).unwrap();

    assert!(decoded.synthetic);
    assert_eq!(decoded, message);
    assert_eq!(serde_json::to_value(&message).unwrap()["synthetic"], true);
}

#[test]
fn is_synthetic_relay_recognizes_flag_prefix_and_plain_user_text() {
    // Runtime relay marked with the flag.
    let mut flagged = ChatMessage::new(ChatRole::User, "anything at all");
    flagged.synthetic = true;
    assert!(flagged.is_synthetic_relay());

    // Legacy row from before the flag existed, recognized by the relay prefix.
    let legacy = ChatMessage::new(ChatRole::User, "Image output from read_file:");
    assert!(legacy.is_synthetic_relay());

    // Ordinary user input is not a relay.
    let normal = ChatMessage::new(ChatRole::User, "please read the file");
    assert!(!normal.is_synthetic_relay());

    // The prefix only marks user-role relays, not assistant text.
    let assistant = ChatMessage::new(ChatRole::Assistant, "Image output from read_file:");
    assert!(!assistant.is_synthetic_relay());
}
