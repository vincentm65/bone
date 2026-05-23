use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio::fs;

use super::{EditFileTool, sha256_hex};
use crate::tools::types::Tool;

fn temp_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("bone-edit-file-{name}-{nanos}"))
}

#[tokio::test]
async fn refuses_empty_search_string() {
    let path = temp_path("empty-search.txt");
    fs::write(&path, "original content").await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "path": path, "search": "", "replace": "replacement" }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("search must not be empty"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "original content");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn refuses_missing_search_string() {
    let path = temp_path("missing-search.txt");
    fs::write(&path, "hello world").await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "path": path, "search": "notfound", "replace": "x" }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("matched 0 times"));
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn refuses_duplicate_search_string_with_line_numbers() {
    let path = temp_path("dup-search.txt");
    fs::write(&path, "foo\nbar\nfoo").await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "path": path, "search": "foo", "replace": "baz" }))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("matched 2 times"));
    assert!(err.contains("line 1"));
    assert!(err.contains("line 3"));
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn successfully_edits_exactly_one_occurrence() {
    let path = temp_path("exact-one.txt");
    fs::write(&path, "hello world").await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "path": path, "search": "hello", "replace": "goodbye" }))
        .await
        .expect("success");

    assert!(result.contains("edited file"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "goodbye world");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn preserves_file_contents_on_failed_duplicate_search() {
    let path = temp_path("preserve-dup.txt");
    let original = "alpha beta alpha gamma";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "path": path, "search": "alpha", "replace": "delta" }))
        .await;

    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn preserves_file_contents_on_missing_search() {
    let path = temp_path("preserve-missing.txt");
    let original = "keep me safe";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "path": path, "search": "nope", "replace": "noway" }))
        .await;

    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn multi_edit_success() {
    let path = temp_path("multi.txt");
    fs::write(&path, "alpha beta gamma").await.expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "edits": [
            { "search": "alpha", "replace": "one" },
            { "search": "beta", "replace": "two" }
        ]
    }))
    .await
    .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one two gamma");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn search_replace_uses_replace_when_text_is_also_present() {
    let path = temp_path("stray-text.txt");
    fs::write(&path, "alpha beta").await.expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "edits": [
            { "search": "alpha", "replace": "one", "text": "ignored" }
        ]
    }))
    .await
    .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one beta");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn search_replace_accepts_text_as_replace_fallback() {
    let path = temp_path("text-fallback.txt");
    fs::write(&path, "alpha beta").await.expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "edits": [
            { "search": "alpha", "text": "one" }
        ]
    }))
    .await
    .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one beta");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn multi_edit_failure_is_atomic() {
    let path = temp_path("multi-atomic.txt");
    let original = "alpha beta gamma";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "path": path,
            "edits": [
                { "search": "alpha", "replace": "one" },
                { "search": "missing", "replace": "two" }
            ]
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn insert_before_and_after_and_delete_work() {
    let path = temp_path("ops.txt");
    fs::write(&path, "one\nthree\nfour\n").await.expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "edits": [
            { "insert_before": "three", "text": "two\n" },
            { "insert_after": "four", "text": "\nfive" },
            { "delete": "one\n" }
        ]
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "two\nthree\nfour\nfive\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn rewrite_replaces_whole_file() {
    let path = temp_path("rewrite.txt");
    fs::write(&path, "old").await.expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({ "path": path, "mode": "rewrite", "content": "new\nfile\n" }))
        .await
        .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "new\nfile\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn expected_hash_mismatch_preserves_file() {
    let path = temp_path("hash.txt");
    fs::write(&path, "old").await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "path": path,
            "search": "old",
            "replace": "new",
            "expected_hash": sha256_hex("different")
        }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("file changed since preview"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "old");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn zero_match_includes_closest_region_hint() {
    let path = temp_path("hint.txt");
    fs::write(
        &path,
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .await
    .expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "path": path,
            "search": "    println!(\"world\");",
            "replace": "    println!(\"universe\");"
        }))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("matched 0 times"));
    assert!(err.contains("Closest region"));
    assert!(err.contains("println!"));
    let _ = fs::remove_file(&path).await;
}

#[cfg(unix)]
#[tokio::test]
async fn preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let path = temp_path("perms.sh");
    fs::write(&path, "old").await.expect("setup");
    fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .await
        .expect("chmod");
    let tool = EditFileTool;

    tool.execute(json!({ "path": path, "search": "old", "replace": "new" }))
        .await
        .expect("success");

    let mode = fs::metadata(&path).await.unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
    let _ = fs::remove_file(&path).await;
}
