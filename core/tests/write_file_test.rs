mod common;

use std::path::PathBuf;

use serde_json::json;
use tokio::fs;

use bone_core::tools::types::Tool;
use bone_core::tools::write_file::WriteFileTool;

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

    assert!(result.contains("wrote"));
    assert!(result.contains("#"));
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
    let err = result.unwrap_err();
    assert!(
        err.contains("file already exists"),
        "unexpected error: {err}"
    );
    assert!(
        err.contains("edit_file") && err.contains("hashline"),
        "error should point at edit_file hashline: {err}"
    );
    assert_eq!(
        fs::read_to_string(&path)
            .await
            .expect("existing file should remain readable"),
        "original"
    );

    let _ = fs::remove_file(path).await;
}

#[cfg(unix)]
#[tokio::test]
async fn refuses_dangling_symlink() {
    use std::os::unix::fs::symlink;

    let path = temp_path("dangling-link.txt");
    let target = temp_path("missing-target.txt");
    symlink(&target, &path).expect("setup symlink");
    let tool = WriteFileTool;

    let result = tool
        .execute(json!({ "path": path, "content": "replacement" }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("file already exists"));
    assert!(
        fs::symlink_metadata(&path)
            .await
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let _ = fs::remove_file(path).await;
}
