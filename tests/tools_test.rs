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
fn seed_default_tools_migrates_v1_python_task_list_to_v2_bash() {
    let dir = temp_dir("task-list-migration");
    let path = dir.join("task_list.yaml");
    // Simulate a v1 (Python-based) task_list.yaml
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
    // v2 uses bash, not python
    assert!(!updated.contains("python3"));
    // v2 uses line_envelope, not json_envelope
    assert!(updated.contains("line_envelope"));
    assert!(!updated.contains("json_envelope"));
    // v2 uses bash set -euo pipefail
    assert!(updated.contains("set -euo pipefail"));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn seed_default_tools_does_not_overwrite_v2_task_list() {
    let dir = temp_dir("task-list-no-migrate");
    let path = dir.join("task_list.yaml");
    // Simulate a v2 (bash-based) task_list.yaml
    fs::write(
        &path,
        r#"name: task_list
version: 2
description: my custom task list
script: |
  echo "custom"
"#,
    )
    .unwrap();

    seed_default_tools(&dir);

    let content = fs::read_to_string(&path).unwrap();
    // Should not be overwritten since it's already v2 (no python3, no json_envelope)
    assert!(content.contains("my custom task list"));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn seed_default_tools_detects_wrong_os_variant() {
    let dir = temp_dir("task-list-wrong-os");
    let path = dir.join("task_list.yaml");
    // Simulate a PowerShell variant on a Unix system (wrong OS)
    fs::write(
        &path,
        r#"name: task_list
version: 2
output:
  kind: line_envelope
script: |
  $ErrorActionPreference = 'Stop'
  Write-Output 'hello'
"#,
    )
    .unwrap();

    seed_default_tools(&dir);

    let updated = fs::read_to_string(&path).unwrap();
    // On Unix, should have been migrated to the bash variant
    assert!(
        updated.contains("set -euo pipefail"),
        "expected bash variant after wrong-OS migration, got: {updated}"
    );
    assert!(!updated.contains("$ErrorActionPreference"));

    let _ = fs::remove_dir_all(dir);
}
