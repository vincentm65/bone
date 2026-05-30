use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio::fs;

use bone::tools::edit_file_unified::{EditFileUnifiedTool, preview_edit_file_unified, sha256_hex};
use bone::tools::types::Tool;

fn temp_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("bone-edit-file-unified-{name}-{nanos}"))
}

#[tokio::test]
async fn applies_minimal_codex_patch() {
    let path = temp_path("minimal.txt");
    fs::write(&path, "alpha\nbeta\ngamma\n")
        .await
        .expect("setup");
    let tool = EditFileUnifiedTool;

    let result = tool
        .execute(json!({
            "path": path,
            "patch": "@@\n-beta\n+two\n"
        }))
        .await
        .expect("success");

    assert!(result.contains("edited file"));
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "alpha\ntwo\ngamma\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn applies_full_codex_envelope() {
    let path = temp_path("envelope.txt");
    fs::write(&path, "old\n").await.expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch",
            path.display()
        )
    }))
    .await
    .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "new\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn accepts_unified_range_header_without_treating_it_as_context() {
    let path = temp_path("range-header.txt");
    fs::write(&path, "alpha\nbeta\ngamma\n")
        .await
        .expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@ -1,3 +1,3 @@\n alpha\n-beta\n+two\n gamma\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "alpha\ntwo\ngamma\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn accepts_old_only_unified_range_header() {
    let path = temp_path("old-range-header.txt");
    fs::write(&path, "alpha\nbeta\n").await.expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@ -2 @@\n-beta\n+two\n"
    }))
    .await
    .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "alpha\ntwo\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn applies_multiple_chunks_sequentially() {
    let path = temp_path("multi.txt");
    fs::write(&path, "one\ntwo\nthree\nfour\n")
        .await
        .expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@\n-one\n+ONE\n@@\n-four\n+FOUR\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "ONE\ntwo\nthree\nFOUR\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn small_anchor_survives_stale_surrounding_context() {
    let path = temp_path("small-anchor.rs");
    fs::write(
        &path,
        "pub mod command_policy;\npub mod dynamic;\npub mod edit_file;\npub mod edit_file_unified;\npub mod read_file;\n",
    )
    .await
    .expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@\n-pub mod edit_file;\n+pub mod edit_diff;\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "pub mod command_policy;\npub mod dynamic;\npub mod edit_diff;\npub mod edit_file_unified;\npub mod read_file;\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn context_header_disambiguates_repeated_lines() {
    let path = temp_path("context.txt");
    fs::write(
        &path,
        "fn first() {\n    value\n}\n\nfn second() {\n    value\n}\n",
    )
    .await
    .expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@ fn second() {\n-    value\n+    changed\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "fn first() {\n    value\n}\n\nfn second() {\n    changed\n}\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn recovers_from_whitespace_drift() {
    let path = temp_path("whitespace.txt");
    fs::write(&path, "fn main() {\n\tlet value = 1;   \n}\n")
        .await
        .expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@\n-  let value = 1;\n+  let value = 2;\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "fn main() {\n  let value = 2;\n}\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn pure_addition_appends_before_final_newline() {
    let path = temp_path("append.txt");
    fs::write(&path, "one\n").await.expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@\n+two\n"
    }))
    .await
    .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one\ntwo\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn preserves_file_when_later_chunk_fails() {
    let path = temp_path("atomic.txt");
    let original = "one\ntwo\nthree\n";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileUnifiedTool;

    let result = tool
        .execute(json!({
            "path": path,
            "patch": "@@\n-one\n+ONE\n@@\n-missing\n+MISSING\n"
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn rejects_patch_for_different_file() {
    let path = temp_path("wrong-file.txt");
    fs::write(&path, "old\n").await.expect("setup");
    let tool = EditFileUnifiedTool;

    let result = tool
        .execute(json!({
            "path": path,
            "patch": "*** Begin Patch\n*** Update File: other.txt\n@@\n-old\n+new\n*** End Patch"
        }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("patch updates other.txt"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "old\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn preview_uses_same_numbered_diff_display() {
    let path = temp_path("preview.txt");
    fs::write(&path, "old\n").await.expect("setup");

    let preview = preview_edit_file_unified(json!({
        "path": path,
        "patch": "@@\n-old\n+new\n"
    }))
    .await
    .expect("preview");

    assert_eq!(preview.before_hash, sha256_hex("old\n"));
    assert!(
        preview.diff.contains("preview.txt")
            || preview.diff.contains("bone-edit-file-unified-preview")
    );
    assert!(preview.diff.contains("- old"));
    assert!(preview.diff.contains("+ new"));
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn expected_hash_mismatch_preserves_file() {
    let path = temp_path("hash.txt");
    fs::write(&path, "old\n").await.expect("setup");
    let tool = EditFileUnifiedTool;

    let result = tool
        .execute(json!({
            "path": path,
            "patch": "@@\n-old\n+new\n",
            "expected_hash": sha256_hex("different")
        }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("file changed since preview"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "old\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn accepts_documented_diff_alias_and_schema_allows_it() {
    let path = temp_path("diff-alias.txt");
    fs::write(&path, "old\n").await.expect("setup");
    let tool = EditFileUnifiedTool;

    let definition = tool.definition();
    assert_eq!(definition.input_schema["required"], json!(["path"]));
    assert_eq!(
        definition.input_schema["anyOf"],
        json!([{ "required": ["patch"] }, { "required": ["diff"] }])
    );

    tool.execute(json!({
        "path": path,
        "diff": "@@\n-old\n+new\n"
    }))
    .await
    .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "new\n");
    let _ = fs::remove_file(&path).await;
}
