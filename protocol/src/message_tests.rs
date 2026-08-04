use super::{ChatMessage, ImageData};

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
