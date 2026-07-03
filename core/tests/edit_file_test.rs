mod common;

use std::path::PathBuf;

use serde_json::json;
use tokio::fs;

use bone_core::tools::edit_file::{EditFileTool, preview_edit_file, sha256_hex};
use bone_core::tools::types::Tool;

fn temp_path(name: &str) -> PathBuf {
    common::temp_path(&format!("edit-file-{name}"))
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
    fs::write(&path, "fn main() {\n    println!(\"hello\");\n}\n")
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
    assert!(err.contains("candidate"));
    assert!(err.contains("println!"));
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn trailing_whitespace_mismatch_recovers() {
    let path = temp_path("trailing-space.txt");
    fs::write(&path, "fn main() {\n    let value = 1;   \n}\n")
        .await
        .expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "search": "    let value = 1;\n",
        "replace": "    let value = 2;\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "fn main() {\n    let value = 2;\n}\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn crlf_mismatch_recovers() {
    let path = temp_path("crlf.txt");
    fs::write(&path, "alpha\r\nbeta\r\ngamma\r\n")
        .await
        .expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "search": "alpha\nbeta\n",
        "replace": "one\ntwo\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\ntwo\ngamma\r\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn tabs_and_spaces_mismatch_recovers() {
    let path = temp_path("tabs-spaces.txt");
    fs::write(&path, "fn main() {\n\t\tprintln!(\"hi\");\n}\n")
        .await
        .expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "search": "  println!(\"hi\");\n",
        "replace": "  println!(\"bye\");\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "fn main() {\n  println!(\"bye\");\n}\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn fuzzy_recovery_applies_for_high_confidence_block() {
    let path = temp_path("fuzzy-high.txt");
    fs::write(
        &path,
        "fn score(input: i32) -> i32 {\n    let adjusted = input + 1;\n    adjusted * 2\n}\n",
    )
    .await
    .expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "search": "fn score(input: i32) -> i32 {\n    let adjusted = input + 2;\n    adjusted * 2\n}\n",
        "replace": "fn score(input: i32) -> i32 {\n    input * 3\n}\n"
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
async fn fuzzy_recovery_rejects_low_confidence_block() {
    let path = temp_path("fuzzy-low.txt");
    let original = "fn alpha() {\n    println!(\"alpha\");\n    println!(\"done\");\n}\n";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "path": path,
            "search": "struct Missing {\n    value: usize,\n    label: String,\n}\n",
            "replace": "replacement\n"
        }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not confident enough"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn fuzzy_recovery_rejects_close_candidates() {
    let path = temp_path("fuzzy-close.txt");
    let original = "fn first() {\n    let value = compute_total(10);\n    println!(\"{}\", value);\n}\n\nfn second() {\n    let value = compute_total(11);\n    println!(\"{}\", value);\n}\n";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "path": path,
            "search": "fn third() {\n    let value = compute_total(12);\n    println!(\"{}\", value);\n}\n",
            "replace": "replacement\n"
        }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not confident enough"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn exact_duplicates_do_not_attempt_fuzzy_recovery() {
    let path = temp_path("exact-dup-no-fuzzy.txt");
    let original = "target block\ntarget block\n";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "path": path, "search": "target block", "replace": "changed" }))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("matched 2 times"));
    assert!(!err.contains("not confident enough"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn multi_edit_failure_is_atomic_after_recovered_edit() {
    let path = temp_path("multi-recover-atomic.txt");
    let original = "fn main() {\n    let value = 1;   \n}\n";
    fs::write(&path, original).await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "path": path,
            "edits": [
                { "search": "    let value = 1;\n", "replace": "    let value = 2;\n" },
                { "search": "definitely missing", "replace": "x" }
            ]
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn preview_uses_same_recovery_logic_as_execute() {
    let path = temp_path("preview-recover.txt");
    fs::write(&path, "fn main() {\n    let value = 1;   \n}\n")
        .await
        .expect("setup");
    let args = json!({
        "path": path,
        "search": "    let value = 1;\n",
        "replace": "    let value = 2;\n"
    });

    let preview = preview_edit_file("edit_file", args.clone())
        .await
        .expect("preview");
    assert!(preview.diff.contains("let value = 2;"));

    let tool = EditFileTool;
    tool.execute(args).await.expect("success");
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "fn main() {\n    let value = 2;\n}\n"
    );
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

#[tokio::test]
async fn replace_all_top_level_replaces_every_occurrence() {
    let path = temp_path("replace-all-top.txt");
    fs::write(&path, "foo bar foo baz foo")
        .await
        .expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({ "path": path, "search": "foo", "replace": "qux", "replace_all": true }))
        .await
        .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "qux bar qux baz qux"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn replace_all_in_edits_array_replaces_every_occurrence() {
    let path = temp_path("replace-all-edits.txt");
    fs::write(&path, "alpha beta alpha gamma alpha")
        .await
        .expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "edits": [
            { "search": "alpha", "replace": "one", "replace_all": true }
        ]
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one beta one gamma one"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn replace_all_errors_on_zero_matches() {
    let path = temp_path("replace-all-zero.txt");
    fs::write(&path, "nothing here").await.expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "path": path, "search": "ghost", "replace": "x", "replace_all": true }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("matched 0 times"));
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn missing_trailing_newline_with_whitespace_mismatch_recovers() {
    // The search omits the trailing newline AND uses spaces where the file
    // has a tab: exact match fails, so the normalized path must handle the
    // newline asymmetry between the needle and whole-line windows — and the
    // replacement must not swallow the line's newline.
    let path = temp_path("no-trailing-newline.txt");
    fs::write(&path, "fn main() {\n\tlet value = 1;\n}\n")
        .await
        .expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "search": "    let value = 1;",
        "replace": "    let value = 2;"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "fn main() {\n    let value = 2;\n}\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn dropped_blank_line_recovers() {
    // The model's search omits a blank line present in the file; window
    // sizes of needle_lines±1 let fuzzy matching find the real block.
    let path = temp_path("dropped-blank-line.txt");
    fs::write(
        &path,
        "fn compute() {\n    let alpha = first_value();\n\n    let beta = second_value();\n}\n",
    )
    .await
    .expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "search": "fn compute() {\n    let alpha = first_value();\n    let beta = second_value();\n}\n",
        "replace": "fn compute() {\n    combined_values()\n}\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "fn compute() {\n    combined_values()\n}\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn unicode_quote_mismatch_recovers() {
    // File contains a curly apostrophe; the model reproduces it as a straight
    // quote. Normalization folds typographic characters.
    let path = temp_path("unicode-quote.txt");
    fs::write(
        &path,
        "// it\u{2019}s the \u{201C}main\u{201D} entry\nfn main() {}\n",
    )
    .await
    .expect("setup");
    let tool = EditFileTool;

    tool.execute(json!({
        "path": path,
        "search": "// it's the \"main\" entry\n",
        "replace": "// entry point\n"
    }))
    .await
    .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "// entry point\nfn main() {}\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn ambiguous_error_includes_match_snippets() {
    let path = temp_path("ambiguous-snippets.txt");
    fs::write(&path, "let x = total;\nother line\nlet x = total;\n")
        .await
        .expect("setup");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "path": path, "search": "let x = total;", "replace": "let x = sum;" }))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("line 1: let x = total;"),
        "missing snippet: {err}"
    );
    assert!(
        err.contains("line 3: let x = total;"),
        "missing snippet: {err}"
    );
    assert!(err.contains("surrounding lines"));
    let _ = fs::remove_file(&path).await;
}
