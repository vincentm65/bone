mod common;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bone_core::tools::read_file::ReadFileTool;
use bone_core::tools::snapshot::SnapshotStore;
use bone_core::tools::types::{Tool, ToolExecutionContext};
use serde_json::json;
use tokio::fs;

fn temp_path(name: &str) -> PathBuf {
    common::temp_path(&format!("simple-read-{name}"))
}

fn live_context() -> ToolExecutionContext {
    let mut context = ToolExecutionContext::default();
    context.snapshots = Arc::new(RwLock::new(SnapshotStore::default()));
    context
}

#[tokio::test]
async fn returns_resolved_path_range_and_numbered_text() {
    let path = temp_path("full.txt");
    fs::write(&path, "line one\nline two\nline three")
        .await
        .unwrap();
    let result = ReadFileTool.execute(json!({ "path": path })).await.unwrap();
    let canonical = fs::canonicalize(&path).await.unwrap();
    assert!(result.starts_with(&format!("File: {}", canonical.display())));
    assert!(result.contains("Range: lines 1-3 of 3; entire file."));
    assert!(result.contains("    1 | line one"));
    assert!(result.contains("    3 | line three"));
    assert!(!result.contains('#'));
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn range_is_clear_and_provides_the_next_call() {
    let path = temp_path("range.txt");
    fs::write(&path, "a\nb\nc\nd").await.unwrap();
    let result = ReadFileTool
        .execute(json!({ "path": path, "start_line": 2, "max_lines": 2 }))
        .await
        .unwrap();
    assert!(result.contains("Range: lines 2-3 of 4"));
    assert!(result.contains("start_line=4"));
    assert!(result.contains("    2 | b"));
    assert!(!result.contains("    4 | d"));
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn explicit_modes_reject_ambiguous_combinations() {
    let single_error = ReadFileTool
        .execute(json!({
            "path": "src/**/*.rs",
            "mode": "single",
        }))
        .await
        .unwrap_err();
    assert!(single_error.contains("mode=single requires one literal file path"));

    let bulk_error = ReadFileTool
        .execute(json!({
            "path": "src/**/*.rs",
            "mode": "bulk",
            "max_lines": 20,
        }))
        .await
        .unwrap_err();
    assert!(bulk_error.contains("mode=bulk cannot use start_line or max_lines"));
}

#[tokio::test]
async fn start_beyond_eof_and_empty_files_are_explicit() {
    let path = temp_path("empty-range.txt");
    fs::write(&path, "only").await.unwrap();
    let beyond = ReadFileTool
        .execute(json!({ "path": path, "start_line": 99 }))
        .await
        .unwrap();
    assert!(beyond.contains("Range: no lines; file has 1 line"));
    fs::write(&path, "").await.unwrap();
    let empty = ReadFileTool.execute(json!({ "path": path })).await.unwrap();
    assert!(empty.contains("Range: empty file; 0 lines total"));
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn long_lines_are_bounded_and_marked_uneditable() {
    let path = temp_path("long.txt");
    fs::write(&path, "x".repeat(5000)).await.unwrap();
    let result = ReadFileTool.execute(json!({ "path": path })).await.unwrap();
    assert!(result.contains("…[truncated]"));
    assert!(result.contains("not editable"));
    assert!(result.len() < 5000);
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn output_is_bounded_between_complete_lines() {
    let path = temp_path("bounded.txt");
    let content = (1..=1000)
        .map(|n| format!("{n:04} {}", "x".repeat(100)))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, content).await.unwrap();
    let result = ReadFileTool.execute(json!({ "path": path })).await.unwrap();
    assert!(result.len() <= 50 * 1024, "{} bytes", result.len());
    assert!(result.contains("Stopped: 50 KiB output limit."));
    assert!(result.contains("Continue: read_file(path="));
    assert!(!result.ends_with("…[truncated]"));
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn image_files_still_return_attachments() {
    let dir = common::temp_dir("simple-read-image");
    fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join("image.png");
    fs::write(&path, [137, 80, 78, 71]).await.unwrap();
    let result = ReadFileTool
        .execute_output(json!({ "path": path }))
        .await
        .unwrap();
    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].media_type, "image/png");
    let _ = fs::remove_dir_all(dir).await;
}

#[tokio::test]
async fn invalid_utf8_gets_an_actionable_error() {
    let path = temp_path("binary.bin");
    fs::write(&path, [0xff, 0xfe]).await.unwrap();
    let error = ReadFileTool
        .execute(json!({ "path": path }))
        .await
        .unwrap_err();
    assert!(error.contains("not valid UTF-8"));
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn preserves_trailing_whitespace_on_the_last_displayed_line() {
    let path = temp_path("trailing-whitespace.txt");
    fs::write(&path, "first\nlast  \t\n").await.unwrap();
    let result = ReadFileTool.execute(json!({ "path": path })).await.unwrap();
    assert!(result.ends_with("    2 | last  \t"), "{result:?}");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn reports_a_poisoned_snapshot_store() {
    let path = temp_path("poisoned-snapshot.txt");
    fs::write(&path, "content\n").await.unwrap();
    let context = ToolExecutionContext::default();
    let snapshots = context.snapshots.clone();
    let _ = std::thread::spawn(move || {
        let _guard = snapshots.write().unwrap();
        panic!("poison snapshot store for test");
    })
    .join();

    let error = ReadFileTool
        .execute_output_live(json!({ "path": path }), None, context)
        .await
        .unwrap_err();
    assert!(error.contains("snapshot store lock is poisoned"), "{error}");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn directories_are_rejected_before_reading() {
    let dir = common::temp_dir("simple-read-directory");
    fs::create_dir_all(&dir).await.unwrap();
    let error = ReadFileTool
        .execute(json!({ "path": dir }))
        .await
        .unwrap_err();
    assert!(error.contains("directory"), "{error}");
    assert!(error.contains("not a regular file"), "{error}");
    let _ = fs::remove_dir_all(dir).await;
}

#[cfg(unix)]
#[tokio::test]
async fn device_and_stream_paths_are_rejected_promptly() {
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        ReadFileTool.execute(json!({ "path": "/dev/zero" })),
    )
    .await
    .expect("device read must not hang")
    .unwrap_err();
    assert!(error.contains("character device"), "{error}");
    assert!(error.contains("refusing to read"), "{error}");

    let error = ReadFileTool
        .execute(json!({ "path": "/dev/stdin" }))
        .await
        .unwrap_err();
    assert!(error.contains("protected device path"), "{error}");
}

#[tokio::test]
async fn repairs_invisible_filename_spelling_and_suggests_typos() {
    let dir = common::temp_dir("simple-read-repair");
    fs::create_dir_all(&dir).await.unwrap();
    let actual = dir.join("Screenshot 3.04\u{202f}PM.txt");
    fs::write(&actual, "repaired").await.unwrap();

    let requested = dir.join("Screenshot 3.04 PM.txt");
    let result = ReadFileTool
        .execute(json!({ "path": requested }))
        .await
        .unwrap();
    assert!(result.contains("repaired"), "{result}");
    assert!(
        result.contains(actual.to_string_lossy().as_ref()),
        "{result}"
    );

    let agents = dir.join("AGENTS.md");
    fs::write(&agents, "agents").await.unwrap();
    let error = ReadFileTool
        .execute(json!({ "path": dir.join("AGENT.md") }))
        .await
        .unwrap_err();
    assert!(error.contains("Did you mean: AGENTS.md?"), "{error}");

    let error = ReadFileTool
        .execute(json!({ "path": dir.join("genuinely-missing.rs") }))
        .await
        .unwrap_err();
    assert!(!error.contains("Did you mean:"), "{error}");
    let _ = fs::remove_dir_all(dir).await;
}

#[tokio::test]
async fn repaired_relative_paths_cannot_escape_working_directory() {
    let root = common::temp_dir("simple-read-repair-boundary");
    fs::create_dir_all(&root).await.unwrap();
    let outside = root
        .parent()
        .unwrap()
        .join("repair-boundary-outside-\u{202f}file.txt");
    fs::write(&outside, "outside").await.unwrap();

    let error = bone_core::tools::snapshot::resolve_existing_path(
        "../repair-boundary-outside- file.txt",
        Some(&root),
    )
    .await
    .unwrap_err();
    assert!(error.contains("could not resolve"), "{error}");
    let _ = fs::remove_file(outside).await;
    let _ = fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn bulk_glob_reads_honor_gitignore_hidden_and_exclude() {
    let root = common::temp_dir("simple-read-bulk-tree");
    fs::create_dir_all(root.join("src")).await.unwrap();
    fs::create_dir_all(root.join(".git")).await.unwrap();
    fs::write(root.join(".gitignore"), "src/ignored.rs\n")
        .await
        .unwrap();
    fs::write(root.join("src/kept.rs"), "kept").await.unwrap();
    fs::write(root.join("src/excluded.rs"), "excluded")
        .await
        .unwrap();
    fs::write(root.join("src/ignored.rs"), "ignored")
        .await
        .unwrap();
    fs::write(root.join("src/.hidden.rs"), "hidden")
        .await
        .unwrap();
    fs::write(root.join(".git/inside.rs"), "git").await.unwrap();
    fs::write(root.join("src/extra.txt"), "extra")
        .await
        .unwrap();

    let context = live_context().with_working_dir(root.clone());
    let output = ReadFileTool
        .execute_output_live(
            json!({
                "path": "src/*.rs",
                "paths": ["src/extra.txt"],
                "exclude": ["**/excluded.rs"]
            }),
            None,
            context,
        )
        .await
        .unwrap();
    assert!(output.content.contains("kept"), "{}", output.content);
    assert!(output.content.contains("extra"), "{}", output.content);
    assert!(!output.content.contains("excluded"), "{}", output.content);
    assert!(!output.content.contains("ignored"), "{}", output.content);
    assert!(!output.content.contains("hidden"), "{}", output.content);
    assert!(!output.content.contains("git"), "{}", output.content);
    assert!(output.content.contains("returned 2 of 2 matched files"));
    let _ = fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn bulk_glob_star_stays_within_one_directory() {
    let root = common::temp_dir("simple-read-bulk-star-flat");
    fs::create_dir_all(root.join("src/nested")).await.unwrap();
    fs::write(root.join("src/top.rs"), "top").await.unwrap();
    fs::write(root.join("src/nested/deep.rs"), "deep")
        .await
        .unwrap();

    let context = live_context().with_working_dir(root.clone());
    let flat = ReadFileTool
        .execute_output_live(json!({ "path": "src/*.rs" }), None, context.clone())
        .await
        .unwrap();
    assert!(flat.content.contains("top"), "{}", flat.content);
    assert!(!flat.content.contains("deep"), "{}", flat.content);
    assert!(
        flat.content.contains("returned 1 of 1 matched files"),
        "{}",
        flat.content
    );

    let recursive = ReadFileTool
        .execute_output_live(json!({ "path": "src/**/*.rs" }), None, context)
        .await
        .unwrap();
    assert!(recursive.content.contains("top"), "{}", recursive.content);
    assert!(recursive.content.contains("deep"), "{}", recursive.content);
    let _ = fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn bulk_reads_reject_line_ranges_and_preserve_image_attachments() {
    let root = common::temp_dir("simple-read-bulk-image");
    fs::create_dir_all(&root).await.unwrap();
    fs::write(root.join("one.rs"), "one\ntwo").await.unwrap();
    fs::write(root.join("image.png"), [137, 80, 78, 71])
        .await
        .unwrap();
    let tool = ReadFileTool;
    let context = live_context().with_working_dir(root.clone());

    let error = tool
        .execute_output_live(
            json!({ "path": "*.rs", "max_lines": 1 }),
            None,
            context.clone(),
        )
        .await
        .unwrap_err();
    assert!(error.contains("Bulk read detected"), "{error}");

    let output = tool
        .execute_output_live(json!({ "path": "*" }), None, context)
        .await
        .unwrap();
    assert_eq!(output.images.len(), 1);
    assert_eq!(output.images[0].media_type, "image/png");
    assert!(output.content.contains("one"), "{}", output.content);
    let _ = fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn bulk_reads_cap_output_and_match_count() {
    let root = common::temp_dir("simple-read-bulk-cap");
    fs::create_dir_all(&root).await.unwrap();
    let content = (0..300)
        .map(|n| format!("{n:03} {}", "x".repeat(300)))
        .collect::<Vec<_>>()
        .join("\n");
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(root.join(name), &content).await.unwrap();
    }

    let context = live_context().with_working_dir(root.clone());
    let output = ReadFileTool
        .execute_output_live(json!({ "path": "*.txt" }), None, context)
        .await
        .unwrap();
    assert!(
        output.content.len() <= 100 * 1024,
        "{} bytes",
        output.content.len()
    );
    assert!(output.content.contains("of 3 matched files"));
    assert!(
        output.content.contains("returned 2 of 3 matched files"),
        "{}",
        output.content
    );
    assert!(output.content.contains("skipped 1"), "{}", output.content);
    let _ = fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn bulk_reads_cap_file_count() {
    let root = common::temp_dir("simple-read-bulk-file-cap");
    fs::create_dir_all(&root).await.unwrap();
    for n in 0..55 {
        fs::write(root.join(format!("{n:02}.txt")), format!("file {n}"))
            .await
            .unwrap();
    }

    let context = live_context().with_working_dir(root.clone());
    let output = ReadFileTool
        .execute_output_live(json!({ "path": "*.txt" }), None, context)
        .await
        .unwrap();
    assert!(output.content.contains("returned 50 of 55 matched files"));
    assert!(output.content.contains("skipped 5"));
    let _ = fs::remove_dir_all(root).await;
}

#[test]
fn schema_is_small_and_bounded() {
    let schema = ReadFileTool.definition().input_schema;
    assert_eq!(schema["required"], json!(["path"]));
    assert_eq!(
        schema["properties"]["mode"]["enum"],
        json!(["single", "bulk"])
    );
    assert_eq!(schema["properties"]["max_lines"]["maximum"], 1000);
    assert_eq!(schema["properties"]["path"]["type"], "string");
    assert_eq!(schema["properties"]["paths"]["type"], "array");
    assert_eq!(schema["properties"]["exclude"]["type"], "array");
    assert_eq!(schema["additionalProperties"], false);
}
