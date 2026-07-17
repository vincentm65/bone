mod common;

use std::path::PathBuf;

use serde_json::json;
use tokio::fs;

use bone_core::tools::types::{Tool, ToolExecutionContext};
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
        err.contains("edit_file") && err.contains("old_text"),
        "error should point at the simple edit_file contract: {err}"
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

#[tokio::test]
async fn live_writes_allow_parent_and_absolute_paths_and_snapshot_canonical_path() {
    let root = temp_path("anchored");
    fs::create_dir_all(&root).await.unwrap();
    let parent_path = root.join("../parent.txt");
    let absolute_path = temp_path("absolute.txt");
    let context = ToolExecutionContext::default().with_working_dir(root.clone());

    for path in [parent_path, absolute_path] {
        WriteFileTool
            .execute_output_live(
                json!({ "path": path, "content": "outside" }),
                None,
                context.clone(),
            )
            .await
            .expect("write outside working directory should succeed");
        let canonical = fs::canonicalize(&path).await.unwrap();
        assert_eq!(fs::read_to_string(&canonical).await.unwrap(), "outside");
        assert!(
            context
                .snapshots
                .read()
                .unwrap()
                .head(&canonical.to_string_lossy())
                .is_some()
        );
        fs::remove_file(canonical).await.unwrap();
    }
    let _ = fs::remove_dir_all(root).await;
}

#[cfg(unix)]
#[tokio::test]
async fn live_writes_allow_symlinked_parent_and_snapshot_canonical_path() {
    use std::os::unix::fs::symlink;

    let root = temp_path("linked-parent");
    let outside = temp_path("linked-target");
    fs::create_dir_all(&root).await.unwrap();
    fs::create_dir_all(&outside).await.unwrap();
    symlink(&outside, root.join("link")).unwrap();
    let context = ToolExecutionContext::default().with_working_dir(root.clone());

    WriteFileTool
        .execute_output_live(
            json!({ "path": "link/outside.txt", "content": "outside" }),
            None,
            context.clone(),
        )
        .await
        .expect("write through symlinked parent should succeed");
    let canonical = fs::canonicalize(outside.join("outside.txt")).await.unwrap();
    assert!(
        context
            .snapshots
            .read()
            .unwrap()
            .head(&canonical.to_string_lossy())
            .is_some()
    );

    let _ = fs::remove_dir_all(root).await;
    let _ = fs::remove_dir_all(outside).await;
}

#[tokio::test]
async fn live_write_recovers_poisoned_snapshot_store() {
    let root = temp_path("poisoned-snapshot");
    fs::create_dir_all(&root).await.unwrap();
    let context = ToolExecutionContext::default().with_working_dir(root.clone());
    let snapshots = context.snapshots.clone();
    let _ = std::thread::spawn(move || {
        let _guard = snapshots.write().unwrap();
        panic!("poison snapshot lock");
    })
    .join();

    WriteFileTool
        .execute_output_live(
            json!({ "path": "created.txt", "content": "created" }),
            None,
            context.clone(),
        )
        .await
        .expect("poisoned snapshot lock should be recovered");
    let canonical = fs::canonicalize(root.join("created.txt")).await.unwrap();
    assert!(
        context
            .snapshots
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .head(&canonical.to_string_lossy())
            .is_some()
    );

    let _ = fs::remove_dir_all(root).await;
}
