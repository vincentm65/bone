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
async fn applies_simple_unified_diff() {
    let path = temp_path("simple.txt");
    fs::write(&path, "alpha\nbeta\ngamma\n")
        .await
        .expect("setup");
    let tool = EditFileUnifiedTool;

    let result = tool
        .execute(json!({
            "path": path,
            "patch": "--- a/simple.txt\n+++ b/simple.txt\n@@ -1,3 +1,3 @@\n alpha\n-beta\n+two\n gamma\n"
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
async fn applies_multiple_hunks_atomically() {
    let path = temp_path("multi.txt");
    fs::write(&path, "one\ntwo\nthree\nfour\n")
        .await
        .expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@ -1,2 +1,2 @@\n-one\n+ONE\n two\n@@ -3,2 +3,2 @@\n three\n-four\n+FOUR\n"
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
async fn context_free_hunk_prepends_at_start_of_file() {
    let path = temp_path("prepend.txt");
    fs::write(&path, "one\ntwo\n").await.expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@ -0,0 +1 @@\n+zero\n"
    }))
    .await
    .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "zero\none\ntwo\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn context_free_hunk_inserts_after_old_start_line() {
    let path = temp_path("insert-after.txt");
    fs::write(&path, "one\ntwo\n").await.expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@ -1,0 +2 @@\n+one-and-a-half\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\none-and-a-half\ntwo\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn preserves_file_when_later_hunk_fails() {
    let path = temp_path("atomic.txt");
    let original = "one\ntwo\nthree\n";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileUnifiedTool;

    let result = tool
        .execute(json!({
            "path": path,
            "patch": "@@ -1,2 +1,2 @@\n-one\n+ONE\n two\n@@ -10,2 +10,2 @@\n-missing\n+MISSING\n nope\n"
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn rejects_exact_matches_that_start_mid_line() {
    let path = temp_path("mid-line.txt");
    let original = "prefixfoo\n";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileUnifiedTool;

    let result = tool
        .execute(json!({
            "path": path,
            "patch": "@@ -1 +1 @@\n-foo\n+bar\n"
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
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
        "patch": "@@ -1,3 +1,3 @@\n fn main() {\n-  let value = 1;\n+  let value = 2;\n }\n"
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
async fn fuzzy_recovers_near_hunk_context() {
    let path = temp_path("fuzzy.txt");
    fs::write(
        &path,
        "fn score(input: i32) -> i32 {\n    let adjusted = input + 1;\n    adjusted * 2\n}\n",
    )
    .await
    .expect("setup");
    let tool = EditFileUnifiedTool;

    tool.execute(json!({
        "path": path,
        "patch": "@@ -1,4 +1,3 @@\n fn score(input: i32) -> i32 {\n-    let adjusted = input + 2;\n-    adjusted * 2\n+    input * 3\n }\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "fn score(input: i32) -> i32 {\n    input * 3\n}\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn rejects_low_confidence_hunk() {
    let path = temp_path("low.txt");
    let original = "fn alpha() {\n    println!(\"alpha\");\n}\n";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileUnifiedTool;

    let result = tool
        .execute(json!({
            "path": path,
            "patch": "@@ -1,4 +1,1 @@\n-struct Missing {\n-    value: usize,\n-}\n+replacement\n"
        }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not confident enough"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn preview_uses_same_numbered_diff_display() {
    let path = temp_path("preview.txt");
    fs::write(&path, "old\n").await.expect("setup");

    let preview = preview_edit_file_unified(json!({
        "path": path,
        "patch": "@@ -1 +1 @@\n-old\n+new\n"
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
            "patch": "@@ -1 +1 @@\n-old\n+new\n",
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
        "diff": "@@ -1 +1 @@\n-old\n+new\n"
    }))
    .await
    .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "new\n");
    let _ = fs::remove_file(&path).await;
}
