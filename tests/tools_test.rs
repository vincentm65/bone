use bone::tools::seed_default_tools;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("bone-tools-mod-{label}-{suffix}"));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn seed_default_tools_migrates_versionless_task_list() {
    let dir = temp_dir("task-list-migration");
    let path = dir.join("task_list.yaml");
    fs::write(
        &path,
        r#"name: task_list
description: old task list
output:
  kind: json_envelope
script: |
  python3 - <<'PY'
  print("hello")
  PY
"#,
    )
    .unwrap();

    seed_default_tools(&dir);

    let updated = fs::read_to_string(&path).unwrap();
    assert!(updated.contains("uv run"));
    assert!(updated.contains("python3"));
    assert!(updated.contains("output:"));
    assert!(updated.contains("json_envelope"));
    assert!(!updated.contains("line_envelope"));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn seed_default_tools_does_not_overwrite_v10_task_list() {
    let dir = temp_dir("task-list-no-migrate");
    let path = dir.join("task_list.yaml");
    fs::write(
        &path,
        r#"name: task_list
version: 12
description: my custom task list
script: |
  echo "custom"
"#,
    )
    .unwrap();

    seed_default_tools(&dir);

    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("my custom task list"));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn seed_default_tools_writes_new_file_for_older_versioned_task_list() {
    let dir = temp_dir("task-list-v3-migration");
    let path = dir.join("task_list.yaml");
    fs::write(
        &path,
        r#"name: task_list
version: 3
output:
  kind: line_envelope
script: |
  uv run -- python3 <<'PYEOF'
  print("hello")
  PYEOF
"#,
    )
    .unwrap();

    seed_default_tools(&dir);

    let original = fs::read_to_string(&path).unwrap();
    assert!(original.contains("version: 3"));
    assert!(original.contains("<<'PYEOF'"));

    let candidate = fs::read_to_string(dir.join("task_list.yaml.new")).unwrap();
    assert!(
        candidate.contains("version: 12"),
        "expected v12 candidate, got: {candidate}"
    );
    assert!(
        candidate.contains("python3 -c"),
        "expected python3 -c syntax, got: {candidate}"
    );
    assert!(!candidate.contains("<<'PYEOF'"));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn seed_default_tools_seeds_task_list_when_absent() {
    let dir = temp_dir("task-list-seed");

    seed_default_tools(&dir);

    let content = fs::read_to_string(dir.join("task_list.yaml")).unwrap();
    assert!(content.contains("version: 12"));
    assert!(content.contains("python3 -c"));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn seed_default_tools_seeds_web_search_when_absent() {
    let dir = temp_dir("web-search-seed");

    seed_default_tools(&dir);

    let content = fs::read_to_string(dir.join("web_search.yaml")).unwrap();
    assert!(content.contains("ddgs"));

    let _ = fs::remove_dir_all(dir);
}
