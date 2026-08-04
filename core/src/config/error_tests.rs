use super::*;

#[test]
fn display_includes_document_operation_and_setting() {
    let error = ConfigError::persist(Path::new("/tmp/config.yaml"), "permission denied")
        .at_setting("general.approval");
    assert_eq!(
        error.to_string(),
        "could not persist /tmp/config.yaml at general.approval: permission denied"
    );
}
