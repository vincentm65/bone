mod common;

use std::path::PathBuf;

use serde_json::json;
use tokio::fs;

use bone::tools::types::Tool;
use bone::tools::write_file::WriteFileTool;

fn temp_path(name: &str) -> PathBuf {
    common::temp_path(&format!("write-file-{name}"))
}

#[tokio::test]
async fn creates_new_file() {
    let path = temp_path("creates").join("nested/file.txt");
    let tool = WriteFileTool;

    let result = tool
        .execute(json!({ "path": path, "content": "hello" }))
        .await
        .expect("write_file should create a new file");

    assert_eq!(result, "wrote 5 bytes");
    assert_eq!(
        fs::read_to_string(&path)
            .await
            .expect("created file should be readable"),
        "hello"
    );

    // Clean up temp dir — best effort
    if let Some(grandparent) = path.parent().and_then(|p| p.parent()) {
        let _ = fs::remove_dir_all(grandparent).await;
    }
}

#[tokio::test]
async fn refuses_to_overwrite_existing_file() {
    let path = temp_path("exists.txt");
    fs::write(&path, "original")
        .await
        .expect("test setup should create existing file");
    let tool = WriteFileTool;

    let result = tool
        .execute(json!({ "path": path, "content": "replacement" }))
        .await;

    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(&path)
            .await
            .expect("existing file should remain readable"),
        "original"
    );

    let _ = fs::remove_file(path).await;
}
